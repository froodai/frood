//! A small client of the Kubernetes API: plain HTTPS with the pod's ServiceAccount token, for
//! the few namespaced objects this module touches. No watch: the core polls `status`, and each
//! poll reads the Job once.

use std::path::PathBuf;
use std::time::Duration;

use reqwest::{Method, StatusCode};
use serde_json::Value;

/// Where an in-cluster pod finds its ServiceAccount.
const SERVICE_ACCOUNT: &str = "/var/run/secrets/kubernetes.io/serviceaccount";

/// The objects this module works with, in one namespace.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Job,
    Pod,
    Claim,
    Secret,
}

impl Kind {
    fn collection(self) -> (&'static str, &'static str) {
        match self {
            Self::Job => ("/apis/batch/v1", "jobs"),
            Self::Pod => ("/api/v1", "pods"),
            Self::Claim => ("/api/v1", "persistentvolumeclaims"),
            Self::Secret => ("/api/v1", "secrets"),
        }
    }
}

/// An answer of the API server that is not a success.
#[derive(Debug)]
pub struct ApiError {
    pub status: u16,
    pub message: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.status {
            0 => write!(f, "Kubernetes API: {}", self.message),
            status => write!(f, "Kubernetes API {status}: {}", self.message),
        }
    }
}

impl ApiError {
    pub fn not_found(&self) -> bool {
        self.status == 404
    }

    pub fn conflict(&self) -> bool {
        self.status == 409
    }
}

/// How requests prove who the module is.
pub enum Token {
    /// The projected ServiceAccount token: read on every request, as the kubelet rotates it.
    File(PathBuf),
    #[cfg_attr(not(test), allow(dead_code))]
    Static(String),
    None,
}

pub struct Kube {
    http: reqwest::Client,
    base: String,
    token: Token,
    pub namespace: String,
}

impl Kube {
    pub fn new(
        base: &str,
        token: Token,
        namespace: &str,
        ca_pem: Option<&[u8]>,
    ) -> Result<Self, String> {
        let mut builder = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10));
        if let Some(pem) = ca_pem {
            let certs = reqwest::Certificate::from_pem_bundle(pem).map_err(|e| e.to_string())?;
            builder = builder.tls_certs_only(certs);
        }
        Ok(Self {
            http: builder.build().map_err(|e| e.to_string())?,
            base: base.trim_end_matches('/').to_owned(),
            token,
            namespace: namespace.to_owned(),
        })
    }

    /// The API server this pod runs under, with its ServiceAccount; `api` (e.g. a
    /// `kubectl proxy` during development) replaces the in-cluster address.
    pub fn in_cluster(api: Option<&str>, namespace: Option<&str>) -> Result<Self, String> {
        let account = PathBuf::from(SERVICE_ACCOUNT);
        let base = match api {
            Some(api) => api.to_owned(),
            None => {
                let host = std::env::var("KUBERNETES_SERVICE_HOST").map_err(|_| {
                    "not in a Kubernetes pod: KUBERNETES_SERVICE_HOST is not set".to_owned()
                })?;
                let port =
                    std::env::var("KUBERNETES_SERVICE_PORT").unwrap_or_else(|_| "443".into());
                // An IPv6 address needs brackets in a URL.
                let host = if host.contains(':') {
                    format!("[{host}]")
                } else {
                    host
                };
                format!("https://{host}:{port}")
            }
        };
        let namespace = match namespace {
            Some(namespace) => namespace.to_owned(),
            None => std::fs::read_to_string(account.join("namespace"))
                .map(|n| n.trim().to_owned())
                .map_err(|e| format!("set `namespace`: cannot read this pod's namespace: {e}"))?,
        };
        let token_file = account.join("token");
        let token = if token_file.exists() {
            Token::File(token_file)
        } else {
            Token::None
        };
        let ca = std::fs::read(account.join("ca.crt")).ok();
        Self::new(&base, token, &namespace, ca.as_deref())
    }

    fn path(&self, kind: Kind, name: Option<&str>, sub: Option<&str>) -> String {
        let (group, plural) = kind.collection();
        let mut path = format!(
            "{}{group}/namespaces/{}/{plural}",
            self.base, self.namespace
        );
        if let Some(name) = name {
            path.push('/');
            path.push_str(name);
        }
        if let Some(sub) = sub {
            path.push('/');
            path.push_str(sub);
        }
        path
    }

    async fn send(
        &self,
        method: Method,
        url: String,
        query: &[(&str, String)],
        body: Option<(&str, &Value)>,
    ) -> Result<String, ApiError> {
        let mut request = self.http.request(method, url).query(query);
        let token = match &self.token {
            Token::File(path) => tokio::fs::read_to_string(path)
                .await
                .map(|t| Some(t.trim().to_owned()))
                .map_err(|e| ApiError {
                    status: 0,
                    message: format!("cannot read the ServiceAccount token: {e}"),
                })?,
            Token::Static(token) => Some(token.clone()),
            Token::None => None,
        };
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        if let Some((content_type, body)) = body {
            request = request
                .header(reqwest::header::CONTENT_TYPE, content_type)
                .body(body.to_string());
        }
        let response = request.send().await.map_err(|e| ApiError {
            status: 0,
            message: e.to_string(),
        })?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if status.is_success() {
            return Ok(text);
        }
        // A Status object says why; anything else is shown as it came.
        let message = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v["message"].as_str().map(str::to_owned))
            .unwrap_or_else(|| text.chars().take(300).collect());
        Err(ApiError {
            status: status.as_u16(),
            message: if message.is_empty() {
                status.canonical_reason().unwrap_or("error").to_owned()
            } else {
                message
            },
        })
    }

    fn json(text: &str) -> Result<Value, ApiError> {
        serde_json::from_str(text).map_err(|e| ApiError {
            status: 0,
            message: format!("unreadable answer: {e}"),
        })
    }

    pub async fn create(&self, kind: Kind, object: &Value) -> Result<Value, ApiError> {
        let text = self
            .send(
                Method::POST,
                self.path(kind, None, None),
                &[],
                Some(("application/json", object)),
            )
            .await?;
        Self::json(&text)
    }

    /// The object, or `None` when it is not there.
    pub async fn get(&self, kind: Kind, name: &str) -> Result<Option<Value>, ApiError> {
        match self
            .send(Method::GET, self.path(kind, Some(name), None), &[], None)
            .await
        {
            Ok(text) => Self::json(&text).map(Some),
            Err(err) if err.not_found() => Ok(None),
            Err(err) => Err(err),
        }
    }

    pub async fn merge_patch(
        &self,
        kind: Kind,
        name: &str,
        patch: &Value,
    ) -> Result<Value, ApiError> {
        let text = self
            .send(
                Method::PATCH,
                self.path(kind, Some(name), None),
                &[],
                Some(("application/merge-patch+json", patch)),
            )
            .await?;
        Self::json(&text)
    }

    /// Deletes the object and what it owns (its pods, its Secret) in the background; not an
    /// error when it is gone.
    pub async fn delete(&self, kind: Kind, name: &str) -> Result<(), ApiError> {
        match self
            .send(
                Method::DELETE,
                self.path(kind, Some(name), None),
                &[("propagationPolicy", "Background".to_owned())],
                None,
            )
            .await
        {
            Err(err) if !err.not_found() => Err(err),
            _ => Ok(()),
        }
    }

    pub async fn list(&self, kind: Kind, label_selector: &str) -> Result<Vec<Value>, ApiError> {
        let text = self
            .send(
                Method::GET,
                self.path(kind, None, None),
                &[("labelSelector", label_selector.to_owned())],
                None,
            )
            .await?;
        Ok(Self::json(&text)?["items"]
            .as_array()
            .cloned()
            .unwrap_or_default())
    }

    /// What a container of a pod printed (stdout and stderr together, as Kubernetes keeps
    /// them); `None` while the container has not started.
    pub async fn logs(
        &self,
        pod: &str,
        container: &str,
        tail_lines: Option<u32>,
    ) -> Result<Option<String>, ApiError> {
        let mut query = vec![("container", container.to_owned())];
        if let Some(tail) = tail_lines {
            query.push(("tailLines", tail.to_string()));
        }
        match self
            .send(
                Method::GET,
                self.path(Kind::Pod, Some(pod), Some("log")),
                &query,
                None,
            )
            .await
        {
            Ok(text) => Ok(Some(text)),
            // "container … is waiting to start", or the pod is not there yet.
            Err(err) if err.status == StatusCode::BAD_REQUEST.as_u16() || err.not_found() => {
                Ok(None)
            }
            Err(err) => Err(err),
        }
    }
}
