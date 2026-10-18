//! Agent runs (docs/13 §5–7): a job that `uses: agents.execute` runs in a runtime module
//! (`agents.execute@1`); the agent works with memory through Frood's MCP with a run token,
//! may ask people and wait, and is resumed in the same session.
//!
//! Contract `agents.execute@1`, one capability endpoint with an `op`:
//! - `start` / `resume`: the run spec (agent, model, instructions, MCP servers, credentials,
//!   budget) and, for `resume`, the session and the message to continue with →
//!   `{ session_ref, state }`;
//! - `send`: a message to a running agent → `{ delivered }`;
//! - `stop`: end the run;
//! - `status` → `{ state: running | finished | failed, session_ref, cost_usd, result, error }`;
//! - `events` after `n` → `{ events: [{ kind, data }] }`, numbered from 1.

use std::time::Duration;

use chrono::Utc;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::jobs::{AgentSettings, JobError, JobSpec};
use crate::questions::{self, NewQuestion, Question};
use crate::runs::{self, Run};
use crate::{Ctx, Dispatch};

/// Run tokens live until the run ends; a run may wait days for an answer.
const RUN_TOKEN_TTL: Duration = Duration::from_secs(14 * 86_400);

fn capability_input(spec: &JobSpec, settings: &AgentSettings, run: &Run) -> Value {
    json!({
        "run_id": run.id,
        "job": spec.name,
        "agent": settings.agent,
        "model": settings.model,
        "instructions": settings.instructions,
        "budget": { "max_usd": settings.budget.max_usd, "max_minutes": settings.budget.max_minutes },
        "ask_user": settings.ask_user.allowed,
    })
}

/// The first message of a run: what it was started for.
fn first_prompt(spec: &JobSpec, run: &Run) -> String {
    let input = serde_json::to_string_pretty(&run.input).unwrap_or_default();
    format!(
        "You are running the Frood job `{}` (run {}). Your instructions are in the system prompt. \
         Work with Frood through its MCP tools. When you are done, give a short report of what you \
         did and which proposals you made.\n\nWhat started this run:\n{input}",
        spec.name, run.id
    )
}

impl Ctx {
    /// The MCP servers the agent gets, with a fresh run token for Frood.
    async fn mcp_servers(
        &self,
        spec: &JobSpec,
        settings: &AgentSettings,
        run: &Run,
        on_behalf_of: Option<Uuid>,
    ) -> Result<Value, JobError> {
        self.auth
            .revoke_run_tokens(run.id)
            .await
            .map_err(|e| JobError::transient(e.to_string()))?;
        let token = self
            .auth
            .issue_run_token(
                run.id,
                &spec.name,
                on_behalf_of,
                &settings.permissions,
                RUN_TOKEN_TTL,
            )
            .await
            .map_err(|e| JobError::transient(e.to_string()))?;
        let servers = vec![json!({
            "name": "frood",
            "path": "/mcp",
            "headers": { "Authorization": format!("Bearer {}", token.token) },
        })];
        for tool in settings.tools.iter().filter(|t| *t != "frood") {
            tracing::warn!(job = %spec.name, tool, "MCP sources are not wired yet; the agent runs without it");
        }
        Ok(Value::Array(servers))
    }

    fn credentials(&self, settings: &AgentSettings) -> Result<Value, JobError> {
        let credential = self
            .gateway
            .credential(&settings.account)
            .ok_or_else(|| JobError::permanent(format!("no account {}", settings.account)))?;
        Ok(json!({
            "provider": match credential.provider {
                frood_config::ModelProvider::Anthropic => "anthropic",
                frood_config::ModelProvider::Openai => "openai",
            },
            "api_key": credential.key,
            "workspace": credential.workspace,
        }))
    }
}

/// Starts an agent run in its runtime, or resumes it after its questions were settled.
pub(crate) async fn start_or_resume(
    ctx: &Ctx,
    spec: &JobSpec,
    settings: &AgentSettings,
    run: &Run,
) -> Result<(), JobError> {
    let on_behalf_of = match (&run.on_behalf_of, &settings.on_behalf_of) {
        (Some(id), _) => Some(*id),
        (None, Some(email)) => Some(
            ctx.auth
                .user_id_by_email(email, false)
                .await
                .map_err(|e| JobError::permanent(format!("on_behalf_of {email}: {e}")))?,
        ),
        (None, None) => None,
    };
    let mut body = capability_input(spec, settings, run);
    body["mcp_servers"] = ctx.mcp_servers(spec, settings, run, on_behalf_of).await?;
    body["credentials"] = ctx.credentials(settings)?;
    let resuming = run.runtime.is_some() && run.session_ref.is_some();
    if resuming {
        let qs = questions::for_run(&ctx.pool, run.id).await?;
        let mut message = questions::resume_message(&qs);
        for pending in run.pending_messages.as_array().into_iter().flatten() {
            if let Some(text) = pending.as_str() {
                message.push_str(&format!("\n\nMessage from a person: {text}"));
            }
        }
        body["op"] = json!("resume");
        body["session_ref"] = json!(run.session_ref);
        body["message"] = json!(message);
    } else {
        body["op"] = json!("start");
        body["prompt"] = json!(first_prompt(spec, run));
    }
    let out = ctx
        .modules
        .call(&settings.runtime, "agents.execute", &body)
        .await?;
    let session = out.get("session_ref").and_then(Value::as_str);
    runs::set_started(&ctx.pool, run.id, &settings.runtime, session, on_behalf_of).await?;
    runs::add_event(
        &ctx.pool,
        run.id,
        if resuming { "resumed" } else { "started" },
        &json!({ "runtime": settings.runtime, "agent": settings.agent, "model": settings.model, "session_ref": session }),
    )
    .await?;
    Ok(())
}

impl Dispatch {
    fn agent_settings(&self, job: &str) -> Option<(&JobSpec, &AgentSettings)> {
        let spec = self.jobs.iter().find(|j| j.name == job)?;
        spec.agent.as_ref().map(|a| (spec, a))
    }

    async fn finish(&self, run: &Run, status: &str, output: Option<&Value>, error: Option<&str>) {
        let pool = &self.ctx.pool;
        let result = match status {
            "succeeded" => runs::succeed(pool, run.id, output.unwrap_or(&Value::Null)).await,
            "cancelled" => runs::cancel(pool, run.id, error.unwrap_or("cancelled")).await,
            _ => runs::fail(pool, run.id, error.unwrap_or("failed"), None).await,
        };
        if let Err(err) = result {
            tracing::error!(run = %run.id, error = %err, "cannot record the end of a run");
        }
        let _ = self.ctx.auth.revoke_run_tokens(run.id).await;
        for q in questions::for_run(pool, run.id).await.unwrap_or_default() {
            if q.status == "open" {
                let _ = questions::set_status(pool, q.id, "cancelled").await;
            }
        }
        let event = match status {
            "succeeded" => "run.succeeded",
            "cancelled" => "run.cancelled",
            _ => "run.failed",
        };
        let _ = self
            .ctx
            .ledger
            .append_event(
                &format!("agent:{}", run.job),
                event,
                format!("run/{}", run.id),
                json!({ "run_id": run.id, "job": run.job, "error": error, "cost_usd": run.cost_usd }),
            )
            .await;
    }

    async fn runtime_call(&self, runtime: &str, body: Value) -> Option<Value> {
        match self
            .ctx
            .modules
            .call(runtime, "agents.execute", &body)
            .await
        {
            Ok(out) => Some(out),
            Err(err) => {
                tracing::warn!(runtime, error = %err, "runtime call failed");
                None
            }
        }
    }

    /// One pass over agent runs: progress, budgets, ends, settled questions, expired questions.
    pub async fn watch_once(&self) -> sqlx::Result<()> {
        for (question, default) in questions::overdue(&self.ctx.pool).await? {
            self.expire(&question, default.as_deref()).await?;
        }
        for run in runs::active(&self.ctx.pool).await? {
            match run.status.as_str() {
                "running" => self.check_running(&run).await?,
                "waiting_for_input" => {
                    let qs = questions::for_run(&self.ctx.pool, run.id).await?;
                    if !qs.is_empty() && qs.iter().all(|q| q.status != "open") {
                        runs::requeue_for_resume(&self.ctx.pool, run.id).await?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    async fn check_running(&self, run: &Run) -> sqlx::Result<()> {
        let Some(runtime) = run.runtime.clone() else {
            return Ok(());
        };
        let Some(status) = self
            .runtime_call(&runtime, json!({ "op": "status", "run_id": run.id }))
            .await
        else {
            return Ok(());
        };
        let seen = runs::runtime_events_seen(&self.ctx.pool, run.id).await?;
        if let Some(out) = self
            .runtime_call(
                &runtime,
                json!({ "op": "events", "run_id": run.id, "after": seen }),
            )
            .await
        {
            for event in out
                .get("events")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let kind = event.get("kind").and_then(Value::as_str).unwrap_or("event");
                let data = event.get("data").cloned().unwrap_or(Value::Null);
                runs::add_event(&self.ctx.pool, run.id, &format!("runtime.{kind}"), &data).await?;
            }
        }
        let cost = status
            .get("cost_usd")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let session = status.get("session_ref").and_then(Value::as_str);
        runs::set_progress(&self.ctx.pool, run.id, cost, session).await?;

        let budget = self
            .agent_settings(&run.job)
            .map(|(_, a)| a.budget.clone())
            .unwrap_or_default();
        let minutes = run
            .started_at
            .map(|t| (Utc::now() - t).num_seconds() as f64 / 60.0)
            .unwrap_or(0.0);
        let over = match (budget.max_usd, budget.max_minutes) {
            (Some(max), _) if cost > max => Some(format!("budget exceeded: ${cost:.4} of ${max}")),
            (_, Some(max)) if minutes > f64::from(max) => Some(format!(
                "time budget exceeded: {minutes:.0} of {max} minutes"
            )),
            _ => None,
        };
        if let Some(reason) = over {
            self.runtime_call(&runtime, json!({ "op": "stop", "run_id": run.id }))
                .await;
            self.finish(run, "failed", None, Some(&reason)).await;
            return Ok(());
        }

        match status.get("state").and_then(Value::as_str) {
            Some("finished") => {
                let open = questions::for_run(&self.ctx.pool, run.id)
                    .await?
                    .iter()
                    .any(|q| q.status == "open");
                if open {
                    runs::set_waiting(&self.ctx.pool, run.id).await?;
                    runs::add_event(&self.ctx.pool, run.id, "waiting", &json!({})).await?;
                } else {
                    let output = json!({ "result": status.get("result"), "cost_usd": cost });
                    self.finish(run, "succeeded", Some(&output), None).await;
                }
            }
            Some("failed") => {
                let error = status
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("the agent failed");
                self.finish(run, "failed", None, Some(error)).await;
            }
            _ => {}
        }
        Ok(())
    }

    async fn expire(&self, question: &Question, default: Option<&str>) -> sqlx::Result<()> {
        match question.on_expire.as_str() {
            "default_answer" => {
                questions::answer(
                    &self.ctx.pool,
                    question.id,
                    default.unwrap_or(""),
                    "system:expiry",
                )
                .await?;
            }
            "cancel" => {
                questions::set_status(&self.ctx.pool, question.id, "expired").await?;
                if let Some(run) = runs::get(&self.ctx.pool, question.run_id).await? {
                    if let Some(runtime) = &run.runtime {
                        self.runtime_call(runtime, json!({ "op": "stop", "run_id": run.id }))
                            .await;
                    }
                    self.finish(&run, "cancelled", None, Some("a question expired"))
                        .await;
                }
            }
            _ => questions::set_status(&self.ctx.pool, question.id, "expired").await?,
        }
        Ok(())
    }

    /// `ask_user` from an agent's run: the question waits for a person; when the agent ends
    /// its turn the run waits too.
    pub async fn ask_user(
        &self,
        run_id: Uuid,
        text: &str,
        options: &[String],
        context: Option<&str>,
    ) -> Result<Question, String> {
        let run = runs::get(&self.ctx.pool, run_id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("the run is not known")?;
        let (_, settings) = self
            .agent_settings(&run.job)
            .ok_or("this job is not an agent job")?;
        if !settings.ask_user.allowed {
            return Err("this job may not ask people; decide yourself or report the doubt".into());
        }
        let expires_at = settings
            .ask_user
            .expire_after
            .as_deref()
            .and_then(|d| humantime::parse_duration(d).ok())
            .and_then(|d| chrono::Duration::from_std(d).ok())
            .map(|d| Utc::now() + d);
        let question = questions::ask(
            &self.ctx.pool,
            NewQuestion {
                run_id,
                job: &run.job,
                asked_for: run.on_behalf_of,
                text,
                options,
                context,
                expires_at,
                on_expire: settings.ask_user.on_expire.as_str(),
                default_answer: settings.ask_user.default_answer.as_deref(),
            },
        )
        .await
        .map_err(|e| e.to_string())?;
        let _ = self
            .ctx
            .ledger
            .append_event(
                &format!("agent:{}", run.job),
                "question.asked",
                format!("question/{}", question.id),
                json!({ "question_id": question.id, "run_id": run_id, "job": run.job, "asked_for": run.on_behalf_of }),
            )
            .await;
        Ok(question)
    }

    /// A person's answer; the watcher resumes the run once all its questions are settled.
    pub async fn answer_question(
        &self,
        id: Uuid,
        answer: &str,
        actor: &str,
    ) -> Result<Question, String> {
        let question = questions::answer(&self.ctx.pool, id, answer, actor)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("the question is not open")?;
        let _ = self
            .ctx
            .ledger
            .append_event(
                actor,
                "question.answered",
                format!("question/{id}"),
                json!({ "question_id": id, "run_id": question.run_id }),
            )
            .await;
        Ok(question)
    }

    pub async fn questions(&self, status: Option<&str>, limit: i64) -> sqlx::Result<Vec<Question>> {
        questions::list(&self.ctx.pool, status, limit).await
    }

    pub async fn question(&self, id: Uuid) -> sqlx::Result<Option<Question>> {
        questions::get(&self.ctx.pool, id).await
    }

    /// `report_progress` from an agent.
    pub async fn report_progress(&self, run_id: Uuid, message: &str) -> sqlx::Result<()> {
        runs::add_event(
            &self.ctx.pool,
            run_id,
            "progress",
            &json!({ "message": message }),
        )
        .await
    }

    /// A person's message to a run: delivered now if the runtime can, else on resume.
    pub async fn send_message(&self, run_id: Uuid, text: &str) -> Result<&'static str, String> {
        let run = runs::get(&self.ctx.pool, run_id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("the run is not known")?;
        match run.status.as_str() {
            "running" => {
                if let Some(runtime) = &run.runtime
                    && let Some(out) = self
                        .runtime_call(
                            runtime,
                            json!({ "op": "send", "run_id": run_id, "message": text }),
                        )
                        .await
                    && out.get("delivered").and_then(Value::as_bool) == Some(true)
                {
                    let _ = runs::add_event(
                        &self.ctx.pool,
                        run_id,
                        "message",
                        &json!({ "text": text, "delivered": true }),
                    )
                    .await;
                    return Ok("delivered");
                }
                runs::push_message(&self.ctx.pool, run_id, text)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok("queued")
            }
            "queued" | "waiting_for_input" => {
                runs::push_message(&self.ctx.pool, run_id, text)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok("queued")
            }
            other => Err(format!("the run is {other}")),
        }
    }

    pub async fn cancel_run(&self, run_id: Uuid) -> Result<(), String> {
        let run = runs::get(&self.ctx.pool, run_id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("the run is not known")?;
        if matches!(run.status.as_str(), "succeeded" | "failed" | "cancelled") {
            return Err(format!("the run is already {}", run.status));
        }
        if let Some(runtime) = &run.runtime {
            self.runtime_call(runtime, json!({ "op": "stop", "run_id": run_id }))
                .await;
        }
        self.finish(&run, "cancelled", None, Some("cancelled by a person"))
            .await;
        Ok(())
    }

    /// `POST /v1/runs`: a run of a job by hand.
    pub async fn start_manual(&self, job: &str, input: &Value) -> Result<Run, String> {
        if !self.jobs.iter().any(|j| j.name == job) {
            return Err(format!("there is no job {job} (or it is turned off)"));
        }
        runs::create_manual(&self.ctx.pool, job, input)
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn runs(
        &self,
        job: Option<&str>,
        status: Option<&str>,
        limit: i64,
    ) -> sqlx::Result<Vec<Run>> {
        runs::list(&self.ctx.pool, job, status, limit).await
    }

    pub async fn run_events(&self, run_id: Uuid, after: i64) -> sqlx::Result<Vec<runs::RunEvent>> {
        runs::events(&self.ctx.pool, run_id, after, 500).await
    }
}
