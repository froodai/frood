//! Frood module `notify-openclaw`: the Inbox in an OpenClaw conversation (docs/28 §4,
//! `notify.send@1` / `notify.update@1`): WhatsApp, iMessage, Signal, whatever the person talks
//! to their OpenClaw agent through.
//!
//! Runs `openclaw mcp serve` as a child and talks MCP to it over stdio. A person links a
//! conversation by sending `frood <code>` in it; the conversation's session key is then their
//! account in this channel. Items arrive as text with numbered choices; a reply with the number,
//! or yes or no, acts as that person. Only the owner's messages count: in a group anyone else
//! writes to the agent, not to Frood. OpenClaw has no edits, so what ends elsewhere comes as a
//! new message. Which items wait for a reply is kept in memory: after a restart they are
//! answered in the console.
//!
//! Environment (written by `frood apply`): `FROOD_URL`, `FROOD_TOKEN`, `FROOD_MODULE` (the
//! channel name), `FROOD_MODULE_CONFIG` (`{"gateway": "ws://…", "language": "en"}`),
//! `OPENCLAW_GATEWAY_TOKEN`, optionally `OPENCLAW_COMMAND` (`openclaw`), `PORT` (8080).

mod bridge;
mod render;
mod words;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use rmcp::RoleClient;
use rmcp::service::Peer;
use serde_json::{Value, json};

use bridge::{Launch, Next};
use words::{Lang, Words};

const MANIFEST: &str = include_str!("../frood-module.yaml");
/// The items of a conversation that wait for a reply; the newest is the one a reply answers.
const MAX_WAITING: usize = 20;

/// An item shown in a conversation, with the choices its message offers.
struct Waiting {
    item: String,
    actions: Vec<Value>,
}

struct Module {
    http: reqwest::Client,
    frood: String,
    token: String,
    channel: String,
    words: &'static Words,
    launch: Launch,
    retry_after: Duration,
    peer: tokio::sync::RwLock<Option<Peer<RoleClient>>>,
    waiting: Mutex<HashMap<String, Vec<Waiting>>>,
}

impl Module {
    fn frood(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        request.bearer_auth(&self.token)
    }

    /// Keeps the bridge running and reads its events; a bridge that stops is started again.
    async fn run(self: Arc<Self>) {
        loop {
            match self.launch.start().await {
                Ok(service) => {
                    tracing::info!(gateway = %self.launch.gateway, "connected to OpenClaw");
                    *self.peer.write().await = Some(service.peer().clone());
                    self.read_events(service.peer()).await;
                    *self.peer.write().await = None;
                    let _ = service.cancel().await;
                }
                Err(err) => tracing::warn!(error = %err, "no OpenClaw"),
            }
            tokio::time::sleep(self.retry_after).await;
        }
    }

    async fn read_events(&self, peer: &Peer<RoleClient>) {
        let mut cursor = 0;
        loop {
            match bridge::next_event(peer, cursor).await {
                Ok(Next::Event(event)) => {
                    cursor = event["cursor"].as_u64().unwrap_or(cursor);
                    self.on_event(peer, &event).await;
                }
                Ok(Next::Gap(after)) => cursor = after,
                Ok(Next::Nothing) => {}
                Err(err) => {
                    tracing::warn!(error = %err, "the OpenClaw bridge stopped");
                    return;
                }
            }
        }
    }

    /// A message from the conversation's owner: a link code or a reply to the newest item.
    async fn on_event(&self, peer: &Peer<RoleClient>, event: &Value) {
        let owner = event["type"] == "message"
            && event["role"] == "user"
            && event["raw"]["senderIsOwner"] == true;
        let (Some(session), Some(text)) = (event["sessionKey"].as_str(), event["text"].as_str())
        else {
            return;
        };
        if !owner {
            return;
        }
        let said = if let Some(code) = render::link_code(text) {
            Some(self.link(session, &code, &event["conversation"]).await)
        } else {
            self.answer(session, text).await
        };
        if let Some(said) = said
            && let Err(err) = bridge::send(peer, session, &said).await
        {
            tracing::warn!(error = %err, "could not answer in the conversation");
        }
    }

    async fn link(&self, session: &str, code: &str, conversation: &Value) -> String {
        let label = conversation["channel"].as_str();
        let response = self
            .frood(
                self.http
                    .post(format!("{}/v1/channels/{}/links", self.frood, self.channel)),
            )
            .json(&json!({ "code": code, "external_id": session, "label": label }))
            .send()
            .await;
        match response {
            Ok(r) if r.status().is_success() => {
                let linked: Value = r.json().await.unwrap_or(Value::Null);
                format!(
                    "{} {}",
                    self.words.linked,
                    linked["email"].as_str().unwrap_or("Frood")
                )
            }
            Ok(r) => {
                let body: Value = r.json().await.unwrap_or(Value::Null);
                let detail = body["detail"].as_str().unwrap_or("?");
                format!("{}: {detail}", self.words.link_failed)
            }
            Err(err) => format!("{}: {err}", self.words.link_failed),
        }
    }

    /// The reply's action on the newest waiting item; nothing when it is talk for the agent.
    async fn answer(&self, session: &str, text: &str) -> Option<String> {
        let (item, action) = {
            let waiting = self.waiting.lock().expect("waiting");
            let newest = waiting.get(session)?.last()?;
            (newest.item.clone(), render::reply(text, &newest.actions)?)
        };
        let mut body = action.clone();
        body["external_id"] = json!(session);
        let response = self
            .frood(
                self.http
                    .post(format!("{}/v1/inbox/{item}/act", self.frood)),
            )
            .json(&body)
            .send()
            .await;
        match response {
            Ok(r) if r.status().is_success() => {
                self.forget(session, &item);
                let done = json!({ "item": { "resolution": { "action": action["action"] } } });
                Some(render::outcome(&done, &self.channel, self.words))
            }
            Ok(r) => {
                let body: Value = r.json().await.unwrap_or(Value::Null);
                Some(format!("⚠️ {}", body["detail"].as_str().unwrap_or("?")))
            }
            Err(err) => Some(format!("⚠️ {err}")),
        }
    }

    /// True when the item was waiting here.
    fn forget(&self, session: &str, item: &str) -> bool {
        let mut waiting = self.waiting.lock().expect("waiting");
        let Some(items) = waiting.get_mut(session) else {
            return false;
        };
        let before = items.len();
        items.retain(|w| w.item != item);
        before != items.len()
    }

    /// `notify.send@1`: shows an item to a person.
    async fn notify_send(&self, body: &Value) -> Result<Value, Refused> {
        let session = body["recipient"]["external_id"]
            .as_str()
            .ok_or(Refused::Forever("no recipient".to_owned()))?;
        let shown = &body["item"];
        let item = shown["item"]["id"].as_str().unwrap_or_default().to_owned();
        let message = render::text(shown, body["console_url"].as_str(), self.words);
        let peer = self.peer.read().await.clone().ok_or(Refused::ForNow)?;
        // The bridge refuses a conversation it no longer has a route for: the core stops trying.
        bridge::send(&peer, session, &message)
            .await
            .map_err(Refused::Forever)?;
        let actions = render::choices(shown);
        if !actions.is_empty() {
            let mut waiting = self.waiting.lock().expect("waiting");
            let items = waiting.entry(session.to_owned()).or_default();
            items.push(Waiting {
                item: item.clone(),
                actions,
            });
            if items.len() > MAX_WAITING {
                items.remove(0);
            }
        }
        Ok(json!({ "message_ref": format!("{session}|{item}") }))
    }

    /// `notify.update@1`: what waited here ended elsewhere. No edits: a new message says so.
    async fn notify_update(&self, body: &Value) -> Result<Value, Refused> {
        let session = body["recipient"]["external_id"]
            .as_str()
            .unwrap_or_default();
        let shown = &body["item"];
        let item = shown["item"]["id"].as_str().unwrap_or_default();
        // Not waiting: answered here a moment ago, or nothing was asked.
        if !self.forget(session, item) {
            return Ok(json!({}));
        }
        let mut said = render::outcome(shown, &self.channel, self.words);
        if let Some(subject) = render::subject(shown) {
            said = format!("{said}: {subject}");
        }
        let peer = self.peer.read().await.clone().ok_or(Refused::ForNow)?;
        bridge::send(&peer, session, &said)
            .await
            .map_err(Refused::Forever)?;
        Ok(json!({}))
    }
}

enum Refused {
    /// The bridge is down: the core tries again.
    ForNow,
    Forever(String),
}

fn router(module: Arc<Module>) -> Router {
    Router::new()
        .route(
            "/manifest",
            get(|| async {
                Json(serde_yaml_ng::from_str::<Value>(MANIFEST).unwrap_or(Value::Null))
            }),
        )
        .route("/healthz", get(healthz))
        .route("/v1/capabilities/notify.send@1", post(notify_send))
        .route("/v1/capabilities/notify.update@1", post(notify_update))
        .with_state(module)
}

/// Alive even with OpenClaw away: the module itself works, and says what it misses.
async fn healthz(State(module): State<Arc<Module>>) -> Json<Value> {
    let connected = module.peer.read().await.is_some();
    Json(json!({ "status": "ok", "openclaw": connected }))
}

/// The core presents the module's token when it calls a capability.
fn from_core(module: &Module, headers: &HeaderMap) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        == Some(module.token.as_str())
}

fn problem(status: StatusCode, detail: &str, retryable: bool) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/problem+json")],
        Json(json!({
            "title": status.canonical_reason(),
            "status": status.as_u16(),
            "detail": detail,
            "retryable": retryable,
        })),
    )
        .into_response()
}

fn reply(out: Result<Value, Refused>) -> Response {
    match out {
        Ok(out) => Json(out).into_response(),
        Err(Refused::ForNow) => problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "OpenClaw is not connected",
            true,
        ),
        Err(Refused::Forever(detail)) => problem(StatusCode::UNPROCESSABLE_ENTITY, &detail, false),
    }
}

async fn notify_send(
    State(module): State<Arc<Module>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !from_core(&module, &headers) {
        return problem(StatusCode::UNAUTHORIZED, "bad or missing token", false);
    }
    reply(module.notify_send(&body).await)
}

async fn notify_update(
    State(module): State<Arc<Module>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !from_core(&module, &headers) {
        return problem(StatusCode::UNAUTHORIZED, "bad or missing token", false);
    }
    reply(module.notify_update(&body).await)
}

/// The Gateway's token as a private file: not on the command line.
fn token_file(token: &str) -> Result<PathBuf, String> {
    let path = std::env::temp_dir().join("openclaw-gateway-token");
    std::fs::write(&path, token).map_err(|e| format!("{}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(path)
}

fn module_from_env() -> Result<Module, String> {
    let config: Value = std::env::var("FROOD_MODULE_CONFIG")
        .ok()
        .filter(|c| !c.trim().is_empty())
        .map(|c| serde_json::from_str(&c).map_err(|e| format!("FROOD_MODULE_CONFIG: {e}")))
        .transpose()?
        .unwrap_or(Value::Null);
    let gateway = config["gateway"]
        .as_str()
        .ok_or(
            "config.gateway: the OpenClaw Gateway's address, e.g. ws://host.docker.internal:18789",
        )?
        .to_owned();
    let gateway_token =
        std::env::var("OPENCLAW_GATEWAY_TOKEN").map_err(|_| "OPENCLAW_GATEWAY_TOKEN is not set")?;
    Ok(Module {
        http: reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| e.to_string())?,
        frood: std::env::var("FROOD_URL")
            .map_err(|_| "FROOD_URL is not set")?
            .trim_end_matches('/')
            .to_owned(),
        token: std::env::var("FROOD_TOKEN").map_err(|_| "FROOD_TOKEN is not set")?,
        channel: std::env::var("FROOD_MODULE").unwrap_or_else(|_| "openclaw".to_owned()),
        words: Lang::from_code(config["language"].as_str().unwrap_or("en")).words(),
        launch: Launch {
            command: std::env::var("OPENCLAW_COMMAND").unwrap_or_else(|_| "openclaw".to_owned()),
            args: Vec::new(),
            gateway,
            token_file: token_file(&gateway_token)?,
        },
        retry_after: Duration::from_secs(15),
        peer: tokio::sync::RwLock::default(),
        waiting: Mutex::default(),
    })
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().json().init();
    let module = match module_from_env() {
        Ok(module) => Arc::new(module),
        Err(err) => {
            eprintln!("error: {err}");
            std::process::exit(2);
        }
    };
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port))
        .await
        .expect("bind");
    tracing::info!(port, channel = %module.channel, "notify-openclaw started");
    tokio::spawn(module.clone().run());
    axum::serve(listener, router(module))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .expect("serve");
}

#[cfg(test)]
mod tests;
