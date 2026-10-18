//! Frood module `runtime-docker`: `agents.execute@1` (contracts/agents.execute/1), one Docker
//! container per agent turn.
//!
//! A run gets a Docker volume for the agent's home: Claude Code and Codex keep their sessions
//! there, so the turn after a person's answer continues the same session. Each turn is a fresh
//! container from the agent's image with the run's credentials and MCP servers; the module reads
//! its output for events, the result and the cost, and removes it when it exits. `stop` removes
//! the volume too.
//!
//! The module talks to the host's Docker, which is root on that machine (A2). Agent containers
//! drop all capabilities, run without new privileges on a read-only root, see only their own
//! volume, and have CPU, memory and process limits.
//!
//! Environment (written by `frood apply`): `FROOD_TOKEN`, `FROOD_URL`, `FROOD_MODULE`,
//! `FROOD_MODULE_CONFIG`, `FROOD_DOCKER_NETWORK` (the deployment's network), `FROOD_DATA`
//! (where run state is kept, default `/data`), `PORT` (8080).

mod agents;
mod backend;

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use agents::{Credentials, McpServer, Parsed};
use backend::{Backend, ContainerSpec, DockerBackend, Exit, Placement};

const MANIFEST: &str = include_str!("../frood-module.yaml");

#[derive(Deserialize, Default)]
struct Settings {
    /// Agents this runtime offers, by name (`claude-code`, `codex`), with their images.
    #[serde(default)]
    agents: BTreeMap<String, AgentImage>,
    /// Overrides the deployment's network.
    network: Option<String>,
    cpus: Option<f64>,
    memory_mb: Option<i64>,
    /// Claude Code's built-in tools agents may use, e.g. "Read,Grep"; none by default.
    #[serde(default)]
    builtin_tools: String,
}

#[derive(Deserialize, Clone)]
struct AgentImage {
    image: String,
}

/// What the module remembers about a run, kept as a file so it survives restarts.
#[derive(Serialize, Deserialize, Clone, Default)]
struct RunRecord {
    run_id: String,
    agent: String,
    volume: String,
    session_ref: Option<String>,
    /// The container of the current turn, while it exists.
    container: Option<String>,
    turns: u32,
    /// Lines of the current turn's output already read.
    lines_seen: usize,
    state: String,
    /// Cost of the finished turns.
    cost_usd: f64,
    result: Option<String>,
    error: Option<String>,
    /// The current turn's last message, and how it ended as the agent reports it.
    answer: Option<String>,
    done: Option<(Option<String>, f64, Option<String>)>,
    events: Vec<Value>,
}

struct App<B: Backend> {
    token: String,
    frood_url: String,
    module: String,
    settings: Settings,
    backend: B,
    data: PathBuf,
    runs: Mutex<HashMap<String, RunRecord>>,
}

/// An error answer: problem details, `retryable` when trying later may help.
fn problem(status: StatusCode, detail: impl Into<String>, retryable: bool) -> Response {
    let body = json!({ "title": status.canonical_reason(), "status": status.as_u16(), "detail": detail.into(), "retryable": retryable });
    (
        status,
        [(header::CONTENT_TYPE, "application/problem+json")],
        Json(body),
    )
        .into_response()
}

fn bad(detail: impl Into<String>) -> Response {
    problem(StatusCode::BAD_REQUEST, detail, false)
}

fn router<B: Backend>(app: Arc<App<B>>) -> Router {
    Router::new()
        .route("/manifest", get(manifest))
        .route(
            "/healthz",
            get(|| async { Json(json!({ "status": "ok" })) }),
        )
        .route("/v1/capabilities/agents.execute@1", post(execute::<B>))
        .with_state(app)
}

async fn manifest() -> Response {
    match serde_yaml_ng::from_str::<Value>(MANIFEST) {
        Ok(value) => Json(value).into_response(),
        Err(err) => problem(StatusCode::INTERNAL_SERVER_ERROR, err.to_string(), false),
    }
}

#[derive(Deserialize)]
struct Spec {
    run_id: String,
    agent: String,
    model: String,
    #[serde(default)]
    instructions: String,
    #[serde(default)]
    mcp_servers: Vec<McpServer>,
    credentials: Credentials,
    #[serde(default)]
    budget: Budget,
}

#[derive(Deserialize, Default)]
struct Budget {
    max_usd: Option<f64>,
}

impl<B: Backend> App<B> {
    fn record_path(&self, run_id: &str) -> PathBuf {
        self.data.join("runs").join(format!("{run_id}.json"))
    }

    fn load(&self) -> HashMap<String, RunRecord> {
        let mut runs = HashMap::new();
        let Ok(dir) = std::fs::read_dir(self.data.join("runs")) else {
            return runs;
        };
        for entry in dir.flatten() {
            if let Ok(text) = std::fs::read_to_string(entry.path())
                && let Ok(record) = serde_json::from_str::<RunRecord>(&text)
            {
                runs.insert(record.run_id.clone(), record);
            }
        }
        runs
    }

    fn save(&self, record: &RunRecord) {
        let path = self.record_path(&record.run_id);
        let tmp = path.with_extension("json.tmp");
        let written = std::fs::create_dir_all(path.parent().expect("runs dir"))
            .and_then(|()| {
                std::fs::write(&tmp, serde_json::to_vec(record).expect("records serialize"))
            })
            .and_then(|()| std::fs::rename(&tmp, &path));
        if let Err(err) = written {
            tracing::error!(run = %record.run_id, error = %err, "cannot save the run's state");
        }
    }

    /// Starts a turn of the run's agent in a new container.
    async fn start_turn(
        &self,
        record: &mut RunRecord,
        spec: &Spec,
        prompt: &str,
        resume: Option<&str>,
    ) -> Result<(), Box<Response>> {
        let image =
            self.settings.agents.get(&spec.agent).ok_or_else(|| {
                Box::new(bad(format!("this runtime has no agent {}", spec.agent)))
            })?;
        let remaining = spec
            .budget
            .max_usd
            .map(|max| (max - record.cost_usd).max(0.0));
        if remaining == Some(0.0) {
            return Err(Box::new(bad("the run's budget is spent")));
        }
        let turn = agents::Turn {
            agent: &spec.agent,
            model: &spec.model,
            instructions: &spec.instructions,
            prompt,
            // Claude Code's session is the run id: known before the agent starts.
            new_session: (spec.agent == "claude-code").then_some(spec.run_id.as_str()),
            resume,
            mcp_servers: &spec.mcp_servers,
            frood_url: &self.frood_url,
            credentials: &spec.credentials,
            max_budget_usd: remaining,
            builtin_tools: &self.settings.builtin_tools,
        };
        let command = agents::command(&turn).map_err(|err| Box::new(bad(err)))?;
        record.turns += 1;
        let container = ContainerSpec {
            name: format!("frood-run-{}-{}", record.run_id, record.turns),
            image: image.image.clone(),
            args: command.args,
            env: command.env,
            volume: record.volume.clone(),
            labels: HashMap::from([
                ("frood.run".to_owned(), record.run_id.clone()),
                ("frood.module".to_owned(), self.module.clone()),
            ]),
        };
        let id = self.backend.run(&container).await.map_err(|err| {
            Box::new(problem(
                StatusCode::SERVICE_UNAVAILABLE,
                format!("cannot start the agent: {err}"),
                true,
            ))
        })?;
        record.container = Some(id);
        record.lines_seen = 0;
        record.state = "running".into();
        record.answer = None;
        record.done = None;
        if spec.agent == "claude-code" && resume.is_none() {
            record.session_ref = Some(spec.run_id.clone());
        }
        Ok(())
    }

    fn apply(record: &mut RunRecord, line: &str) {
        for parsed in agents::parse_line(&record.agent, line) {
            match parsed {
                Parsed::Event(event) => record.events.push(event),
                Parsed::Session(session) => record.session_ref = Some(session),
                Parsed::Answer(text) => record.answer = Some(text),
                Parsed::Done {
                    result,
                    cost_usd,
                    error,
                } => record.done = Some((result, cost_usd, error)),
            }
        }
    }

    /// Reads what the current turn printed and, when it has exited, closes it.
    async fn refresh(&self, record: &mut RunRecord) {
        let Some(id) = record.container.clone() else {
            return;
        };
        let state = self.backend.state(&id).await;
        let output = self.backend.stdout(&id).await.unwrap_or_default();
        let exited = matches!(state, Ok(Exit::Exited(_) | Exit::Gone));
        let lines: Vec<&str> = output.split_inclusive('\n').collect();
        // A line without its newline may still be written, unless the agent has exited.
        let complete = if exited {
            lines.len()
        } else {
            lines.iter().filter(|l| l.ends_with('\n')).count()
        };
        for line in lines.iter().take(complete).skip(record.lines_seen) {
            Self::apply(record, line);
        }
        record.lines_seen = record.lines_seen.max(complete);
        let code = match state {
            Ok(Exit::Running) | Err(_) => return,
            Ok(Exit::Exited(code)) => Some(code),
            Ok(Exit::Gone) => None,
        };
        let (result, cost, error) = record.done.take().unwrap_or((None, 0.0, None));
        record.cost_usd += cost;
        let failure = match (code, error) {
            (_, Some(error)) => Some(error),
            (None, None) => Some("the agent's container disappeared".to_owned()),
            (Some(0), None) => None,
            (Some(code), None) => {
                let stderr = self.backend.stderr_tail(&id).await.unwrap_or_default();
                Some(format!(
                    "the agent exited with code {code}: {}",
                    stderr.trim()
                ))
            }
        };
        match failure {
            None => {
                record.state = "finished".into();
                record.result = result.or_else(|| record.answer.take());
                record.error = None;
            }
            Some(error) => {
                record
                    .events
                    .push(json!({ "kind": "error", "data": { "message": error } }));
                record.state = "failed".into();
                record.error = Some(error);
            }
        }
        if let Err(err) = self.backend.remove(&id).await {
            tracing::warn!(container = %id, error = %err, "cannot remove a finished container");
        }
        record.container = None;
    }

    fn status(record: &RunRecord) -> Value {
        json!({
            "state": record.state,
            "session_ref": record.session_ref,
            "cost_usd": record.cost_usd,
            "result": record.result,
            "error": record.error,
        })
    }
}

async fn execute<B: Backend>(
    State(app): State<Arc<App<B>>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let presented = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    if presented != Some(app.token.as_str()) {
        return problem(StatusCode::UNAUTHORIZED, "bad or missing token", false);
    }
    let Some(run_id) = body["run_id"].as_str().map(str::to_owned) else {
        return bad("run_id is required");
    };
    let mut runs = app.runs.lock().await;
    match body["op"].as_str().unwrap_or_default() {
        "start" => {
            let spec: Spec = match serde_json::from_value(body.clone()) {
                Ok(spec) => spec,
                Err(err) => return bad(format!("start: {err}")),
            };
            if let Some(record) = runs.get(&run_id)
                && record.container.is_some()
            {
                // A retried start: the turn is already there.
                return Json(json!({ "session_ref": record.session_ref, "state": "running" }))
                    .into_response();
            }
            let Some(prompt) = body["prompt"].as_str() else {
                return bad("start: prompt is required");
            };
            let mut record = RunRecord {
                run_id: run_id.clone(),
                agent: spec.agent.clone(),
                volume: format!("frood-run-{run_id}"),
                ..Default::default()
            };
            if let Err(err) = app.backend.create_volume(&record.volume).await {
                return problem(
                    StatusCode::SERVICE_UNAVAILABLE,
                    format!("cannot create the run's volume: {err}"),
                    true,
                );
            }
            if let Err(response) = app.start_turn(&mut record, &spec, prompt, None).await {
                let _ = app.backend.remove_volume(&record.volume).await;
                return *response;
            }
            app.save(&record);
            let reply = json!({ "session_ref": record.session_ref, "state": "running" });
            runs.insert(run_id, record);
            Json(reply).into_response()
        }
        "resume" => {
            let spec: Spec = match serde_json::from_value(body.clone()) {
                Ok(spec) => spec,
                Err(err) => return bad(format!("resume: {err}")),
            };
            let record = runs.entry(run_id.clone()).or_insert_with(|| RunRecord {
                // The module lost its state; the volume with the session may still be there.
                run_id: run_id.clone(),
                agent: spec.agent.clone(),
                volume: format!("frood-run-{run_id}"),
                ..Default::default()
            });
            app.refresh(record).await;
            if record.container.is_some() {
                return problem(StatusCode::CONFLICT, "the agent is still working", true);
            }
            let Some(session) = record
                .session_ref
                .clone()
                .or_else(|| body["session_ref"].as_str().map(str::to_owned))
            else {
                return bad("resume: the run has no session to continue");
            };
            let message = body["message"].as_str().unwrap_or_default();
            if let Err(response) = app.start_turn(record, &spec, message, Some(&session)).await {
                return *response;
            }
            app.save(record);
            Json(json!({ "session_ref": record.session_ref, "state": "running" })).into_response()
        }
        "send" => {
            if !runs.contains_key(&run_id) {
                return problem(StatusCode::NOT_FOUND, "no such run", false);
            }
            // Agents here take input only between turns: the core resumes with the message.
            Json(json!({ "delivered": false })).into_response()
        }
        "stop" => {
            let record = runs.remove(&run_id).unwrap_or_else(|| RunRecord {
                volume: format!("frood-run-{run_id}"),
                ..Default::default()
            });
            if let Some(id) = &record.container {
                let _ = app.backend.remove(id).await;
            }
            if let Err(err) = app.backend.remove_volume(&record.volume).await {
                tracing::warn!(run = %run_id, error = %err, "cannot remove the run's volume");
            }
            let _ = std::fs::remove_file(app.record_path(&run_id));
            Json(json!({})).into_response()
        }
        op @ ("status" | "events") => {
            let Some(record) = runs.get_mut(&run_id) else {
                return problem(StatusCode::NOT_FOUND, "no such run", false);
            };
            app.refresh(record).await;
            app.save(record);
            if op == "status" {
                return Json(App::<B>::status(record)).into_response();
            }
            let after = body["after"].as_u64().unwrap_or(0) as usize;
            let events = record.events.get(after..).unwrap_or_default().to_vec();
            Json(json!({ "events": events })).into_response()
        }
        other => bad(format!("unknown op {other:?}")),
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let env = |name: &str| std::env::var(name).unwrap_or_default();
    let settings: Settings = match serde_json::from_str(&env("FROOD_MODULE_CONFIG")) {
        Ok(settings) => settings,
        Err(err) => {
            tracing::error!(error = %err, "FROOD_MODULE_CONFIG is not valid settings");
            std::process::exit(2);
        }
    };
    if settings.agents.is_empty() {
        tracing::error!(
            "name at least one agent and its image in the module's config: agents: {{ claude-code: {{ image: … }} }}"
        );
        std::process::exit(2);
    }
    let placement = Placement {
        network: settings
            .network
            .clone()
            .or_else(|| std::env::var("FROOD_DOCKER_NETWORK").ok()),
        cpus: settings.cpus,
        memory_mb: settings.memory_mb,
    };
    let backend = match DockerBackend::connect(placement) {
        Ok(backend) => backend,
        Err(err) => {
            tracing::error!(error = %err, "cannot reach Docker; mount the Docker socket");
            std::process::exit(2);
        }
    };
    let app = App {
        token: env("FROOD_TOKEN"),
        frood_url: env("FROOD_URL"),
        module: env("FROOD_MODULE"),
        settings,
        backend,
        data: PathBuf::from(std::env::var("FROOD_DATA").unwrap_or_else(|_| "/data".into())),
        runs: Mutex::new(HashMap::new()),
    };
    let runs = app.load();
    tracing::info!(runs = runs.len(), "runtime ready");
    *app.runs.lock().await = runs;
    let port = std::env::var("PORT").unwrap_or_else(|_| "8080".into());
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}"))
        .await
        .expect("port is free");
    axum::serve(listener, router(Arc::new(app)))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .expect("server runs");
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    use super::backend::process::ProcessBackend;
    use super::*;

    /// A stand-in for `claude` that prints stream-json like Claude Code: it asks when told to,
    /// keeps a session file in its home, and fails when the prompt says so.
    const FAKE_CLAUDE: &str = include_str!("../images/agent-fake/claude");

    fn app(dir: &tempfile::TempDir) -> Arc<App<ProcessBackend>> {
        let script = dir.path().join("claude");
        std::fs::write(&script, FAKE_CLAUDE).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let settings = Settings {
            agents: BTreeMap::from([(
                "claude-code".to_owned(),
                AgentImage {
                    image: script.display().to_string(),
                },
            )]),
            ..Default::default()
        };
        Arc::new(App {
            token: "rt-token".into(),
            frood_url: "http://frood:8080".into(),
            module: "agents".into(),
            settings,
            backend: ProcessBackend::new(dir.path().to_owned()),
            data: dir.path().join("data"),
            runs: Mutex::new(HashMap::new()),
        })
    }

    async fn call<B: Backend>(app: &Arc<App<B>>, body: Value) -> (StatusCode, Value) {
        let request = Request::post("/v1/capabilities/agents.execute@1")
            .header(header::AUTHORIZATION, "Bearer rt-token")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = router(app.clone()).oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = http_body_util::BodyExt::collect(response.into_body())
            .await
            .unwrap()
            .to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    fn spec(op: &str, run: &str) -> Value {
        json!({
            "op": op, "run_id": run, "job": "daily-sweep", "agent": "claude-code",
            "model": "claude-haiku-5-5", "instructions": "Sweep.",
            "mcp_servers": [{ "name": "frood", "path": "/mcp", "headers": { "Authorization": "Bearer frood_run_x" } }],
            "credentials": { "provider": "anthropic", "kind": "api_key", "api_key": "sk-test" },
            "budget": { "max_usd": 0.5 },
        })
    }

    /// Polls `status` until the turn is no longer running.
    async fn settle<B: Backend>(app: &Arc<App<B>>, run: &str) -> Value {
        for _ in 0..100 {
            let (_, status) = call(app, json!({ "op": "status", "run_id": run })).await;
            if status["state"] != "running" {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        panic!("the turn did not end");
    }

    #[tokio::test]
    async fn a_run_asks_then_resumes_its_session_and_finishes() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(&dir);
        let run = "0199f1c2-0000-7000-8000-000000000001";

        let mut start = spec("start", run);
        start["prompt"] = json!("Sweep the chat. ASK about the release.");
        let (status, started) = call(&app, start).await;
        assert_eq!(status, StatusCode::OK, "{started}");
        assert_eq!(started["session_ref"], run, "the session is the run id");
        let first = settle(&app, run).await;
        assert_eq!(first["state"], "finished", "{first}");
        assert_eq!(first["result"], "Asked.");
        assert_eq!(first["cost_usd"], 0.01);
        let (_, events) = call(&app, json!({ "op": "events", "run_id": run, "after": 0 })).await;
        let kinds: Vec<&str> = events["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["kind"].as_str().unwrap())
            .collect();
        assert_eq!(kinds, ["init", "tool_use", "usage"]);
        let home = app.backend.volume_dir(&format!("frood-run-{run}"));
        let env = std::fs::read_to_string(home.join("last-env")).unwrap();
        assert!(env.contains("key=sk-test"), "{env}");

        // A message comes later: delivered with the next turn, in the same session.
        let (_, sent) = call(
            &app,
            json!({ "op": "send", "run_id": run, "message": "hi" }),
        )
        .await;
        assert_eq!(sent["delivered"], false);
        let mut resume = spec("resume", run);
        resume["session_ref"] = json!(run);
        resume["message"] = json!("Answers to your questions: Decision");
        let (status, resumed) = call(&app, resume).await;
        assert_eq!(status, StatusCode::OK, "{resumed}");
        let second = settle(&app, run).await;
        assert_eq!(second["state"], "finished", "{second}");
        assert_eq!(second["result"], "Proposed 1 change");
        assert!(
            (second["cost_usd"].as_f64().unwrap() - 0.03).abs() < 1e-9,
            "costs add up"
        );
        let env = std::fs::read_to_string(home.join("last-env")).unwrap();
        assert!(env.contains("turns=2"), "same session file: {env}");
        let (_, events) = call(&app, json!({ "op": "events", "run_id": run, "after": 3 })).await;
        assert_eq!(events["events"][1]["kind"], "assistant");

        // The state survives a restart of the module.
        let reloaded = app.load();
        assert_eq!(reloaded[run].cost_usd, second["cost_usd"].as_f64().unwrap());

        let (status, _) = call(&app, json!({ "op": "stop", "run_id": run })).await;
        assert_eq!(status, StatusCode::OK);
        assert!(!home.exists(), "the volume is gone");
        let (status, _) = call(&app, json!({ "op": "status", "run_id": run })).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = call(&app, json!({ "op": "stop", "run_id": run })).await;
        assert_eq!(status, StatusCode::OK, "stop is idempotent");
    }

    #[tokio::test]
    async fn failures_and_refusals() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(&dir);
        let run = "0199f1c2-0000-7000-8000-000000000002";

        let mut start = spec("start", run);
        start["prompt"] = json!("please crash");
        call(&app, start).await;
        let failed = settle(&app, run).await;
        assert_eq!(failed["state"], "failed");
        assert!(
            failed["error"].as_str().unwrap().contains("code 3: boom"),
            "{failed}"
        );

        let mut codex = spec("start", "0199f1c2-0000-7000-8000-000000000003");
        codex["agent"] = json!("codex");
        codex["prompt"] = json!("Go");
        let (status, refused) = call(&app, codex).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            refused["detail"]
                .as_str()
                .unwrap()
                .contains("no agent codex")
        );

        let request = Request::post("/v1/capabilities/agents.execute@1")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(spec("status", run).to_string()))
            .unwrap();
        let response = router(app.clone()).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    #[ignore = "needs Docker and the image frood-agent-fake:test (docker build -t frood-agent-fake:test images/agent-fake)"]
    async fn docker_runs_each_turn_in_a_container_and_keeps_the_session() {
        let dir = tempfile::tempdir().unwrap();
        let settings = Settings {
            agents: BTreeMap::from([(
                "claude-code".to_owned(),
                AgentImage {
                    image: "frood-agent-fake:test".into(),
                },
            )]),
            cpus: Some(0.5),
            memory_mb: Some(256),
            ..Default::default()
        };
        let app = Arc::new(App {
            token: "rt-token".into(),
            frood_url: "http://frood:8080".into(),
            module: "agents-test".into(),
            settings,
            backend: DockerBackend::connect(Placement {
                cpus: Some(0.5),
                memory_mb: Some(256),
                ..Default::default()
            })
            .unwrap(),
            data: dir.path().join("data"),
            runs: Mutex::new(HashMap::new()),
        });
        let run = format!("0199f1c2-0000-7000-8000-{:012}", std::process::id());
        let mut start = spec("start", &run);
        start["prompt"] = json!("ASK about the release");
        let (status, started) = call(&app, start).await;
        assert_eq!(status, StatusCode::OK, "{started}");
        let first = settle(&app, &run).await;
        assert_eq!(first["state"], "finished", "{first}");

        // The second turn is a new container on the same volume: the session file is there.
        let mut resume = spec("resume", &run);
        resume["message"] = json!("Decision");
        let (status, resumed) = call(&app, resume).await;
        assert_eq!(status, StatusCode::OK, "{resumed}");
        let second = settle(&app, &run).await;
        assert_eq!(second["state"], "finished", "{second}");
        assert_eq!(second["result"], "Proposed 1 change");

        call(&app, json!({ "op": "stop", "run_id": run })).await;
        let docker = bollard::Docker::connect_with_local_defaults().unwrap();
        assert!(
            docker
                .inspect_volume(&format!("frood-run-{run}"))
                .await
                .is_err(),
            "the volume is removed"
        );
    }
}
