//! The agents this runtime runs: the command line of one turn and how to read what it prints.
//!
//! - `claude-code`: Claude Code in print mode, `--output-format stream-json`. The session id is
//!   the run id (`--session-id` on the first turn, `--resume` after); only the MCP servers of the
//!   run (`--strict-mcp-config`), no built-in tools unless the settings allow some, permission
//!   mode `dontAsk` with the run's MCP servers allowed.
//! - `codex`: Codex `exec --json`; the session is the thread id it prints first, continued with
//!   `exec resume`. Codex reports tokens but not dollars, so its runs count as $0 and only the
//!   time budget limits them.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, Clone, Deserialize)]
pub struct McpServer {
    pub name: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Credentials {
    pub provider: String,
    pub kind: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub token: Option<String>,
}

/// One turn of an agent: the first one, or a resumed one.
pub struct Turn<'a> {
    pub agent: &'a str,
    pub model: &'a str,
    pub instructions: &'a str,
    /// The first prompt, or the message the session continues with.
    pub prompt: &'a str,
    /// For a first turn of Claude Code: the session id to create.
    pub new_session: Option<&'a str>,
    /// The session to continue.
    pub resume: Option<&'a str>,
    pub mcp_servers: &'a [McpServer],
    pub frood_url: &'a str,
    pub credentials: &'a Credentials,
    pub max_budget_usd: Option<f64>,
    /// Claude Code's built-in tools the agent may use, e.g. `Read,Grep`; empty for none.
    pub builtin_tools: &'a str,
}

/// Arguments for the image's entrypoint (the agent program) and its environment.
pub struct Command {
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

fn server_url(server: &McpServer, frood_url: &str) -> Result<String, String> {
    match (&server.url, &server.path) {
        (Some(url), _) => Ok(url.clone()),
        (None, Some(path)) => Ok(format!("{}{path}", frood_url.trim_end_matches('/'))),
        (None, None) => Err(format!(
            "MCP server {} has neither url nor path",
            server.name
        )),
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

pub fn command(turn: &Turn<'_>) -> Result<Command, String> {
    for server in turn.mcp_servers {
        if !valid_name(&server.name) {
            return Err(format!(
                "MCP server name {:?} must be letters, digits, _ or -",
                server.name
            ));
        }
    }
    match turn.agent {
        "claude-code" => claude_code(turn),
        "codex" => codex(turn),
        other => Err(format!("this runtime has no agent {other}")),
    }
}

fn claude_code(turn: &Turn<'_>) -> Result<Command, String> {
    let creds = turn.credentials;
    if creds.provider != "anthropic" {
        return Err(format!(
            "claude-code pays with an Anthropic account, not {}",
            creds.provider
        ));
    }
    let mut env = vec![
        // No auto-updates, telemetry or error reports from inside a run.
        (
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".to_owned(),
            "1".to_owned(),
        ),
    ];
    match creds.kind.as_str() {
        "api_key" => {
            let key = creds.api_key.clone().ok_or("credentials have no api_key")?;
            env.push(("ANTHROPIC_API_KEY".into(), key));
            if let Some(workspace) = &creds.workspace {
                env.push((
                    "ANTHROPIC_CUSTOM_HEADERS".into(),
                    format!("anthropic-workspace-id: {workspace}"),
                ));
            }
        }
        "subscription" => {
            let token = creds.token.clone().ok_or("credentials have no token")?;
            env.push(("CLAUDE_CODE_OAUTH_TOKEN".into(), token));
        }
        other => return Err(format!("unknown credentials kind {other}")),
    }
    let mut servers = serde_json::Map::new();
    for server in turn.mcp_servers {
        servers.insert(
            server.name.clone(),
            json!({ "type": "http", "url": server_url(server, turn.frood_url)?, "headers": server.headers }),
        );
    }
    // The prompt comes right after -p, before the variadic --allowedTools at the end.
    let mut args: Vec<String> = vec!["-p".into(), turn.prompt.into()];
    args.extend(
        [
            "--output-format",
            "stream-json",
            "--verbose",
            "--model",
            turn.model,
            "--append-system-prompt",
            turn.instructions,
            "--strict-mcp-config",
            "--permission-mode",
            "dontAsk",
            "--tools",
            turn.builtin_tools,
        ]
        .map(str::to_owned),
    );
    args.push("--mcp-config".into());
    args.push(json!({ "mcpServers": servers }).to_string());
    match (turn.resume, turn.new_session) {
        (Some(session), _) => args.extend(["--resume".into(), session.to_owned()]),
        (None, Some(session)) => args.extend(["--session-id".into(), session.to_owned()]),
        (None, None) => {}
    }
    if let Some(budget) = turn.max_budget_usd {
        args.extend(["--max-budget-usd".into(), format!("{budget:.4}")]);
    }
    args.push("--allowedTools".into());
    args.extend(turn.mcp_servers.iter().map(|s| format!("mcp__{}", s.name)));
    Ok(Command { args, env })
}

/// A TOML basic string.
fn toml_string(value: &str) -> String {
    serde_json::to_string(value).expect("strings serialize")
}

fn codex(turn: &Turn<'_>) -> Result<Command, String> {
    let creds = turn.credentials;
    if creds.provider != "openai" {
        return Err(format!(
            "codex pays with an OpenAI account, not {}",
            creds.provider
        ));
    }
    let env = match creds.kind.as_str() {
        "api_key" => vec![(
            "CODEX_API_KEY".to_owned(),
            creds.api_key.clone().ok_or("credentials have no api_key")?,
        )],
        // The image's entrypoint writes it to ~/.codex/auth.json.
        "subscription" => vec![(
            "CODEX_AUTH_JSON".to_owned(),
            creds.token.clone().ok_or("credentials have no token")?,
        )],
        other => return Err(format!("unknown credentials kind {other}")),
    };
    let mut args: Vec<String> = [
        "exec",
        "--json",
        "--skip-git-repo-check",
        "--sandbox",
        "read-only",
        "--model",
        turn.model,
    ]
    .map(str::to_owned)
    .to_vec();
    for server in turn.mcp_servers {
        let key = format!("mcp_servers.{}", server.name);
        let headers: Vec<String> = server
            .headers
            .iter()
            .map(|(k, v)| format!("{} = {}", toml_string(k), toml_string(v)))
            .collect();
        args.extend([
            "-c".into(),
            format!(
                "{key}.url={}",
                toml_string(&server_url(server, turn.frood_url)?)
            ),
            "-c".into(),
            format!("{key}.http_headers={{ {} }}", headers.join(", ")),
            "-c".into(),
            format!("{key}.default_tools_approval_mode=\"approve\""),
        ]);
    }
    match turn.resume {
        Some(session) => args.extend(["resume".into(), session.into(), turn.prompt.into()]),
        // Codex has no flag for extra system instructions: they open the first message.
        None => args.push(format!("{}\n\n---\n\n{}", turn.instructions, turn.prompt)),
    }
    Ok(Command { args, env })
}

/// What a line of the agent's output means.
#[derive(Debug, PartialEq)]
pub enum Parsed {
    /// An event for the run's log: `{kind, data}`.
    Event(Value),
    /// The agent's session id.
    Session(String),
    /// A message of the agent; the last one is the run's result unless the agent says otherwise.
    Answer(String),
    /// The end of the turn as the agent reports it.
    Done {
        result: Option<String>,
        cost_usd: f64,
        error: Option<String>,
    },
}

fn event(kind: &str, data: Value) -> Parsed {
    Parsed::Event(json!({ "kind": kind, "data": data }))
}

fn short(text: &str) -> String {
    const MAX: usize = 2000;
    if text.chars().count() <= MAX {
        return text.to_owned();
    }
    let cut: String = text.chars().take(MAX).collect();
    format!("{cut}…")
}

pub fn parse_line(agent: &str, line: &str) -> Vec<Parsed> {
    let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
        return Vec::new();
    };
    match agent {
        "claude-code" => parse_claude(&value),
        "codex" => parse_codex(&value),
        _ => Vec::new(),
    }
}

fn parse_claude(v: &Value) -> Vec<Parsed> {
    let mut out = Vec::new();
    let content = || {
        v.pointer("/message/content")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    match v["type"].as_str() {
        Some("system") if v["subtype"] == "init" => {
            if let Some(session) = v["session_id"].as_str() {
                out.push(Parsed::Session(session.to_owned()));
            }
            let servers: Vec<Value> = v["mcp_servers"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|s| json!({ "name": s["name"], "status": s["status"] }))
                .collect();
            out.push(event(
                "init",
                json!({ "model": v["model"], "mcp_servers": servers }),
            ));
        }
        Some("assistant") => {
            for block in content() {
                match block["type"].as_str() {
                    Some("text") => {
                        let text = block["text"].as_str().unwrap_or_default();
                        if !text.trim().is_empty() {
                            out.push(event("assistant", json!({ "text": short(text) })));
                            out.push(Parsed::Answer(text.to_owned()));
                        }
                    }
                    Some("tool_use") => {
                        out.push(event("tool_use", json!({ "name": block["name"] })))
                    }
                    _ => {}
                }
            }
        }
        Some("user") => {
            for block in content() {
                if block["type"] == "tool_result" {
                    out.push(event(
                        "tool_result",
                        json!({ "is_error": block["is_error"].as_bool().unwrap_or(false) }),
                    ));
                }
            }
        }
        Some("result") => {
            let cost = v["total_cost_usd"].as_f64().unwrap_or(0.0);
            out.push(event(
                "usage",
                json!({
                    "cost_usd": cost,
                    "num_turns": v["num_turns"],
                    "duration_ms": v["duration_ms"],
                    "input_tokens": v.pointer("/usage/input_tokens"),
                    "output_tokens": v.pointer("/usage/output_tokens"),
                }),
            ));
            let failed = v["is_error"].as_bool().unwrap_or(false);
            let result = v["result"].as_str().map(str::to_owned);
            out.push(Parsed::Done {
                error: failed.then(|| {
                    result.clone().unwrap_or_else(|| {
                        v["subtype"]
                            .as_str()
                            .unwrap_or("the agent failed")
                            .to_owned()
                    })
                }),
                result: if failed { None } else { result },
                cost_usd: cost,
            });
        }
        _ => {}
    }
    out
}

fn parse_codex(v: &Value) -> Vec<Parsed> {
    let mut out = Vec::new();
    match v["type"].as_str() {
        Some("thread.started") => {
            if let Some(thread) = v["thread_id"].as_str() {
                out.push(Parsed::Session(thread.to_owned()));
            }
            out.push(event("init", json!({ "thread_id": v["thread_id"] })));
        }
        Some("item.completed") => {
            let item = &v["item"];
            match item["type"].as_str() {
                Some("agent_message") => {
                    let text = item["text"].as_str().unwrap_or_default();
                    out.push(event("assistant", json!({ "text": short(text) })));
                    out.push(Parsed::Answer(text.to_owned()));
                }
                Some("mcp_tool_call") => out.push(event(
                    "tool_use",
                    json!({ "name": format!("{}/{}", item["server"].as_str().unwrap_or("?"), item["tool"].as_str().unwrap_or("?")), "status": item["status"] }),
                )),
                Some("error") => out.push(event("error", json!({ "message": item["message"] }))),
                _ => {}
            }
        }
        Some("turn.completed") => out.push(event("usage", v["usage"].clone())),
        Some("turn.failed") => {
            let message = v
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("the turn failed")
                .to_owned();
            out.push(event("error", json!({ "message": message })));
            out.push(Parsed::Done {
                result: None,
                cost_usd: 0.0,
                error: Some(message),
            });
        }
        Some("error") => out.push(event("error", json!({ "message": v["message"] }))),
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn creds(provider: &str, kind: &str) -> Credentials {
        Credentials {
            provider: provider.into(),
            kind: kind.into(),
            api_key: Some("sk-test".into()),
            workspace: Some("wrkspc_01".into()),
            token: Some("oauth".into()),
        }
    }

    fn servers() -> Vec<McpServer> {
        vec![
            McpServer {
                name: "frood".into(),
                path: Some("/mcp".into()),
                url: None,
                headers: BTreeMap::from([("Authorization".into(), "Bearer frood_run_x".into())]),
            },
            McpServer {
                name: "source-team-chat".into(),
                path: None,
                url: Some("http://team-chat:8080/mcp".into()),
                headers: BTreeMap::new(),
            },
        ]
    }

    fn turn<'a>(agent: &'a str, servers: &'a [McpServer], creds: &'a Credentials) -> Turn<'a> {
        Turn {
            agent,
            model: "claude-haiku-5-5",
            instructions: "Sweep.",
            prompt: "Go",
            new_session: Some("0199-run"),
            resume: None,
            mcp_servers: servers,
            frood_url: "http://frood:8080/",
            credentials: creds,
            max_budget_usd: Some(0.5),
            builtin_tools: "",
        }
    }

    #[test]
    fn claude_code_gets_only_the_run_servers_and_its_key() {
        let servers = servers();
        let key = creds("anthropic", "api_key");
        let cmd = command(&turn("claude-code", &servers, &key)).unwrap();
        assert_eq!(&cmd.args[..2], ["-p", "Go"]);
        let at = |flag: &str| {
            let i = cmd.args.iter().position(|a| a == flag).unwrap();
            cmd.args[i + 1].clone()
        };
        let mcp: Value = serde_json::from_str(&at("--mcp-config")).unwrap();
        assert_eq!(mcp["mcpServers"]["frood"]["url"], "http://frood:8080/mcp");
        assert_eq!(
            mcp["mcpServers"]["frood"]["headers"]["Authorization"],
            "Bearer frood_run_x"
        );
        assert_eq!(at("--session-id"), "0199-run");
        assert_eq!(at("--max-budget-usd"), "0.5000");
        assert_eq!(at("--tools"), "");
        assert!(cmd.args.contains(&"--strict-mcp-config".to_owned()));
        assert_eq!(
            cmd.args[cmd.args.len() - 3..],
            ["--allowedTools", "mcp__frood", "mcp__source-team-chat"]
        );
        assert!(
            cmd.env
                .contains(&("ANTHROPIC_API_KEY".into(), "sk-test".into()))
        );
        assert!(cmd.env.contains(&(
            "ANTHROPIC_CUSTOM_HEADERS".into(),
            "anthropic-workspace-id: wrkspc_01".into()
        )));

        let sub = creds("anthropic", "subscription");
        let mut resumed = turn("claude-code", &servers, &sub);
        resumed.resume = Some("0199-run");
        let cmd = command(&resumed).unwrap();
        assert!(cmd.args.windows(2).any(|w| w == ["--resume", "0199-run"]));
        assert!(!cmd.args.contains(&"--session-id".to_owned()));
        assert!(
            cmd.env
                .contains(&("CLAUDE_CODE_OAUTH_TOKEN".into(), "oauth".into()))
        );
        assert!(!cmd.env.iter().any(|(k, _)| k == "ANTHROPIC_API_KEY"));

        let openai = creds("openai", "api_key");
        assert!(command(&turn("claude-code", &servers, &openai)).is_err());
    }

    #[test]
    fn codex_gets_servers_as_config_overrides() {
        let servers = servers();
        let key = creds("openai", "api_key");
        let cmd = command(&turn("codex", &servers, &key)).unwrap();
        assert_eq!(&cmd.args[..2], ["exec", "--json"]);
        assert!(
            cmd.args
                .contains(&"mcp_servers.frood.url=\"http://frood:8080/mcp\"".to_owned())
        );
        assert!(
            cmd.args.contains(
                &"mcp_servers.frood.http_headers={ \"Authorization\" = \"Bearer frood_run_x\" }"
                    .to_owned()
            )
        );
        assert!(cmd.args.last().unwrap().starts_with("Sweep.\n\n---\n\nGo"));
        assert_eq!(
            cmd.env,
            [("CODEX_API_KEY".to_owned(), "sk-test".to_owned())]
        );
    }

    #[test]
    fn claude_stream_json_becomes_events_and_a_result() {
        let lines = [
            r#"{"type":"system","subtype":"init","session_id":"s1","model":"claude-haiku-5-5","mcp_servers":[{"name":"frood","status":"connected"}]}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Reading"},{"type":"tool_use","name":"mcp__frood__ask_user","input":{}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t","content":"ok"}]}}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"result":"Asked one question","total_cost_usd":0.0123,"num_turns":2,"duration_ms":900,"usage":{"input_tokens":10,"output_tokens":5}}"#,
            "not json",
        ];
        let parsed: Vec<Parsed> = lines
            .iter()
            .flat_map(|l| parse_line("claude-code", l))
            .collect();
        assert_eq!(parsed[0], Parsed::Session("s1".into()));
        assert!(
            matches!(&parsed[1], Parsed::Event(e) if e["data"]["mcp_servers"][0]["status"] == "connected")
        );
        assert!(
            matches!(&parsed[4], Parsed::Event(e) if e["kind"] == "tool_use" && e["data"]["name"] == "mcp__frood__ask_user")
        );
        assert_eq!(
            parsed.last().unwrap(),
            &Parsed::Done {
                result: Some("Asked one question".into()),
                cost_usd: 0.0123,
                error: None
            }
        );
    }
}
