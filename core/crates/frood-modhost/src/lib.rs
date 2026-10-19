//! Module host (docs/16 §2): container modules from `frood.yaml`, their manifests and the
//! capabilities they provide.
//!
//! Module protocol, HTTP on the module's port (8080 in its container):
//! - `GET /manifest` — the module's manifest (`frood-module.yaml` as JSON);
//! - `GET /healthz` — 200 when ready;
//! - `POST /v1/capabilities/{capability}` — call a capability, e.g. `media.transcribe@1`;
//!   JSON in, JSON out; errors as problem details with an optional `retryable` flag.
//!
//! Both directions authenticate with the module's service token: the module calls the Frood
//! API with it, and the core presents it when calling the module.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use frood_config::{Config, ServerPlacement};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Port modules listen on inside their containers.
pub const MODULE_PORT: u16 = 8080;
/// First host port for modules when the server runs on the host.
pub const FIRST_HOST_PORT: u16 = 18100;

/// Name of the secret that holds a module's service token.
pub fn token_secret(module: &str) -> String {
    format!(
        "FROOD_MODULE_TOKEN_{}",
        module.to_ascii_uppercase().replace('-', "_")
    )
}

/// Compose service name of a module.
pub fn service_name(module: &str) -> String {
    format!("module-{module}")
}

/// Host port of a module when the server runs on the host: its `port`, or the next free
/// one from 18100 in name order (modules with a `url` take none).
pub fn host_port(config: &Config, module: &str) -> u16 {
    if let Some(port) = config.modules.get(module).and_then(|m| m.port) {
        return port;
    }
    let taken: Vec<u16> = config.modules.values().filter_map(|m| m.port).collect();
    let mut next = FIRST_HOST_PORT;
    for (name, settings) in &config.modules {
        if settings.port.is_some() || settings.url.is_some() {
            continue;
        }
        while taken.contains(&next) {
            next += 1;
        }
        if name == module {
            return next;
        }
        next += 1;
    }
    next
}

/// Where the core reaches a module.
pub fn module_url(config: &Config, module: &str) -> String {
    if let Some(url) = config.modules.get(module).and_then(|m| m.url.clone()) {
        return url.trim_end_matches('/').to_owned();
    }
    match config.deploy.server {
        ServerPlacement::Container => format!("http://{}:{MODULE_PORT}", service_name(module)),
        ServerPlacement::Host => format!("http://127.0.0.1:{}", host_port(config, module)),
    }
}

/// What a module declares about itself (docs/16 §2). Unknown fields are kept out.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Capabilities with major versions, e.g. `media.transcribe@1`.
    #[serde(default)]
    pub provides: Vec<String>,
    /// Scopes of the module's service token.
    #[serde(default)]
    pub frood_api: Vec<String>,
}

impl Manifest {
    /// The versioned capability this module gives for a capability name, if any.
    pub fn capability(&self, name: &str) -> Option<&str> {
        self.provides
            .iter()
            .map(String::as_str)
            .find(|p| p.split('@').next() == Some(name))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ModuleError {
    #[error("module {0:?} is not in modules")]
    Unknown(String),
    #[error("no module provides {0}")]
    NoProvider(String),
    #[error("module {module} is not reachable: {message}")]
    Unavailable { module: String, message: String },
    #[error("module {module} refused the call ({status}): {message}")]
    Rejected {
        module: String,
        status: u16,
        message: String,
        retryable: bool,
    },
    #[error("module {module} answered something unexpected: {message}")]
    Invalid { module: String, message: String },
}

impl ModuleError {
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Unavailable { .. } | Self::NoProvider(_) => true,
            Self::Rejected { retryable, .. } => *retryable,
            _ => false,
        }
    }
}

#[derive(Debug, Clone)]
struct Endpoint {
    url: String,
    token: Option<String>,
}

/// The installation's container modules.
#[derive(Clone)]
pub struct Modules {
    http: reqwest::Client,
    endpoints: Arc<BTreeMap<String, Endpoint>>,
    manifests: Arc<RwLock<BTreeMap<String, Manifest>>>,
}

impl Modules {
    /// `token_of` reads a module's service token (from the secrets file).
    pub fn new(config: &Config, token_of: impl Fn(&str) -> Option<String>) -> Self {
        let endpoints = config
            .modules
            .keys()
            .map(|name| {
                let endpoint = Endpoint {
                    url: module_url(config, name),
                    token: token_of(&token_secret(name)),
                };
                (name.clone(), endpoint)
            })
            .collect();
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(300))
                .build()
                .expect("HTTP client builds"),
            endpoints: Arc::new(endpoints),
            manifests: Arc::default(),
        }
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.endpoints.keys().map(String::as_str)
    }

    /// Modules whose manifest (as discovered so far) provides a capability, e.g. `notify.send`.
    pub fn providers(&self, capability: &str) -> Vec<String> {
        let Ok(manifests) = self.manifests.read() else {
            return Vec::new();
        };
        manifests
            .iter()
            .filter(|(_, m)| m.capability(capability).is_some())
            .map(|(name, _)| name.clone())
            .collect()
    }

    pub fn token(&self, module: &str) -> Option<&str> {
        self.endpoints.get(module)?.token.as_deref()
    }

    pub fn manifest(&self, module: &str) -> Option<Manifest> {
        self.manifests.read().ok()?.get(module).cloned()
    }

    fn endpoint(&self, module: &str) -> Result<&Endpoint, ModuleError> {
        self.endpoints
            .get(module)
            .ok_or_else(|| ModuleError::Unknown(module.to_owned()))
    }

    /// Reads a module's manifest and remembers it.
    pub async fn discover(&self, module: &str) -> Result<Manifest, ModuleError> {
        let endpoint = self.endpoint(module)?;
        let unavailable = |message: String| ModuleError::Unavailable {
            module: module.to_owned(),
            message,
        };
        let response = self
            .http
            .get(format!("{}/manifest", endpoint.url))
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| unavailable(e.to_string()))?;
        if !response.status().is_success() {
            return Err(unavailable(format!(
                "manifest answered {}",
                response.status()
            )));
        }
        let manifest: Manifest = response.json().await.map_err(|e| ModuleError::Invalid {
            module: module.to_owned(),
            message: e.to_string(),
        })?;
        if let Ok(mut manifests) = self.manifests.write() {
            manifests.insert(module.to_owned(), manifest.clone());
        }
        Ok(manifest)
    }

    /// Discovers every module, retrying the missing ones until all answered or `wait` passed.
    /// `on_found` runs once per module with its manifest (the server registers the module's
    /// service token there).
    pub async fn discover_all<F, Fut>(&self, wait: Duration, mut on_found: F)
    where
        F: FnMut(String, Manifest) -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        let deadline = tokio::time::Instant::now() + wait;
        let mut pending: Vec<String> = self.endpoints.keys().cloned().collect();
        while !pending.is_empty() {
            let mut still = Vec::new();
            for name in pending {
                match self.discover(&name).await {
                    Ok(manifest) => {
                        tracing::info!(module = %name, version = %manifest.version, provides = ?manifest.provides, "module found");
                        on_found(name, manifest).await;
                    }
                    Err(err) => {
                        tracing::debug!(module = %name, error = %err, "module not ready");
                        still.push(name);
                    }
                }
            }
            pending = still;
            if pending.is_empty() || tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        for name in &pending {
            tracing::warn!(module = %name, "module did not answer; its capabilities are unavailable until it does");
        }
    }

    /// The module that provides `capability` (a name without version): the one named in
    /// `preferred`, or the only module that declares it.
    pub fn provider(
        &self,
        capability: &str,
        preferred: Option<&str>,
    ) -> Result<String, ModuleError> {
        if let Some(module) = preferred {
            self.endpoint(module)?;
            return Ok(module.to_owned());
        }
        let manifests = self
            .manifests
            .read()
            .map_err(|_| ModuleError::NoProvider(capability.to_owned()))?;
        let mut found = manifests
            .iter()
            .filter(|(_, m)| m.capability(capability).is_some())
            .map(|(name, _)| name.clone());
        match (found.next(), found.next()) {
            (Some(one), None) => Ok(one),
            (Some(_), Some(_)) => Err(ModuleError::Invalid {
                module: capability.to_owned(),
                message: "several modules provide it; name one in the job's `with.module`".into(),
            }),
            (None, _) => Err(ModuleError::NoProvider(capability.to_owned())),
        }
    }

    /// Calls a capability of a module. `capability` is a name without version; the version
    /// comes from the module's manifest.
    pub async fn call(
        &self,
        module: &str,
        capability: &str,
        input: &Value,
    ) -> Result<Value, ModuleError> {
        let endpoint = self.endpoint(module)?;
        let manifest = match self.manifest(module) {
            Some(manifest) => manifest,
            None => self.discover(module).await?,
        };
        let versioned = manifest
            .capability(capability)
            .ok_or_else(|| ModuleError::Invalid {
                module: module.to_owned(),
                message: format!("its manifest does not provide {capability}"),
            })?
            .to_owned();
        let mut request = self
            .http
            .post(format!("{}/v1/capabilities/{versioned}", endpoint.url))
            .json(input);
        if let Some(token) = &endpoint.token {
            request = request.bearer_auth(token);
        }
        let response = request.send().await.map_err(|e| ModuleError::Unavailable {
            module: module.to_owned(),
            message: e.to_string(),
        })?;
        let status = response.status();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        if status.is_success() {
            return Ok(body);
        }
        let message = body
            .get("detail")
            .and_then(Value::as_str)
            .unwrap_or("no details")
            .to_owned();
        if status.is_server_error() && body.get("retryable").is_none() {
            return Err(ModuleError::Unavailable {
                module: module.to_owned(),
                message: format!("HTTP {status}: {message}"),
            });
        }
        Err(ModuleError::Rejected {
            module: module.to_owned(),
            status: status.as_u16(),
            message,
            retryable: body
                .get("retryable")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }
}

#[cfg(test)]
mod tests {
    use axum::Json;
    use axum::http::HeaderMap;
    use axum::routing::{get, post};
    use serde_json::json;

    use super::*;

    fn config(yaml_modules: &str, server: &str) -> Config {
        let raw = format!(
            "version: 1\ninstallation: {{name: t}}\nstorage: {{uses: postgres, url: 'postgres://x'}}\n\
             deploy: {{server: {server}, build: .}}\nmodules:\n{yaml_modules}"
        );
        frood_config::parse(
            &raw,
            &frood_config::Secrets::from_pairs(Vec::<(String, String)>::new()),
        )
        .unwrap()
        .config
    }

    #[test]
    fn urls_follow_where_the_server_runs() {
        let modules = "  stt: { build: ../stt }\n  telegram: { image: t, port: 18100 }\n  zz: { image: z }\n  dev: { url: 'http://127.0.0.1:9999/' }\n";
        let host = config(modules, "host");
        assert_eq!(module_url(&host, "telegram"), "http://127.0.0.1:18100");
        assert_eq!(module_url(&host, "stt"), "http://127.0.0.1:18101");
        assert_eq!(module_url(&host, "zz"), "http://127.0.0.1:18102");
        assert_eq!(module_url(&host, "dev"), "http://127.0.0.1:9999");
        let container = config(modules, "container");
        assert_eq!(module_url(&container, "stt"), "http://module-stt:8080");
        assert_eq!(token_secret("my-stt"), "FROOD_MODULE_TOKEN_MY_STT");
    }

    #[tokio::test]
    async fn discovers_and_calls_a_capability() {
        let app = axum::Router::new()
            .route(
                "/manifest",
                get(|| async {
                    Json(json!({ "name": "stt-fake", "version": "0.1.0", "provides": ["media.transcribe@1"], "frood_api": [] }))
                }),
            )
            .route(
                "/v1/capabilities/media.transcribe@1",
                post(|headers: HeaderMap, Json(body): Json<Value>| async move {
                    let auth = headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or_default().to_owned();
                    Json(json!({ "text": format!("{} {}", body["hint"].as_str().unwrap_or_default(), auth) }))
                }),
            )
            .route(
                "/v1/capabilities/media.fail@1",
                post(|| async { (axum::http::StatusCode::BAD_REQUEST, Json(json!({ "detail": "bad audio" }))) }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let cfg = config(&format!("  stt: {{ url: '{url}' }}\n"), "host");
        let modules = Modules::new(&cfg, |name| {
            (name == "FROOD_MODULE_TOKEN_STT").then(|| "secret".to_owned())
        });
        assert!(matches!(
            modules.provider("media.transcribe", None),
            Err(ModuleError::NoProvider(_))
        ));
        let mut found = Vec::new();
        modules
            .discover_all(Duration::from_secs(1), |name, _| {
                found.push(name);
                async {}
            })
            .await;
        assert_eq!(found, ["stt"]);
        assert_eq!(modules.provider("media.transcribe", None).unwrap(), "stt");
        let out = modules
            .call("stt", "media.transcribe", &json!({ "hint": "hi" }))
            .await
            .unwrap();
        assert_eq!(out["text"], "hi Bearer secret");

        let err = modules
            .call("stt", "media.parse", &json!({}))
            .await
            .unwrap_err();
        assert!(matches!(err, ModuleError::Invalid { .. }), "{err}");
    }
}
