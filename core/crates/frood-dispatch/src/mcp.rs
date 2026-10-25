//! A small MCP client (Streamable HTTP) for checking a connection: initialize, then list the
//! server's tools. Agents talk to sources through their own clients; this one only asks.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};

const PROTOCOL: &str = "2025-06-18";
const LIMIT: Duration = Duration::from_secs(20);

/// A tool the server offers.
#[derive(Debug, Clone, Serialize)]
pub struct ToolInfo {
    pub name: String,
    pub description: Option<String>,
    /// The server says the tool only reads (`readOnlyHint`); none when it does not say.
    pub read_only: Option<bool>,
}

/// What a check found: the server's name and its tools.
#[derive(Debug, Clone, Serialize)]
pub struct Checked {
    pub server: Option<String>,
    pub tools: Vec<ToolInfo>,
}

/// Connects to `url` with these headers and lists the tools. Errors say what a person can fix:
/// the address, the token, the server.
pub async fn check(url: &str, headers: &BTreeMap<String, String>) -> Result<Checked, String> {
    let parsed = url::Url::parse(url).map_err(|_| format!("{url:?} is not an address"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(format!("{url:?} is not an http or https address"));
    }
    let client = reqwest::Client::builder()
        .timeout(LIMIT)
        .build()
        .map_err(|e| e.to_string())?;
    let mut session: Option<String> = None;
    let init = call(
        &client,
        url,
        headers,
        &mut session,
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": PROTOCOL, "capabilities": {},
            "clientInfo": { "name": "frood", "version": env!("CARGO_PKG_VERSION") } } }),
    )
    .await?;
    let server = init["serverInfo"]["name"].as_str().map(str::to_owned);
    notify(&client, url, headers, session.as_deref()).await;
    let mut tools = Vec::new();
    let mut cursor: Option<String> = None;
    for page in 2..20 {
        let mut params = json!({});
        if let Some(cursor) = &cursor {
            params["cursor"] = json!(cursor);
        }
        let listed = call(
            &client,
            url,
            headers,
            &mut session,
            json!({ "jsonrpc": "2.0", "id": page, "method": "tools/list", "params": params }),
        )
        .await?;
        for tool in listed["tools"].as_array().into_iter().flatten() {
            let Some(name) = tool["name"].as_str() else {
                continue;
            };
            tools.push(ToolInfo {
                name: name.to_owned(),
                description: tool["description"]
                    .as_str()
                    .map(|d| d.lines().next().unwrap_or(d).chars().take(200).collect()),
                read_only: tool["annotations"]["readOnlyHint"].as_bool(),
            });
        }
        cursor = listed["nextCursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    Ok(Checked { server, tools })
}

async fn call(
    client: &reqwest::Client,
    url: &str,
    headers: &BTreeMap<String, String>,
    session: &mut Option<String>,
    message: Value,
) -> Result<Value, String> {
    let id = message["id"].clone();
    let mut request = client
        .post(url)
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", PROTOCOL)
        .json(&message);
    for (name, value) in headers {
        request = request.header(name, value);
    }
    if let Some(session) = session.as_deref() {
        request = request.header("mcp-session-id", session);
    }
    let response = request
        .send()
        .await
        .map_err(|e| format!("the server did not answer: {}", e.without_url()))?;
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(format!(
            "the server refused the token ({status}); check it and its permissions"
        ));
    }
    if !status.is_success() {
        return Err(format!("the server answered {status}"));
    }
    if let Some(id) = response
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
    {
        *session = Some(id.to_owned());
    }
    let streamed = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|t| t.starts_with("text/event-stream"));
    let body = response
        .text()
        .await
        .map_err(|e| format!("the server's answer broke off: {}", e.without_url()))?;
    let answer = if streamed {
        answer_in_stream(&body, &id)
    } else {
        serde_json::from_str(&body).ok()
    }
    .ok_or_else(|| "the server's answer is not MCP".to_owned())?;
    if let Some(error) = answer.get("error") {
        return Err(format!(
            "the server said: {}",
            error["message"].as_str().unwrap_or("an error")
        ));
    }
    Ok(answer["result"].clone())
}

/// The answer with this id among the server-sent events.
fn answer_in_stream(body: &str, id: &Value) -> Option<Value> {
    body.split("\n\n")
        .filter_map(|event| {
            let data: Vec<&str> = event
                .lines()
                .filter_map(|l| l.strip_prefix("data:").map(str::trim_start))
                .collect();
            serde_json::from_str::<Value>(&data.join("\n")).ok()
        })
        .find(|message| message["id"] == *id)
}

async fn notify(
    client: &reqwest::Client,
    url: &str,
    headers: &BTreeMap<String, String>,
    session: Option<&str>,
) {
    let mut request = client
        .post(url)
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", PROTOCOL)
        .json(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
    for (name, value) in headers {
        request = request.header(name, value);
    }
    if let Some(session) = session {
        request = request.header("mcp-session-id", session);
    }
    let _ = request.send().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_are_found_among_events() {
        let body = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\n\
                    event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[]}}\n\n";
        assert_eq!(
            answer_in_stream(body, &json!(2)).unwrap()["result"]["tools"],
            json!([])
        );
        assert!(answer_in_stream(body, &json!(3)).is_none());
    }
}
