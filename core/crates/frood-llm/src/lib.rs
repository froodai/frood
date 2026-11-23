//! Built-in LLM gateway (docs/16 §1): `llm.complete@1` and `llm.embed@1` over the APIs of
//! Anthropic and OpenAI, paid by the accounts in `frood.yaml` (docs/13 §8).
//!
//! Calls are single requests with structured output: the caller gives a JSON Schema and gets
//! a value that matches it. Rust has no official SDK for either provider, so this speaks
//! their HTTP APIs directly.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use frood_config::{Account, ModelProvider};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const ANTHROPIC_URL: &str = "https://api.anthropic.com";
const OPENAI_URL: &str = "https://api.openai.com";
/// How long a key printed by `api_key_helper` is used before the command runs again (Claude
/// Code's default for its apiKeyHelper); a 401 runs it again at once.
const HELPER_TTL: Duration = Duration::from_secs(5 * 60);
/// How long the helper may take.
const HELPER_TIMEOUT: Duration = Duration::from_secs(30);
/// Opts into server-side fallback when a safety classifier declines a request.
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
/// Models that take `fallbacks: "default"`.
const FALLBACK_MODELS: [&str; 4] = [
    "claude-fable-5-1",
    "claude-opus-5-5",
    "claude-opus-5",
    "claude-sonnet-5-5",
];

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("no account {0:?} in accounts")]
    UnknownAccount(String),
    #[error("account {0:?} cannot compute embeddings")]
    NoEmbeddings(String),
    #[error("the model declined the request{}", .0.as_deref().map(|c| format!(" ({c})")).unwrap_or_default())]
    Refused(Option<String>),
    #[error("the answer was cut off at the token limit")]
    Truncated,
    #[error("the provider answered HTTP {status}: {body}")]
    Http { status: u16, body: String },
    #[error("cannot reach the provider: {0}")]
    Network(String),
    #[error("unexpected answer from the provider: {0}")]
    Invalid(String),
    #[error("api_key_helper: {0}")]
    KeyHelper(String),
}

impl LlmError {
    /// Worth trying again later: rate limits, overload, server errors, network.
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Http { status, .. } => *status == 408 || *status == 429 || *status >= 500,
            // The helper's own login or network may be down for a while.
            Self::Network(_) | Self::KeyHelper(_) => true,
            _ => false,
        }
    }
}

pub type Result<T> = std::result::Result<T, LlmError>;

/// One structured completion: instructions, the input, and the shape of the answer.
#[derive(Debug, Clone)]
pub struct CompleteRequest {
    /// Always named in the config: Frood has no default model (D4).
    pub model: String,
    /// Instructions of the job.
    pub system: String,
    /// The input. Untrusted content goes here, quoted, never into `system`.
    pub user: String,
    /// JSON Schema of the answer: every object closed (`additionalProperties: false`) with
    /// all properties required, as both providers' strict modes want.
    pub schema: Value,
    pub max_tokens: u32,
    /// Anthropic `output_config.effort`: low, medium, high, xhigh, max.
    pub effort: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Completion {
    pub value: Value,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, Clone)]
pub struct Embeddings {
    pub model: String,
    pub vectors: Vec<Vec<f32>>,
}

/// An account's credentials, for a runtime module that runs an agent with them.
#[derive(Debug, Clone)]
pub struct Credential {
    pub provider: ModelProvider,
    /// A subscription's token works only inside the vendor's own agent, for its owner.
    pub subscription: bool,
    /// The API key, or the subscription's token; empty when `api_key_helper` gives the key.
    pub key: String,
    /// A command that prints the API key: the agent runs it itself (Claude Code's
    /// `apiKeyHelper`).
    pub api_key_helper: Option<String>,
    pub workspace: Option<String>,
    /// Another endpoint with the provider's API (a proxy or a company gateway), for an API key:
    /// the agent sends its requests there too.
    pub base_url: Option<String>,
    /// The subscriber, for a subscription.
    pub owner: Option<String>,
    pub monthly_usd: Option<f64>,
}

#[derive(Clone)]
struct Endpoint {
    provider: ModelProvider,
    key: Key,
    /// `anthropic-workspace-id` for organization-level Anthropic keys.
    workspace: Option<String>,
    base_url: String,
}

/// Where an endpoint's key comes from.
#[derive(Clone)]
enum Key {
    Fixed(String),
    /// A command's output, kept for `HELPER_TTL`.
    Helper {
        command: String,
        cached: Arc<tokio::sync::Mutex<Option<(String, Instant)>>>,
    },
}

impl Key {
    /// The key to send; `fresh` runs the helper again even if its last key is recent.
    async fn get(&self, fresh: bool) -> Result<String> {
        let (command, cached) = match self {
            Self::Fixed(key) => return Ok(key.clone()),
            Self::Helper { command, cached } => (command, cached),
        };
        let mut cached = cached.lock().await;
        if !fresh
            && let Some((key, at)) = cached.as_ref()
            && at.elapsed() < HELPER_TTL
        {
            return Ok(key.clone());
        }
        let key = run_helper(command).await?;
        *cached = Some((key.clone(), Instant::now()));
        Ok(key)
    }

    fn renewable(&self) -> bool {
        matches!(self, Self::Helper { .. })
    }
}

/// Runs `api_key_helper` with `sh -c`; its trimmed standard output is the key.
async fn run_helper(command: &str) -> Result<String> {
    let run = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(HELPER_TIMEOUT, run)
        .await
        .map_err(|_| LlmError::KeyHelper(format!("took longer than {HELPER_TIMEOUT:?}")))?
        .map_err(|e| LlmError::KeyHelper(format!("cannot run it: {e}")))?;
    if !output.status.success() {
        // Its error output, but never its standard output: that may hold a key.
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(LlmError::KeyHelper(format!(
            "exited with {}: {}",
            output.status,
            stderr.trim().chars().take(300).collect::<String>()
        )));
    }
    let key = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if key.is_empty() {
        return Err(LlmError::KeyHelper("printed no key".into()));
    }
    Ok(key)
}

/// Calls models on behalf of jobs, by account name.
#[derive(Clone)]
pub struct Gateway {
    http: reqwest::Client,
    /// Accounts the core may call models with directly: API keys.
    accounts: BTreeMap<String, Endpoint>,
    /// Every account, for agent runtimes.
    credentials: BTreeMap<String, Credential>,
}

impl Gateway {
    pub fn new(accounts: &BTreeMap<String, Account>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(600))
            .build()
            .expect("HTTP client builds");
        let mut endpoints = BTreeMap::new();
        let mut credentials = BTreeMap::new();
        for (name, account) in accounts {
            let credential = match account {
                Account::ApiKey {
                    provider,
                    secret,
                    api_key_helper,
                    base_url,
                    workspace,
                    budget,
                } => {
                    let default = match provider {
                        ModelProvider::Anthropic => ANTHROPIC_URL,
                        ModelProvider::Openai => OPENAI_URL,
                    };
                    endpoints.insert(
                        name.clone(),
                        Endpoint {
                            provider: *provider,
                            key: match api_key_helper {
                                Some(command) => Key::Helper {
                                    command: command.clone(),
                                    cached: Arc::default(),
                                },
                                None => Key::Fixed(secret.clone().unwrap_or_default()),
                            },
                            workspace: workspace.clone(),
                            base_url: base_url
                                .as_deref()
                                .unwrap_or(default)
                                .trim_end_matches('/')
                                .to_owned(),
                        },
                    );
                    Credential {
                        provider: *provider,
                        subscription: false,
                        key: secret.clone().unwrap_or_default(),
                        api_key_helper: api_key_helper.clone(),
                        workspace: workspace.clone(),
                        base_url: base_url
                            .as_deref()
                            .map(|url| url.trim_end_matches('/').to_owned()),
                        owner: None,
                        monthly_usd: budget.as_ref().map(|b| b.monthly_usd),
                    }
                }
                Account::Subscription {
                    provider,
                    owner,
                    secret,
                    budget,
                } => Credential {
                    provider: *provider,
                    subscription: true,
                    key: secret.clone(),
                    api_key_helper: None,
                    workspace: None,
                    base_url: None,
                    owner: Some(owner.clone()),
                    monthly_usd: budget.as_ref().map(|b| b.monthly_usd),
                },
            };
            credentials.insert(name.clone(), credential);
        }
        Self {
            http,
            accounts: endpoints,
            credentials,
        }
    }

    /// Accounts the core can call models with directly (API keys; subscriptions only work
    /// inside their vendor's agent).
    pub fn api_accounts(&self) -> Vec<String> {
        self.accounts.keys().cloned().collect()
    }

    /// What an agent runtime needs to pay with this account. Goes only to runtime modules.
    pub fn credential(&self, account: &str) -> Option<Credential> {
        self.credentials.get(account).cloned()
    }

    fn endpoint(&self, account: &str) -> Result<&Endpoint> {
        self.accounts
            .get(account)
            .ok_or_else(|| LlmError::UnknownAccount(account.to_owned()))
    }

    pub async fn complete(&self, account: &str, request: &CompleteRequest) -> Result<Completion> {
        let endpoint = self.endpoint(account)?;
        match endpoint.provider {
            ModelProvider::Anthropic => self.anthropic(endpoint, request).await,
            ModelProvider::Openai => self.openai(endpoint, request).await,
        }
    }

    /// Sends `body` with the endpoint's key; a key from `api_key_helper` that the provider
    /// rejects (401) is renewed once and the request sent again.
    async fn post(
        &self,
        endpoint: &Endpoint,
        request: impl Fn(&str) -> reqwest::RequestBuilder,
        body: &Value,
    ) -> Result<Value> {
        let key = endpoint.key.get(false).await?;
        let mut response = request(&key)
            .json(body)
            .send()
            .await
            .map_err(|e| LlmError::Network(e.to_string()))?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED && endpoint.key.renewable() {
            let key = endpoint.key.get(true).await?;
            response = request(&key)
                .json(body)
                .send()
                .await
                .map_err(|e| LlmError::Network(e.to_string()))?;
        }
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| LlmError::Network(e.to_string()))?;
        if !status.is_success() {
            return Err(LlmError::Http {
                status: status.as_u16(),
                body: text.chars().take(500).collect(),
            });
        }
        serde_json::from_str(&text).map_err(|e| LlmError::Invalid(e.to_string()))
    }

    async fn anthropic(
        &self,
        endpoint: &Endpoint,
        request: &CompleteRequest,
    ) -> Result<Completion> {
        let model = request.model.clone();
        let mut output_config = json!({
            "format": { "type": "json_schema", "schema": request.schema },
        });
        if let Some(effort) = &request.effort {
            output_config["effort"] = json!(effort);
        }
        let mut body = json!({
            "model": model,
            "max_tokens": request.max_tokens,
            "system": request.system,
            "messages": [{ "role": "user", "content": request.user }],
            "output_config": output_config,
        });
        let fallbacks = FALLBACK_MODELS.contains(&model.as_str());
        if fallbacks {
            body["fallbacks"] = json!("default");
        }
        let call = |key: &str| {
            let mut call = self
                .http
                .post(format!("{}/v1/messages", endpoint.base_url))
                .header("x-api-key", key)
                .header("anthropic-version", "2023-06-01");
            if let Some(workspace) = &endpoint.workspace {
                call = call.header("anthropic-workspace-id", workspace);
            }
            if fallbacks {
                call = call.header("anthropic-beta", FALLBACK_BETA);
            }
            call
        };
        let response: AnthropicResponse =
            serde_json::from_value(self.post(endpoint, call, &body).await?)
                .map_err(|e| LlmError::Invalid(e.to_string()))?;
        match response.stop_reason.as_deref() {
            Some("refusal") => {
                return Err(LlmError::Refused(
                    response.stop_details.and_then(|d| d.category),
                ));
            }
            Some("max_tokens") => return Err(LlmError::Truncated),
            _ => {}
        }
        let text: String = response
            .content
            .iter()
            .filter(|block| block.kind == "text")
            .filter_map(|block| block.text.as_deref())
            .collect();
        Ok(Completion {
            value: parse_json(&text)?,
            model: response.model,
            input_tokens: response.usage.input_tokens,
            output_tokens: response.usage.output_tokens,
        })
    }

    async fn openai(&self, endpoint: &Endpoint, request: &CompleteRequest) -> Result<Completion> {
        let model = request.model.clone();
        let body = json!({
            "model": model,
            "input": [
                { "role": "system", "content": request.system },
                { "role": "user", "content": request.user },
            ],
            "max_output_tokens": request.max_tokens,
            "text": { "format": {
                "type": "json_schema",
                "name": "result",
                "schema": request.schema,
                "strict": true,
            } },
        });
        let call = |key: &str| {
            self.http
                .post(format!("{}/v1/responses", endpoint.base_url))
                .bearer_auth(key)
        };
        let response: OpenAiResponse =
            serde_json::from_value(self.post(endpoint, call, &body).await?)
                .map_err(|e| LlmError::Invalid(e.to_string()))?;
        if response.status.as_deref() == Some("incomplete") {
            return Err(LlmError::Truncated);
        }
        let mut text = String::new();
        for item in response.output.iter().filter(|i| i.kind == "message") {
            for part in &item.content {
                match part.kind.as_str() {
                    "output_text" => text.push_str(part.text.as_deref().unwrap_or_default()),
                    "refusal" => return Err(LlmError::Refused(part.refusal.clone())),
                    _ => {}
                }
            }
        }
        let usage = response.usage.unwrap_or_default();
        Ok(Completion {
            value: parse_json(&text)?,
            model: response.model,
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
        })
    }

    /// Embeddings for search (`llm.embed@1`). Only OpenAI accounts have them.
    pub async fn embed(&self, account: &str, model: &str, inputs: &[String]) -> Result<Embeddings> {
        let endpoint = self.endpoint(account)?;
        if endpoint.provider != ModelProvider::Openai {
            return Err(LlmError::NoEmbeddings(account.to_owned()));
        }
        let model = model.to_owned();
        let body = json!({ "model": model, "input": inputs, "encoding_format": "float" });
        let call = |key: &str| {
            self.http
                .post(format!("{}/v1/embeddings", endpoint.base_url))
                .bearer_auth(key)
        };
        let response: OpenAiEmbeddings =
            serde_json::from_value(self.post(endpoint, call, &body).await?)
                .map_err(|e| LlmError::Invalid(e.to_string()))?;
        let mut data = response.data;
        data.sort_by_key(|d| d.index);
        if data.len() != inputs.len() {
            return Err(LlmError::Invalid(format!(
                "{} embeddings for {} inputs",
                data.len(),
                inputs.len()
            )));
        }
        Ok(Embeddings {
            model,
            vectors: data.into_iter().map(|d| d.embedding).collect(),
        })
    }
}

fn parse_json(text: &str) -> Result<Value> {
    serde_json::from_str(text.trim())
        .map_err(|e| LlmError::Invalid(format!("the answer is not the requested JSON: {e}")))
}

#[derive(Deserialize)]
struct AnthropicResponse {
    model: String,
    #[serde(default)]
    content: Vec<AnthropicBlock>,
    stop_reason: Option<String>,
    #[serde(default)]
    stop_details: Option<StopDetails>,
    usage: AnthropicUsage,
}

#[derive(Deserialize)]
struct AnthropicBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Deserialize)]
struct StopDetails {
    #[serde(default)]
    category: Option<String>,
}

#[derive(Deserialize)]
struct AnthropicUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
}

#[derive(Deserialize)]
struct OpenAiResponse {
    model: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    output: Vec<OpenAiItem>,
    #[serde(default)]
    usage: Option<OpenAiUsage>,
}

#[derive(Deserialize)]
struct OpenAiItem {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    content: Vec<OpenAiPart>,
}

#[derive(Deserialize)]
struct OpenAiPart {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    refusal: Option<String>,
}

#[derive(Deserialize, Default)]
struct OpenAiUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
}

#[derive(Deserialize)]
struct OpenAiEmbeddings {
    data: Vec<OpenAiEmbedding>,
}

#[derive(Deserialize)]
struct OpenAiEmbedding {
    index: usize,
    embedding: Vec<f32>,
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::extract::State;
    use axum::http::HeaderMap;
    use axum::routing::post;
    use axum::{Json, Router};

    use super::*;

    type Seen = Arc<Mutex<Vec<(HeaderMap, Value)>>>;

    /// A fake provider that records requests and answers with `reply(body)`.
    async fn provider(reply: fn(&Value) -> Value) -> (String, Seen) {
        let seen: Seen = Arc::default();
        let handler = move |State(seen): State<Seen>,
                            headers: HeaderMap,
                            Json(body): Json<Value>| async move {
            let answer = reply(&body);
            seen.lock().unwrap().push((headers, body));
            Json(answer)
        };
        let app = Router::new()
            .route("/v1/messages", post(handler))
            .route("/v1/responses", post(handler))
            .route("/v1/embeddings", post(handler))
            .with_state(seen.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (url, seen)
    }

    fn gateway(provider: ModelProvider, url: &str) -> Gateway {
        let account = Account::ApiKey {
            provider,
            secret: Some("test-key".into()),
            api_key_helper: None,
            base_url: Some(url.to_owned()),
            workspace: None,
            budget: None,
        };
        Gateway::new(&BTreeMap::from([("main".to_owned(), account)]))
    }

    fn request() -> CompleteRequest {
        CompleteRequest {
            model: "claude-opus-5-5".into(),
            system: "Extract.".into(),
            user: "<source>hello</source>".into(),
            schema: json!({ "type": "object", "properties": { "ok": { "type": "boolean" } }, "required": ["ok"], "additionalProperties": false }),
            max_tokens: 16000,
            effort: Some("medium".into()),
        }
    }

    #[tokio::test]
    async fn anthropic_structured_completion() {
        let (url, seen) = provider(|_| {
            json!({
                "model": "claude-opus-5-5",
                "content": [{ "type": "thinking", "thinking": "" }, { "type": "text", "text": "{\"ok\": true}" }],
                "stop_reason": "end_turn",
                "usage": { "input_tokens": 12, "output_tokens": 5 }
            })
        })
        .await;
        let done = gateway(ModelProvider::Anthropic, &url)
            .complete("main", &request())
            .await
            .unwrap();
        assert_eq!(done.value, json!({ "ok": true }));
        assert_eq!(done.input_tokens, 12);

        let (headers, body) = seen.lock().unwrap()[0].clone();
        assert_eq!(headers["x-api-key"], "test-key");
        assert_eq!(headers["anthropic-version"], "2023-06-01");
        assert_eq!(headers["anthropic-beta"], FALLBACK_BETA);
        assert_eq!(body["model"], "claude-opus-5-5");
        assert_eq!(body["fallbacks"], "default");
        assert_eq!(body["output_config"]["effort"], "medium");
        assert_eq!(body["output_config"]["format"]["type"], "json_schema");
        assert_eq!(body["messages"][0]["content"], "<source>hello</source>");
        assert!(body.get("thinking").is_none());
    }

    #[tokio::test]
    async fn organization_keys_name_their_workspace() {
        let (url, seen) = provider(|_| {
            json!({ "model": "claude-haiku-5-5", "content": [{ "type": "text", "text": "{\"ok\": true}" }],
                    "stop_reason": "end_turn", "usage": {} })
        })
        .await;
        let account = Account::ApiKey {
            provider: ModelProvider::Anthropic,
            secret: Some("org-key".into()),
            api_key_helper: None,
            base_url: Some(url),
            workspace: Some("wrkspc_01abc".into()),
            budget: None,
        };
        let gateway = Gateway::new(&BTreeMap::from([("main".to_owned(), account)]));
        gateway.complete("main", &request()).await.unwrap();
        assert_eq!(
            seen.lock().unwrap()[0].0["anthropic-workspace-id"],
            "wrkspc_01abc"
        );
    }

    #[tokio::test]
    async fn anthropic_refusal_and_cut_off() {
        let (url, _) = provider(|_| {
            json!({ "model": "claude-opus-5-5", "content": [], "stop_reason": "refusal",
                    "stop_details": { "type": "refusal", "category": "cyber" }, "usage": {} })
        })
        .await;
        let err = gateway(ModelProvider::Anthropic, &url)
            .complete("main", &request())
            .await
            .unwrap_err();
        assert!(
            matches!(err, LlmError::Refused(Some(ref c)) if c == "cyber"),
            "{err}"
        );

        let (url, seen) = provider(|_| {
            json!({ "model": "claude-haiku-5-5", "content": [{ "type": "text", "text": "{\"ok\"" }],
                    "stop_reason": "max_tokens", "usage": {} })
        })
        .await;
        let mut haiku = request();
        haiku.model = "claude-haiku-5-5".into();
        haiku.effort = None;
        let err = gateway(ModelProvider::Anthropic, &url)
            .complete("main", &haiku)
            .await
            .unwrap_err();
        assert!(matches!(err, LlmError::Truncated));
        // No server-side fallback for models that do not take it; effort only when set.
        let (headers, body) = seen.lock().unwrap()[0].clone();
        assert!(headers.get("anthropic-beta").is_none());
        assert!(body.get("fallbacks").is_none());
        assert!(body["output_config"].get("effort").is_none());
    }

    #[tokio::test]
    async fn openai_structured_completion_and_refusal() {
        let (url, seen) = provider(|_| {
            json!({
                "model": "gpt-6-astra", "status": "completed",
                "output": [{ "type": "message", "content": [{ "type": "output_text", "text": "{\"ok\": false}" }] }],
                "usage": { "input_tokens": 3, "output_tokens": 4 }
            })
        })
        .await;
        let mut gpt = request();
        gpt.model = "gpt-6-luna".into();
        let done = gateway(ModelProvider::Openai, &url)
            .complete("main", &gpt)
            .await
            .unwrap();
        assert_eq!(done.value, json!({ "ok": false }));
        let (headers, body) = seen.lock().unwrap()[0].clone();
        assert_eq!(headers["authorization"], "Bearer test-key");
        assert_eq!(body["model"], "gpt-6-luna");
        assert_eq!(body["text"]["format"]["strict"], true);
        assert_eq!(body["input"][0]["role"], "system");

        let (url, _) = provider(|_| {
            json!({ "model": "gpt-6-astra", "status": "completed",
                    "output": [{ "type": "message", "content": [{ "type": "refusal", "refusal": "no" }] }] })
        })
        .await;
        let err = gateway(ModelProvider::Openai, &url)
            .complete("main", &request())
            .await
            .unwrap_err();
        assert!(matches!(err, LlmError::Refused(_)));
    }

    #[tokio::test]
    async fn embeddings_keep_input_order() {
        let (url, seen) = provider(|body| {
            let n = body["input"].as_array().unwrap().len();
            let data: Vec<Value> = (0..n)
                .rev()
                .map(|i| json!({ "object": "embedding", "index": i, "embedding": [i as f32, 1.0] }))
                .collect();
            json!({ "object": "list", "data": data, "model": "text-embedding-3-small" })
        })
        .await;
        let gateway = gateway(ModelProvider::Openai, &url);
        let out = gateway
            .embed("main", "text-embedding-3-small", &["a".into(), "b".into()])
            .await
            .unwrap();
        assert_eq!(out.vectors, vec![vec![0.0, 1.0], vec![1.0, 1.0]]);
        assert_eq!(seen.lock().unwrap()[0].1["model"], "text-embedding-3-small");

        let anthropic = super::Gateway::new(&BTreeMap::from([(
            "a".to_owned(),
            Account::ApiKey {
                provider: ModelProvider::Anthropic,
                secret: Some("k".into()),
                api_key_helper: None,
                base_url: None,
                workspace: None,
                budget: None,
            },
        )]));
        assert!(matches!(
            anthropic.embed("a", "m", &["x".into()]).await,
            Err(LlmError::NoEmbeddings(_))
        ));
    }

    #[tokio::test]
    async fn a_key_helper_is_run_again_when_its_key_is_rejected() {
        // A gateway that takes only the second key the helper prints.
        let seen: Arc<Mutex<Vec<String>>> = Arc::default();
        let handler = |State(seen): State<Arc<Mutex<Vec<String>>>>, headers: HeaderMap| async move {
            let key = headers["x-api-key"].to_str().unwrap().to_owned();
            seen.lock().unwrap().push(key.clone());
            if key != "key-2" {
                return (
                    axum::http::StatusCode::UNAUTHORIZED,
                    Json(json!({ "error": "expired" })),
                );
            }
            (
                axum::http::StatusCode::OK,
                Json(
                    json!({ "model": "claude-haiku-5-5", "content": [{ "type": "text", "text": "{\"ok\": true}" }],
                             "stop_reason": "end_turn", "usage": {} }),
                ),
            )
        };
        let app = Router::new()
            .route("/gw/anthropic/v1/messages", post(handler))
            .with_state(seen.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/gw/anthropic", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let counter = std::env::temp_dir().join(format!("frood-key-helper-{}", std::process::id()));
        let _ = std::fs::remove_file(&counter);
        let command = format!(
            "n=$(cat '{0}' 2>/dev/null || echo 0); n=$((n+1)); echo $n > '{0}'; echo \"  key-$n \"",
            counter.display()
        );
        let account = Account::ApiKey {
            provider: ModelProvider::Anthropic,
            secret: None,
            api_key_helper: Some(command.clone()),
            base_url: Some(url),
            workspace: None,
            budget: None,
        };
        let gateway = Gateway::new(&BTreeMap::from([("work".to_owned(), account)]));
        let mut haiku = request();
        haiku.model = "claude-haiku-5-5".into();
        gateway.complete("work", &haiku).await.unwrap();
        // The next call keeps the renewed key: the helper is not run for every request.
        gateway.complete("work", &haiku).await.unwrap();
        assert_eq!(*seen.lock().unwrap(), ["key-1", "key-2", "key-2"]);
        assert_eq!(std::fs::read_to_string(&counter).unwrap().trim(), "2");
        let _ = std::fs::remove_file(&counter);

        // The agent gets the command, not a key.
        let credential = gateway.credential("work").unwrap();
        assert_eq!(credential.key, "");
        assert_eq!(credential.api_key_helper.as_deref(), Some(command.as_str()));

        // A helper that fails says so, without what it printed.
        let broken = Gateway::new(&BTreeMap::from([(
            "work".to_owned(),
            Account::ApiKey {
                provider: ModelProvider::Anthropic,
                secret: None,
                api_key_helper: Some("echo secret-ish; echo 'not logged in' >&2; exit 3".into()),
                base_url: Some("http://127.0.0.1:9".into()),
                workspace: None,
                budget: None,
            },
        )]));
        let err = broken.complete("work", &haiku).await.unwrap_err();
        let text = err.to_string();
        assert!(matches!(err, LlmError::KeyHelper(_)), "{text}");
        assert!(
            text.contains("not logged in") && !text.contains("secret-ish"),
            "{text}"
        );
    }

    #[test]
    fn subscriptions_are_for_runtimes_only() {
        let gateway = Gateway::new(&BTreeMap::from([
            (
                "api".to_owned(),
                Account::ApiKey {
                    provider: ModelProvider::Anthropic,
                    secret: Some("k".into()),
                    api_key_helper: None,
                    base_url: Some("https://llm-gateway.example.com/anthropic/".into()),
                    workspace: None,
                    budget: None,
                },
            ),
            (
                "max".to_owned(),
                Account::Subscription {
                    provider: ModelProvider::Anthropic,
                    owner: "alice@example.com".into(),
                    secret: "oauth".into(),
                    budget: Some(frood_config::AccountBudget { monthly_usd: 20.0 }),
                },
            ),
        ]));
        assert_eq!(gateway.api_accounts(), ["api"]);
        let api = gateway.credential("api").unwrap();
        assert!(!api.subscription);
        assert_eq!(
            api.base_url.as_deref(),
            Some("https://llm-gateway.example.com/anthropic"),
            "the agent goes to the same gateway as direct calls"
        );
        let max = gateway.credential("max").unwrap();
        assert!(max.subscription);
        assert_eq!(max.base_url, None);
        assert_eq!(max.owner.as_deref(), Some("alice@example.com"));
        assert_eq!(max.monthly_usd, Some(20.0));
    }
}
