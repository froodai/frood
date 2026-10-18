//! Built-in LLM gateway (docs/16 §1): `llm.complete@1` and `llm.embed@1` over the APIs of
//! Anthropic and OpenAI, paid by the accounts in `frood.yaml` (docs/13 §8).
//!
//! Calls are single requests with structured output: the caller gives a JSON Schema and gets
//! a value that matches it. Rust has no official SDK for either provider, so this speaks
//! their HTTP APIs directly.

use std::collections::BTreeMap;
use std::time::Duration;

use frood_config::{Account, ModelProvider};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Model used when a job names none.
pub const DEFAULT_ANTHROPIC_MODEL: &str = "claude-opus-5-5";
pub const DEFAULT_OPENAI_MODEL: &str = "gpt-6-astra";
pub const DEFAULT_EMBEDDING_MODEL: &str = "text-embedding-3-small";

const ANTHROPIC_URL: &str = "https://api.anthropic.com";
const OPENAI_URL: &str = "https://api.openai.com";
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
}

impl LlmError {
    /// Worth trying again later: rate limits, overload, server errors, network.
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Http { status, .. } => *status == 408 || *status == 429 || *status >= 500,
            Self::Network(_) => true,
            _ => false,
        }
    }
}

pub type Result<T> = std::result::Result<T, LlmError>;

/// One structured completion: instructions, the input, and the shape of the answer.
#[derive(Debug, Clone)]
pub struct CompleteRequest {
    pub model: Option<String>,
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

#[derive(Clone)]
struct Endpoint {
    provider: ModelProvider,
    key: String,
    base_url: String,
}

/// Calls models on behalf of jobs, by account name.
#[derive(Clone)]
pub struct Gateway {
    http: reqwest::Client,
    accounts: BTreeMap<String, Endpoint>,
}

impl Gateway {
    pub fn new(accounts: &BTreeMap<String, Account>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(600))
            .build()
            .expect("HTTP client builds");
        let accounts = accounts
            .iter()
            .map(|(name, account)| {
                let Account::ApiKey {
                    provider,
                    secret,
                    base_url,
                } = account;
                let default = match provider {
                    ModelProvider::Anthropic => ANTHROPIC_URL,
                    ModelProvider::Openai => OPENAI_URL,
                };
                let endpoint = Endpoint {
                    provider: *provider,
                    key: secret.clone(),
                    base_url: base_url
                        .as_deref()
                        .unwrap_or(default)
                        .trim_end_matches('/')
                        .to_owned(),
                };
                (name.clone(), endpoint)
            })
            .collect();
        Self { http, accounts }
    }

    /// The account a job uses when it names none: the only account, or the first Anthropic one.
    pub fn default_account(&self) -> Option<&str> {
        if self.accounts.len() == 1 {
            return self.accounts.keys().next().map(String::as_str);
        }
        self.accounts
            .iter()
            .find(|(_, e)| e.provider == ModelProvider::Anthropic)
            .map(|(name, _)| name.as_str())
    }

    pub fn has_account(&self, name: &str) -> bool {
        self.accounts.contains_key(name)
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

    async fn post(&self, request: reqwest::RequestBuilder, body: &Value) -> Result<Value> {
        let response = request
            .json(body)
            .send()
            .await
            .map_err(|e| LlmError::Network(e.to_string()))?;
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
        let model = request
            .model
            .clone()
            .unwrap_or_else(|| DEFAULT_ANTHROPIC_MODEL.to_owned());
        let mut output_config = json!({
            "format": { "type": "json_schema", "schema": request.schema },
        });
        // Claude Opus 5.5 defaults to medium effort; say so explicitly.
        let effort = request
            .effort
            .clone()
            .or_else(|| (model == DEFAULT_ANTHROPIC_MODEL).then(|| "medium".to_owned()));
        if let Some(effort) = effort {
            output_config["effort"] = json!(effort);
        }
        let mut body = json!({
            "model": model,
            "max_tokens": request.max_tokens,
            "system": request.system,
            "messages": [{ "role": "user", "content": request.user }],
            "output_config": output_config,
        });
        let mut call = self
            .http
            .post(format!("{}/v1/messages", endpoint.base_url))
            .header("x-api-key", &endpoint.key)
            .header("anthropic-version", "2023-06-01");
        if FALLBACK_MODELS.contains(&model.as_str()) {
            body["fallbacks"] = json!("default");
            call = call.header("anthropic-beta", FALLBACK_BETA);
        }
        let response: AnthropicResponse = serde_json::from_value(self.post(call, &body).await?)
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
        let model = request
            .model
            .clone()
            .unwrap_or_else(|| DEFAULT_OPENAI_MODEL.to_owned());
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
        let call = self
            .http
            .post(format!("{}/v1/responses", endpoint.base_url))
            .bearer_auth(&endpoint.key);
        let response: OpenAiResponse = serde_json::from_value(self.post(call, &body).await?)
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
    pub async fn embed(
        &self,
        account: &str,
        model: Option<&str>,
        inputs: &[String],
    ) -> Result<Embeddings> {
        let endpoint = self.endpoint(account)?;
        if endpoint.provider != ModelProvider::Openai {
            return Err(LlmError::NoEmbeddings(account.to_owned()));
        }
        let model = model.unwrap_or(DEFAULT_EMBEDDING_MODEL).to_owned();
        let body = json!({ "model": model, "input": inputs, "encoding_format": "float" });
        let call = self
            .http
            .post(format!("{}/v1/embeddings", endpoint.base_url))
            .bearer_auth(&endpoint.key);
        let response: OpenAiEmbeddings = serde_json::from_value(self.post(call, &body).await?)
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
            secret: "test-key".into(),
            base_url: Some(url.to_owned()),
        };
        Gateway::new(&BTreeMap::from([("main".to_owned(), account)]))
    }

    fn request() -> CompleteRequest {
        CompleteRequest {
            model: None,
            system: "Extract.".into(),
            user: "<source>hello</source>".into(),
            schema: json!({ "type": "object", "properties": { "ok": { "type": "boolean" } }, "required": ["ok"], "additionalProperties": false }),
            max_tokens: 16000,
            effort: None,
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
        assert_eq!(body["model"], DEFAULT_ANTHROPIC_MODEL);
        assert_eq!(body["fallbacks"], "default");
        assert_eq!(body["output_config"]["effort"], "medium");
        assert_eq!(body["output_config"]["format"]["type"], "json_schema");
        assert_eq!(body["messages"][0]["content"], "<source>hello</source>");
        assert!(body.get("thinking").is_none());
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
        haiku.model = Some("claude-haiku-5-5".into());
        let err = gateway(ModelProvider::Anthropic, &url)
            .complete("main", &haiku)
            .await
            .unwrap_err();
        assert!(matches!(err, LlmError::Truncated));
        // No server-side fallback for models that do not take it, and no implied effort.
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
        let done = gateway(ModelProvider::Openai, &url)
            .complete("main", &request())
            .await
            .unwrap();
        assert_eq!(done.value, json!({ "ok": false }));
        let (headers, body) = seen.lock().unwrap()[0].clone();
        assert_eq!(headers["authorization"], "Bearer test-key");
        assert_eq!(body["model"], DEFAULT_OPENAI_MODEL);
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
            .embed("main", None, &["a".into(), "b".into()])
            .await
            .unwrap();
        assert_eq!(out.vectors, vec![vec![0.0, 1.0], vec![1.0, 1.0]]);
        assert_eq!(seen.lock().unwrap()[0].1["model"], DEFAULT_EMBEDDING_MODEL);

        let anthropic = super::Gateway::new(&BTreeMap::from([(
            "a".to_owned(),
            Account::ApiKey {
                provider: ModelProvider::Anthropic,
                secret: "k".into(),
                base_url: None,
            },
        )]));
        assert!(matches!(
            anthropic.embed("a", None, &["x".into()]).await,
            Err(LlmError::NoEmbeddings(_))
        ));
    }

    #[test]
    fn default_account_prefers_the_only_one_then_anthropic() {
        let key = |provider| Account::ApiKey {
            provider,
            secret: "k".into(),
            base_url: None,
        };
        let one = Gateway::new(&BTreeMap::from([(
            "o".to_owned(),
            key(ModelProvider::Openai),
        )]));
        assert_eq!(one.default_account(), Some("o"));
        let two = Gateway::new(&BTreeMap::from([
            ("a-openai".to_owned(), key(ModelProvider::Openai)),
            ("b-claude".to_owned(), key(ModelProvider::Anthropic)),
        ]));
        assert_eq!(two.default_account(), Some("b-claude"));
        assert_eq!(Gateway::new(&BTreeMap::new()).default_account(), None);
    }
}
