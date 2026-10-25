//! OAuth clients: Client ID Metadata Documents (fetched, cached) and Dynamic Client
//! Registration (stored), plus redirect URI rules.

use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;
use uuid::Uuid;

use crate::{Auth, AuthError, Result};

const CIMD_MAX_BYTES: usize = 5 * 1024;
const CIMD_TTL: Duration = Duration::from_secs(600);
const MAX_REGISTERED_CLIENTS: i64 = 1000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientInfo {
    pub client_id: String,
    pub name: Option<String>,
    pub redirect_uris: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RegistrationRequest {
    pub redirect_uris: Vec<String>,
    #[serde(default)]
    pub client_name: Option<String>,
    #[serde(default)]
    pub token_endpoint_auth_method: Option<String>,
}

/// Client id of the admin panel the core serves at `/admin/`.
pub const ADMIN_CLIENT: &str = "frood-admin";

impl Auth {
    /// Looks up a client: an `https://` client id is a metadata document URL, anything else
    /// must have been registered through DCR.
    pub(crate) async fn resolve_client(&self, client_id: &str) -> Result<ClientInfo> {
        // The installation's own admin panel: it may only come back to this installation.
        if client_id == ADMIN_CLIENT {
            return Ok(ClientInfo {
                client_id: client_id.to_owned(),
                name: Some("Frood admin panel".to_owned()),
                redirect_uris: vec![format!(
                    "{}/admin/",
                    self.settings().issuer.trim_end_matches('/')
                )],
            });
        }
        if client_id.starts_with("https://") {
            if let Some((info, at)) = self
                .inner
                .cimd_cache
                .lock()
                .expect("cache lock")
                .get(client_id)
                && at.elapsed() < CIMD_TTL
            {
                return Ok(info.clone());
            }
            let document = fetch_metadata_document(client_id).await?;
            let info = parse_metadata_document(client_id, &document)?;
            self.inner
                .cimd_cache
                .lock()
                .expect("cache lock")
                .insert(client_id.to_owned(), (info.clone(), Instant::now()));
            return Ok(info);
        }
        let row: Option<(Option<String>, Vec<String>)> = sqlx::query_as(
            "SELECT client_name, redirect_uris FROM oauth_clients WHERE client_id = $1",
        )
        .bind(client_id)
        .fetch_optional(self.pool())
        .await?;
        let (name, redirect_uris) =
            row.ok_or_else(|| AuthError::Invalid(format!("unknown client {client_id:?}")))?;
        Ok(ClientInfo {
            client_id: client_id.to_owned(),
            name,
            redirect_uris,
        })
    }

    /// RFC 7591 registration of a public client.
    pub(crate) async fn register_client(&self, request: RegistrationRequest) -> Result<ClientInfo> {
        if let Some(method) = &request.token_endpoint_auth_method
            && method != "none"
        {
            return Err(AuthError::Invalid(format!(
                "invalid_client_metadata: token_endpoint_auth_method {method:?} is not supported; use none with PKCE"
            )));
        }
        if request.redirect_uris.is_empty() || request.redirect_uris.len() > 10 {
            return Err(AuthError::Invalid(
                "invalid_redirect_uri: give 1 to 10 redirect URIs".into(),
            ));
        }
        for uri in &request.redirect_uris {
            check_redirect_uri(uri)
                .map_err(|e| AuthError::Invalid(format!("invalid_redirect_uri: {e}")))?;
        }
        let name = request
            .client_name
            .map(|n| n.chars().take(100).collect::<String>());
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM oauth_clients")
            .fetch_one(self.pool())
            .await?;
        if count >= MAX_REGISTERED_CLIENTS {
            return Err(AuthError::Invalid(
                "too many registered clients; remove unused ones".into(),
            ));
        }
        let client_id = format!("frood_client_{}", Uuid::now_v7().simple());
        sqlx::query(
            "INSERT INTO oauth_clients (client_id, client_name, redirect_uris) VALUES ($1, $2, $3)",
        )
        .bind(&client_id)
        .bind(&name)
        .bind(&request.redirect_uris)
        .execute(self.pool())
        .await?;
        Ok(ClientInfo {
            client_id,
            name,
            redirect_uris: request.redirect_uris,
        })
    }
}

/// Redirect URIs a client may register: https, loopback http (any port at use time, RFC 8252
/// §7.3), or a private-use scheme like `cursor://` (RFC 8252 §7.1).
fn check_redirect_uri(uri: &str) -> std::result::Result<(), String> {
    if uri.len() > 512 {
        return Err("redirect URI is too long".into());
    }
    let url = Url::parse(uri).map_err(|_| format!("{uri:?} is not a URL"))?;
    if url.fragment().is_some() {
        return Err(format!("{uri:?} must not have a fragment"));
    }
    match url.scheme() {
        "https" => Ok(()),
        "http" if is_loopback_host(&url) => Ok(()),
        "http" => Err(format!("{uri:?}: plain http is allowed only for localhost")),
        "file" | "javascript" | "data" | "vbscript" | "about" | "blob" | "ws" | "wss" => {
            Err(format!("{uri:?}: scheme {:?} is not allowed", url.scheme()))
        }
        scheme if scheme.contains('.') || scheme.len() > 3 => Ok(()),
        scheme => Err(format!("{uri:?}: scheme {scheme:?} is not allowed")),
    }
}

fn is_loopback_host(url: &Url) -> bool {
    matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
}

/// Exact match, except loopback http URIs, where the port may differ (RFC 8252 §7.3).
pub(crate) fn redirect_allowed(registered: &[String], requested: &str) -> bool {
    if registered.iter().any(|r| r == requested) {
        return true;
    }
    let Ok(requested) = Url::parse(requested) else {
        return false;
    };
    if requested.scheme() != "http" || !is_loopback_host(&requested) {
        return false;
    }
    registered
        .iter()
        .filter_map(|r| Url::parse(r).ok())
        .any(|r| {
            r.scheme() == "http"
                && is_loopback_host(&r)
                && r.host_str() == requested.host_str()
                && r.path() == requested.path()
                && r.query() == requested.query()
        })
}

fn parse_metadata_document(client_id: &str, document: &Value) -> Result<ClientInfo> {
    let invalid =
        |m: &str| AuthError::Invalid(format!("client metadata document {client_id}: {m}"));
    if document.get("client_id").and_then(Value::as_str) != Some(client_id) {
        return Err(invalid("client_id must equal the document URL"));
    }
    let redirect_uris: Vec<String> = document
        .get("redirect_uris")
        .and_then(Value::as_array)
        .map(|uris| {
            uris.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    if redirect_uris.is_empty() {
        return Err(invalid("redirect_uris is missing"));
    }
    for uri in &redirect_uris {
        check_redirect_uri(uri).map_err(|e| invalid(&e))?;
    }
    let name = document
        .get("client_name")
        .and_then(Value::as_str)
        .map(|n| n.chars().take(100).collect());
    Ok(ClientInfo {
        client_id: client_id.to_owned(),
        name,
        redirect_uris,
    })
}

/// Fetches a metadata document without letting the URL reach internal networks: https only,
/// every resolved address must be public, the connection is pinned to the checked address,
/// no redirects, at most 5 KB.
async fn fetch_metadata_document(url: &str) -> Result<Value> {
    let invalid = |m: String| AuthError::Invalid(format!("client metadata document {url}: {m}"));
    let parsed = Url::parse(url).map_err(|_| invalid("not a URL".into()))?;
    if parsed.scheme() != "https"
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.fragment().is_some()
    {
        return Err(invalid("must be a plain https URL".into()));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| invalid("has no host".into()))?
        .to_owned();
    let port = parsed.port_or_known_default().unwrap_or(443);
    let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), port))
        .await
        .map_err(|e| invalid(format!("cannot resolve host: {e}")))?
        .collect();
    if addresses.is_empty() || addresses.iter().any(|a| !is_public_ip(a.ip())) {
        return Err(invalid("host resolves to a non-public address".into()));
    }
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .resolve(&host, addresses[0])
        .build()
        .map_err(|e| invalid(e.to_string()))?;
    let response = client
        .get(url)
        .header("accept", "application/json")
        .send()
        .await
        .map_err(|e| invalid(format!("fetch failed: {e}")))?;
    if !response.status().is_success() {
        return Err(invalid(format!("answered HTTP {}", response.status())));
    }
    let bytes = response.bytes().await.map_err(|e| invalid(e.to_string()))?;
    if bytes.len() > CIMD_MAX_BYTES {
        return Err(invalid("is larger than 5 KB".into()));
    }
    serde_json::from_slice(&bytes).map_err(|_| invalid("is not JSON".into()))
}

/// Not a private, local, link-local or reserved address: safe to fetch on someone's word.
pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_multicast()
                || a == 0
                || (a == 100 && (64..128).contains(&b)))
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public_ip(IpAddr::V4(v4));
            }
            let first = v6.segments()[0];
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80)
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn redirect_rules() {
        let registered = vec![
            "https://claude.ai/api/mcp/auth_callback".to_owned(),
            "http://127.0.0.1/callback".to_owned(),
        ];
        assert!(redirect_allowed(
            &registered,
            "https://claude.ai/api/mcp/auth_callback"
        ));
        assert!(redirect_allowed(
            &registered,
            "http://127.0.0.1:53124/callback"
        ));
        assert!(!redirect_allowed(
            &registered,
            "http://127.0.0.1:53124/other"
        ));
        assert!(!redirect_allowed(
            &registered,
            "http://localhost:53124/callback"
        ));
        assert!(!redirect_allowed(
            &registered,
            "https://evil.example/api/mcp/auth_callback"
        ));

        assert!(check_redirect_uri("https://chatgpt.com/connector/oauth/abc").is_ok());
        assert!(check_redirect_uri("http://localhost:3000/cb").is_ok());
        assert!(check_redirect_uri("cursor://anysphere.cursor-mcp/oauth/callback").is_ok());
        assert!(check_redirect_uri("http://example.com/cb").is_err());
        assert!(check_redirect_uri("https://x.example/cb#frag").is_err());
        assert!(check_redirect_uri("ftp://x").is_err());
        assert!(check_redirect_uri("file:///etc/passwd").is_err());
        assert!(check_redirect_uri("javascript://x").is_err());
    }

    #[test]
    fn metadata_documents_must_name_themselves() {
        let url = "https://chatgpt.com/oauth/client.json";
        let ok = json!({ "client_id": url, "client_name": "ChatGPT", "redirect_uris": ["https://chatgpt.com/cb"] });
        let info = parse_metadata_document(url, &ok).unwrap();
        assert_eq!(info.name.as_deref(), Some("ChatGPT"));
        assert!(
            parse_metadata_document(
                url,
                &json!({ "client_id": "https://other", "redirect_uris": ["https://x/cb"] })
            )
            .is_err()
        );
        assert!(parse_metadata_document(url, &json!({ "client_id": url })).is_err());
    }

    #[test]
    fn only_public_addresses_pass() {
        for blocked in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:10.0.0.1",
        ] {
            assert!(
                !is_public_ip(blocked.parse().unwrap()),
                "{blocked} must be blocked"
            );
        }
        for allowed in ["8.8.8.8", "1.1.1.1", "2606:4700:4700::1111"] {
            assert!(
                is_public_ip(allowed.parse().unwrap()),
                "{allowed} must pass"
            );
        }
    }

    #[tokio::test]
    async fn metadata_fetch_refuses_internal_targets() {
        for url in [
            "http://example.com/c.json",
            "https://127.0.0.1/c.json",
            "https://localhost/c.json",
            "https://user:pw@example.com/c.json",
        ] {
            assert!(
                fetch_metadata_document(url).await.is_err(),
                "{url} must be refused"
            );
        }
    }
}
