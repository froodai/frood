//! `openclaw mcp serve` as a child process: the OpenClaw Gateway's conversations over MCP on
//! stdio. Its event queue lives as long as the process, so a new process starts from cursor 0.

use std::path::PathBuf;

use rmcp::model::CallToolRequestParams;
use rmcp::service::{Peer, RunningService};
use rmcp::transport::TokioChildProcess;
use rmcp::{RoleClient, ServiceExt};
use serde_json::{Map, Value, json};
use tokio::process::Command;

/// How long one `events_wait` waits; the bridge allows up to 300 s.
pub(crate) const WAIT_MS: u64 = 25_000;

/// How to start the bridge.
#[derive(Clone)]
pub(crate) struct Launch {
    pub(crate) command: String,
    /// Arguments before the Gateway's: a test's script, say.
    pub(crate) args: Vec<String>,
    pub(crate) gateway: String,
    pub(crate) token_file: PathBuf,
}

pub(crate) type Service = RunningService<RoleClient, ()>;

impl Launch {
    pub(crate) async fn start(&self) -> Result<Service, String> {
        let mut command = Command::new(&self.command);
        command
            .args(&self.args)
            .args(["mcp", "serve", "--url", &self.gateway, "--token-file"])
            .arg(&self.token_file)
            // No Claude notifications: only the event queue is read here.
            .args(["--claude-channel-mode", "off"]);
        let transport = TokioChildProcess::new(command)
            .map_err(|e| format!("cannot start {}: {e}", self.command))?;
        ().serve(transport)
            .await
            .map_err(|e| format!("openclaw mcp serve did not start: {e}"))
    }
}

/// One tool call: its structured result, or what the bridge said went wrong.
async fn call(
    peer: &Peer<RoleClient>,
    tool: &'static str,
    arguments: Value,
) -> Result<Value, String> {
    let arguments: Map<String, Value> = match arguments {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    let result = peer
        .call_tool(CallToolRequestParams::new(tool).with_arguments(arguments))
        .await
        .map_err(|e| e.to_string())?;
    if result.is_error == Some(true) {
        let said = serde_json::to_value(&result.content)
            .ok()
            .and_then(|c| c[0]["text"].as_str().map(str::to_owned))
            .unwrap_or_else(|| format!("{tool} failed"));
        return Err(said);
    }
    Ok(result.structured_content.unwrap_or(Value::Null))
}

/// The next event after `cursor`, or nothing within the wait.
pub(crate) async fn next_event(peer: &Peer<RoleClient>, cursor: u64) -> Result<Next, String> {
    let out = call(
        peer,
        "events_wait",
        json!({ "after_cursor": cursor, "timeout_ms": WAIT_MS }),
    )
    .await?;
    if let Some(oldest) = out["gap"]["oldest_available_cursor"].as_u64() {
        return Ok(Next::Gap(oldest.saturating_sub(1)));
    }
    Ok(match out.get("event").filter(|e| !e.is_null()) {
        Some(event) => Next::Event(event.clone()),
        None => Next::Nothing,
    })
}

pub(crate) enum Next {
    Event(Value),
    /// Events were dropped from the queue: go on from this cursor.
    Gap(u64),
    Nothing,
}

/// Text through the route the conversation already has.
pub(crate) async fn send(
    peer: &Peer<RoleClient>,
    session_key: &str,
    text: &str,
) -> Result<(), String> {
    call(
        peer,
        "messages_send",
        json!({ "session_key": session_key, "text": text }),
    )
    .await
    .map(|_| ())
}
