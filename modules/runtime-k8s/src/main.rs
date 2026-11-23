//! Frood module `runtime-k8s`: `agents.execute@1` (contracts/agents.execute/1), one Kubernetes
//! Job per agent turn.
//!
//! The same runtime as `runtime-docker`, for a cluster without Docker: the agent programs, their
//! command lines, MCP wiring, events, results, costs and run states are the same (`agents.rs` is
//! the same file). What differs is where turns run:
//! - a run gets a PersistentVolumeClaim for the agent's home, where Claude Code and Codex keep
//!   their sessions, so the turn after a person's answer continues the same session; `stop`
//!   deletes it;
//! - each turn is a Job with one pod from the agent's image, its command line and credentials in
//!   a Secret the Job owns; the module reads the pod's log for events, the result and the cost,
//!   and deletes the Job (with its pod and Secret) when the turn has ended;
//! - agent pods run as a non-root user without privilege escalation or capabilities, on a
//!   read-only root, with no API token, within CPU and memory limits, and end at the run's
//!   `max_minutes` whatever the agent does.
//!
//! The module needs only namespaced RBAC: Jobs (create, get, patch, delete, deletecollection),
//! pods (list), pods/log (get), PersistentVolumeClaims (create, delete) and Secrets (create).
//!
//! Environment: `FROOD_TOKEN`, `FROOD_URL`, `FROOD_MODULE`, `FROOD_MODULE_CONFIG`, `FROOD_DATA`
//! (where run state is kept, default `/data`), `PORT` (8080), and the pod's ServiceAccount.

mod agents;
mod backend;
mod kube;
#[cfg(test)]
mod mock;

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
use backend::{Backend, ContainerSpec, Exit, KubeBackend, Placement};

const MANIFEST: &str = include_str!("../frood-module.yaml");

#[derive(Deserialize, Default)]
struct Settings {
    /// Agents this runtime offers, by name (`claude-code`, `codex`), with their images.
    #[serde(default)]
    agents: BTreeMap<String, AgentImage>,
    /// Namespace of the agent Jobs; the module's own by default.
    namespace: Option<String>,
    /// Where agent pods reach Frood (a Service's DNS name); `FROOD_URL` by default.
    frood_url: Option<String>,
    cpus: Option<f64>,
    memory_mb: Option<i64>,
    /// Claude Code's built-in tools agents may use, e.g. "Read,Grep"; none by default.
    #[serde(default)]
    builtin_tools: String,
    session_mb: Option<i64>,
    storage_class: Option<String>,
    read_only_root: Option<bool>,
    run_as_user: Option<i64>,
    service_account: Option<String>,
    #[serde(default)]
    image_pull_secrets: Vec<String>,
    #[serde(default)]
    labels: BTreeMap<String, String>,
    #[serde(default)]
    node_selector: BTreeMap<String, String>,
    keep_finished_minutes: Option<u64>,
    /// The API server, instead of the in-cluster one (e.g. `kubectl proxy` while developing).
    api_server: Option<String>,
}

impl Settings {
    fn placement(&self) -> Placement {
        let default = Placement::default();
        Placement {
            cpus: self.cpus,
            memory_mb: self.memory_mb,
            session_mb: self.session_mb.unwrap_or(default.session_mb),
            storage_class: self.storage_class.clone(),
            read_only_root: self.read_only_root.unwrap_or(default.read_only_root),
            run_as_user: self.run_as_user.unwrap_or(default.run_as_user),
            service_account: self.service_account.clone(),
            image_pull_secrets: self.image_pull_secrets.clone(),
            labels: self.labels.clone(),
            node_selector: self.node_selector.clone(),
            keep_finished_minutes: self
                .keep_finished_minutes
                .unwrap_or(default.keep_finished_minutes),
        }
    }
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
    /// The Job of the current turn, while it exists.
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
    max_minutes: Option<u64>,
}

/// Kubernetes ends a turn this long after the run's time budget, in case `stop` does not come:
/// the core stops runs over their minutes itself and says so.
const DEADLINE_GRACE_SECS: u64 = 60;

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

    /// Starts a turn of the run's agent in a new Job.
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
            deadline_secs: spec
                .budget
                .max_minutes
                .map(|minutes| minutes * 60 + DEADLINE_GRACE_SECS),
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
        let exited = matches!(state, Ok(Exit::Exited(_) | Exit::Failed(_) | Exit::Gone));
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
        let (code, stopped) = match state {
            Ok(Exit::Running) | Err(_) => return,
            Ok(Exit::Exited(code)) => (Some(code), None),
            Ok(Exit::Failed(why)) => (None, Some(why)),
            Ok(Exit::Gone) => (None, None),
        };
        let (result, cost, error) = record.done.take().unwrap_or((None, 0.0, None));
        record.cost_usd += cost;
        let failure = match (code, error) {
            (_, Some(error)) => Some(error),
            (None, None) => {
                Some(stopped.unwrap_or_else(|| "the agent's job disappeared".to_owned()))
            }
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
            tracing::warn!(job = %id, error = %err, "cannot remove a finished job");
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

/// The run's volume (a PersistentVolumeClaim).
fn volume_name(run_id: &str) -> String {
    format!("frood-run-{run_id}")
}

/// Lowercase letters, digits and dashes, short enough for `frood-run-<id>-<turn>` to be a Job
/// name (63 characters at most).
fn valid_run_id(run_id: &str) -> bool {
    (1..=40).contains(&run_id.len())
        && run_id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !run_id.starts_with('-')
        && !run_id.ends_with('-')
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
    // It names Kubernetes objects and the record's file.
    if !valid_run_id(&run_id) {
        return bad("run_id must be a UUID");
    }
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
                volume: volume_name(&run_id),
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
                volume: volume_name(&run_id),
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
                volume: volume_name(&run_id),
                ..Default::default()
            });
            if let Some(id) = &record.container {
                let _ = app.backend.remove(id).await;
            }
            // And any turn the module lost track of: a Job left suspended would stay forever,
            // with the run's key in its Secret.
            if let Err(err) = app.backend.remove_run_turns(&run_id).await {
                tracing::warn!(run = %run_id, error = %err, "cannot remove the run's jobs");
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
    let kube = match kube::Kube::in_cluster(
        settings.api_server.as_deref(),
        settings.namespace.as_deref(),
    ) {
        Ok(kube) => kube,
        Err(err) => {
            tracing::error!(error = %err, "cannot reach the Kubernetes API");
            std::process::exit(2);
        }
    };
    tracing::info!(namespace = %kube.namespace, "agent jobs go to this namespace");
    let backend = KubeBackend::new(kube, settings.placement());
    // Agent pods reach Frood by a Service's name, which may differ from the module's own way.
    let frood_url = settings
        .frood_url
        .clone()
        .unwrap_or_else(|| env("FROOD_URL"));
    let app = App {
        token: env("FROOD_TOKEN"),
        frood_url,
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
            let mut term =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("SIGTERM handler");
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {},
                _ = term.recv() => {},
            }
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

    use super::mock::{Cluster, NAMESPACE, TOKEN};
    use super::*;

    /// The fake Claude Code of runtime-docker: prints stream-json, keeps a session file in its
    /// home, asks when told to, fails when the prompt says so.
    const FAKE_CLAUDE: &str = include_str!("../../runtime-docker/images/agent-fake/claude");

    /// Both runtimes run the agents the same way: one file, kept equal.
    #[test]
    fn agents_are_the_same_as_in_runtime_docker() {
        assert!(
            include_str!("agents.rs") == include_str!("../../runtime-docker/src/agents.rs"),
            "copy modules/runtime-docker/src/agents.rs to modules/runtime-k8s/src/agents.rs"
        );
    }

    async fn app(dir: &tempfile::TempDir) -> (Arc<App<KubeBackend>>, Cluster) {
        let script = dir.path().join("claude");
        std::fs::write(&script, FAKE_CLAUDE).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let cluster = Cluster::start(dir.path()).await;
        let settings = Settings {
            agents: BTreeMap::from([
                (
                    "claude-code".to_owned(),
                    AgentImage {
                        image: script.display().to_string(),
                    },
                ),
                (
                    "codex".to_owned(),
                    AgentImage {
                        image: "missing/agent-codex:0.1.15".into(),
                    },
                ),
            ]),
            cpus: Some(1.0),
            memory_mb: Some(2048),
            ..Default::default()
        };
        let kube = kube::Kube::new(
            &cluster.url,
            kube::Token::Static(TOKEN.into()),
            NAMESPACE,
            None,
        )
        .unwrap();
        let backend = KubeBackend::new(kube, settings.placement());
        let app = Arc::new(App {
            token: "rt-token".into(),
            frood_url: "http://frood.agents-ns.svc:8080".into(),
            module: "agents".into(),
            settings,
            backend,
            data: dir.path().join("data"),
            runs: Mutex::new(HashMap::new()),
        });
        (app, cluster)
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
            "credentials": { "provider": "anthropic", "kind": "api_key", "api_key": "sk-test", "base_url": "https://llm-gateway.example.com" },
            "budget": { "max_usd": 0.5, "max_minutes": 15 },
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
        let (app, cluster) = app(&dir).await;
        let run = "0199f1c2-0000-7000-8000-000000000001";

        let mut start = spec("start", run);
        start["prompt"] =
            json!("Sweep the chat, $(HOME) and $$ stay as they are. ASK about the release.");
        let (status, started) = call(&app, start).await;
        assert_eq!(status, StatusCode::OK, "{started}");
        assert_eq!(started["session_ref"], run, "the session is the run id");
        {
            let world = cluster.world.lock().unwrap();
            let job = &world.jobs[&format!("frood-run-{run}-1")];
            assert_eq!(
                job["spec"]["suspend"], false,
                "let go once its Secret is there"
            );
            assert_eq!(job["spec"]["activeDeadlineSeconds"], 15 * 60 + 60);
            assert!(
                !job.to_string().contains("sk-test"),
                "the key is not in the Job"
            );
            assert!(
                !job.to_string().contains("frood_run_x"),
                "nor the run token"
            );
            let order: Vec<&str> = world.requests.iter().map(String::as_str).take(4).collect();
            assert_eq!(
                order,
                [
                    "POST persistentvolumeclaims",
                    "POST jobs",
                    "POST secrets",
                    &*format!("PATCH jobs/frood-run-{run}-1")
                ]
            );
        }
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
        let home = cluster
            .world
            .lock()
            .unwrap()
            .claim_dir(&format!("frood-run-{run}"));
        let env = std::fs::read_to_string(home.join("last-env")).unwrap();
        assert!(env.contains("key=sk-test"), "{env}");
        assert!(
            env.contains("base=https://llm-gateway.example.com"),
            "{env}"
        );
        let session = std::fs::read_to_string(home.join(".claude").join(run)).unwrap();
        assert!(
            session.contains("$(HOME) and $$ stay"),
            "the prompt arrives as written: {session}"
        );
        {
            let world = cluster.world.lock().unwrap();
            assert!(world.jobs.is_empty(), "the finished turn's Job is deleted");
            assert!(world.secrets.is_empty(), "and its Secret with it");
            assert!(
                world.claims.contains_key(&format!("frood-run-{run}")),
                "the sessions stay"
            );
        }

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
        assert!(cluster.world.lock().unwrap().claims.is_empty());
        let (status, _) = call(&app, json!({ "op": "status", "run_id": run })).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = call(&app, json!({ "op": "stop", "run_id": run })).await;
        assert_eq!(status, StatusCode::OK, "stop is idempotent");
    }

    #[tokio::test]
    async fn a_key_helper_runs_inside_the_agent_and_stays_out_of_the_job() {
        let dir = tempfile::tempdir().unwrap();
        let (app, cluster) = app(&dir).await;
        let run = "0199f1c2-0000-7000-8000-000000000005";
        let mut start = spec("start", run);
        start["prompt"] = json!("Go");
        start["credentials"] = json!({
            "provider": "anthropic", "kind": "api_key", "api_key_helper": "echo short-lived-key",
            "base_url": "http://llm-gateway.agents-ns.svc:4000/anthropic",
        });
        let (status, started) = call(&app, start).await;
        assert_eq!(status, StatusCode::OK, "{started}");
        {
            let world = cluster.world.lock().unwrap();
            let job = world.jobs[&format!("frood-run-{run}-1")].to_string();
            assert!(!job.contains("short-lived-key") && !job.contains("apiKeyHelper"));
            let secret = world.secrets.values().next().unwrap().to_string();
            assert!(!secret.contains("ANTHROPIC_API_KEY"), "{secret}");
        }
        let done = settle(&app, run).await;
        assert_eq!(done["state"], "finished", "{done}");
        let home = cluster
            .world
            .lock()
            .unwrap()
            .claim_dir(&format!("frood-run-{run}"));
        let env = std::fs::read_to_string(home.join("last-env")).unwrap();
        assert!(env.contains("key=short-lived-key"), "{env}");
        assert!(
            env.contains("base=http://llm-gateway.agents-ns.svc:4000/anthropic"),
            "{env}"
        );
    }

    #[tokio::test]
    async fn stop_ends_a_working_turn() {
        let dir = tempfile::tempdir().unwrap();
        let (app, cluster) = app(&dir).await;
        let run = "0199f1c2-0000-7000-8000-000000000004";
        let mut start = spec("start", run);
        start["prompt"] = json!("Go");
        let (status, _) = call(&app, start).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = call(&app, json!({ "op": "stop", "run_id": run })).await;
        assert_eq!(status, StatusCode::OK);
        let world = cluster.world.lock().unwrap();
        assert!(world.jobs.is_empty() && world.pods.is_empty() && world.secrets.is_empty());
        assert!(world.claims.is_empty());
    }

    #[tokio::test]
    async fn failures_and_refusals() {
        let dir = tempfile::tempdir().unwrap();
        let (app, cluster) = app(&dir).await;
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

        // An image that cannot be pulled fails the turn instead of waiting for the budget.
        let codex = "0199f1c2-0000-7000-8000-000000000003";
        let mut start = spec("start", codex);
        start["agent"] = json!("codex");
        start["credentials"] =
            json!({ "provider": "openai", "kind": "api_key", "api_key": "sk-test" });
        start["prompt"] = json!("Go");
        let (status, started) = call(&app, start).await;
        assert_eq!(status, StatusCode::OK, "{started}");
        let stuck = settle(&app, codex).await;
        assert_eq!(stuck["state"], "failed");
        assert!(
            stuck["error"]
                .as_str()
                .unwrap()
                .contains("ImagePullBackOff"),
            "{stuck}"
        );

        // Over its time, Kubernetes ends the turn.
        let late = "0199f1c2-0000-7000-8000-000000000005";
        let mut start = spec("start", late);
        start["prompt"] = json!("Go");
        call(&app, start).await;
        cluster
            .world
            .lock()
            .unwrap()
            .exceed_deadline(&format!("frood-run-{late}-1"));
        let over = settle(&app, late).await;
        assert_eq!(over["state"], "failed");
        assert!(
            over["error"].as_str().unwrap().contains("ran out of time"),
            "{over}"
        );

        // What RBAC does not allow is a retryable 503, and nothing is left behind.
        cluster.world.lock().unwrap().forbidden.push("jobs".into());
        let denied = "0199f1c2-0000-7000-8000-000000000006";
        let mut start = spec("start", denied);
        start["prompt"] = json!("Go");
        let (status, refused) = call(&app, start).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{refused}");
        assert_eq!(refused["retryable"], true);
        assert!(
            refused["detail"].as_str().unwrap().contains("forbidden"),
            "{refused}"
        );
        assert!(
            !cluster
                .world
                .lock()
                .unwrap()
                .claims
                .contains_key(&format!("frood-run-{denied}")),
            "its volume is removed"
        );

        let mut other = spec("start", "0199f1c2-0000-7000-8000-000000000007");
        other["agent"] = json!("aider");
        other["prompt"] = json!("Go");
        let (status, refused) = call(&app, other).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            refused["detail"]
                .as_str()
                .unwrap()
                .contains("no agent aider")
        );

        let (status, _) = call(&app, json!({ "op": "status", "run_id": "../etc" })).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "a run id names objects and files"
        );

        let request = Request::post("/v1/capabilities/agents.execute@1")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(spec("status", run).to_string()))
            .unwrap();
        let response = router(app.clone()).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    /// The mock API server refuses what the Role does not allow; the Role the manifest asks for
    /// and the one the Helm chart makes are that same Role.
    #[test]
    fn the_role_is_the_same_everywhere() {
        let role: BTreeMap<String, Vec<String>> = super::mock::ROLE
            .iter()
            .map(|(r, verbs)| (r.to_string(), verbs.iter().map(|v| v.to_string()).collect()))
            .collect();
        let manifest: Value = serde_yaml_ng::from_str(MANIFEST).unwrap();
        let asked: BTreeMap<String, Vec<String>> = manifest["permissions"]["kubernetes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|rule| {
                (
                    rule["resources"][0].as_str().unwrap().to_owned(),
                    serde_json::from_value(rule["verbs"].clone()).unwrap(),
                )
            })
            .collect();
        assert_eq!(asked, role, "frood-module.yaml");
        // The chart's rules are plain YAML lines: `resources: [x]` then `verbs: [a, b]`.
        let chart = include_str!("../../../deploy/helm/frood/templates/agents.yaml");
        let list = |line: &str, key: &str| -> Option<Vec<String>> {
            let inner = line.trim().strip_prefix(key)?.trim();
            let inner = inner.strip_prefix('[')?.strip_suffix(']')?;
            Some(inner.split(',').map(|v| v.trim().to_owned()).collect())
        };
        let lines: Vec<&str> = chart.lines().collect();
        let made: BTreeMap<String, Vec<String>> = lines
            .windows(2)
            .filter_map(|pair| {
                let resource = list(pair[0], "resources:")?.remove(0);
                Some((resource, list(pair[1], "verbs:")?))
            })
            .collect();
        assert_eq!(made, role, "the chart's Role");
    }

    #[tokio::test]
    async fn leftover_jobs_of_a_run_are_removed() {
        let dir = tempfile::tempdir().unwrap();
        let (app, cluster) = app(&dir).await;
        let run = "0199f1c2-0000-7000-8000-000000000008";
        // The module went down after creating the first turn's Job and before saving the run:
        // the Job is still suspended, with its Secret.
        let leftover = |name: &str, uid: &str| {
            let mut world = cluster.world.lock().unwrap();
            world.jobs.insert(
                name.to_owned(),
                json!({ "metadata": { "name": name, "uid": uid, "labels": {
                    "frood.run": run, "app.kubernetes.io/managed-by": "frood-runtime-k8s" } },
                    "spec": { "suspend": true }, "status": {} }),
            );
            world.secrets.insert(
                name.to_owned(),
                json!({ "metadata": { "name": name, "ownerReferences": [{ "uid": uid }] } }),
            );
        };
        leftover(&format!("frood-run-{run}-1"), "uid-left-1");
        let mut start = spec("start", run);
        start["prompt"] = json!("Go");
        // A retried start finds it: it is removed, and the next attempt works.
        let (status, refused) = call(&app, start.clone()).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{refused}");
        assert_eq!(refused["retryable"], true);
        {
            let world = cluster.world.lock().unwrap();
            assert!(world.jobs.is_empty() && world.secrets.is_empty());
        }
        let (status, started) = call(&app, start).await;
        assert_eq!(status, StatusCode::OK, "{started}");
        assert_eq!(settle(&app, run).await["state"], "finished");

        // `stop` takes every Job of the run, also those the module does not know of.
        leftover(&format!("frood-run-{run}-7"), "uid-left-7");
        leftover("frood-run-someone-else-1", "uid-other");
        cluster
            .world
            .lock()
            .unwrap()
            .jobs
            .get_mut("frood-run-someone-else-1")
            .unwrap()["metadata"]["labels"]["frood.run"] = json!("someone-else");
        let (status, _) = call(&app, json!({ "op": "stop", "run_id": run })).await;
        assert_eq!(status, StatusCode::OK);
        let world = cluster.world.lock().unwrap();
        assert_eq!(
            world.jobs.keys().collect::<Vec<_>>(),
            ["frood-run-someone-else-1"],
            "only the run's own"
        );
        assert_eq!(world.secrets.len(), 1);
        assert!(world.claims.is_empty());
    }

    #[tokio::test]
    async fn the_api_server_wants_the_modules_token() {
        let dir = tempfile::tempdir().unwrap();
        let cluster = Cluster::start(dir.path()).await;
        let kube = kube::Kube::new(&cluster.url, kube::Token::None, NAMESPACE, None).unwrap();
        let err = kube.get(kube::Kind::Job, "x").await.unwrap_err();
        assert_eq!(err.status, 401);
        let token = dir.path().join("token");
        std::fs::write(&token, format!("{TOKEN}\n")).unwrap();
        let kube =
            kube::Kube::new(&cluster.url, kube::Token::File(token), NAMESPACE, None).unwrap();
        assert!(kube.get(kube::Kind::Job, "x").await.unwrap().is_none());
    }
}
