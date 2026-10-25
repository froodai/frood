//! Signing in to an MCP server (the MCP authorization spec, 2025-06-18): the server names its
//! authorization server (RFC 9728), Frood registers itself there (RFC 7591), the person signs
//! in with PKCE, and the tokens are refreshed when they run out.

use std::time::Duration;

use base64::Engine;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

const LIMIT: Duration = Duration::from_secs(20);

/// What Frood keeps of a sign-in, sealed in the vault.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Grant {
    pub token_endpoint: String,
    pub client_id: String,
    #[serde(default)]
    pub client_secret: Option<String>,
    /// The MCP server the tokens are for (`resource`).
    pub resource: String,
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
}

impl Grant {
    /// Runs out within a minute.
    pub fn stale(&self, now: DateTime<Utc>) -> bool {
        self.expires_at
            .is_some_and(|at| at - chrono::Duration::seconds(60) <= now)
    }
}

/// A sign-in that waits for the person to come back.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pending {
    pub token_endpoint: String,
    pub client_id: String,
    #[serde(default)]
    pub client_secret: Option<String>,
    pub resource: String,
    pub redirect_uri: String,
    pub verifier: String,
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(LIMIT)
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()
        .expect("an HTTP client")
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// A random string for PKCE and `state`.
pub fn random() -> String {
    let mut bytes = Vec::with_capacity(32);
    bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    b64(&bytes)
}

async fn get_json(client: &reqwest::Client, url: &str) -> Option<Value> {
    let response = client
        .get(url)
        .header("accept", "application/json")
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    response.json().await.ok()
}

/// `https://host/a/b` + `/.well-known/x` → `https://host/.well-known/x/a/b` (RFC 8414, 9728),
/// then the same at the root.
fn well_known(base: &url::Url, name: &str) -> Vec<String> {
    let origin = base.origin().ascii_serialization();
    let path = base.path().trim_end_matches('/');
    let mut out = Vec::new();
    if !path.is_empty() {
        out.push(format!("{origin}/.well-known/{name}{path}"));
    }
    out.push(format!("{origin}/.well-known/{name}"));
    out
}

/// Where to sign in for an MCP server: its authorization server's endpoints.
pub struct Discovered {
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub registration_endpoint: Option<String>,
    pub scopes: Vec<String>,
}

pub async fn discover(server: &str) -> Result<Discovered, String> {
    let client = client();
    let url = url::Url::parse(server).map_err(|_| format!("{server:?} is not an address"))?;
    // The server names its resource metadata in the 401's WWW-Authenticate, or it is at the
    // well-known place.
    let mut candidates = Vec::new();
    if let Ok(response) = client
        .post(server)
        .header("accept", "application/json, text/event-stream")
        .json(
            &serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": { "name": "frood", "version": env!("CARGO_PKG_VERSION") } } }),
        )
        .send()
        .await
        && let Some(named) = response
            .headers()
            .get("www-authenticate")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| parameter(v, "resource_metadata"))
    {
        candidates.push(named);
    }
    candidates.extend(well_known(&url, "oauth-protected-resource"));
    let mut issuer = None;
    let mut scopes = Vec::new();
    for candidate in candidates {
        if let Some(meta) = get_json(&client, &candidate).await
            && let Some(first) = meta["authorization_servers"][0].as_str()
        {
            issuer = Some(first.to_owned());
            scopes = strings(&meta["scopes_supported"]);
            break;
        }
    }
    // An older server is its own authorization server.
    let issuer = issuer.unwrap_or_else(|| url.origin().ascii_serialization());
    let issuer_url =
        url::Url::parse(&issuer).map_err(|_| format!("{issuer:?} is not an address"))?;
    let mut places = well_known(&issuer_url, "oauth-authorization-server");
    places.extend(well_known(&issuer_url, "openid-configuration"));
    for place in places {
        if let Some(meta) = get_json(&client, &place).await
            && let (Some(authorize), Some(token)) = (
                meta["authorization_endpoint"].as_str(),
                meta["token_endpoint"].as_str(),
            )
        {
            if scopes.is_empty() {
                scopes = strings(&meta["scopes_supported"]);
            }
            return Ok(Discovered {
                authorization_endpoint: authorize.to_owned(),
                token_endpoint: token.to_owned(),
                registration_endpoint: meta["registration_endpoint"].as_str().map(str::to_owned),
                scopes,
            });
        }
    }
    Err(format!(
        "{server} does not say where to sign in (no OAuth metadata); connect it with a token"
    ))
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s.as_str().map(str::to_owned))
        .collect()
}

/// `key="value"` in a WWW-Authenticate header.
fn parameter(header: &str, key: &str) -> Option<String> {
    let at = header.find(&format!("{key}="))? + key.len() + 1;
    let rest = &header[at..];
    let value = match rest.strip_prefix('"') {
        Some(quoted) => &quoted[..quoted.find('"')?],
        None => rest.split([',', ' ']).next()?,
    };
    Some(value.to_owned())
}

/// Registers Frood as a client of the authorization server; its id and secret, if it gave one.
pub async fn register(
    registration_endpoint: &str,
    redirect_uri: &str,
    installation: &str,
) -> Result<(String, Option<String>), String> {
    let response = client()
        .post(registration_endpoint)
        .json(&serde_json::json!({
            "client_name": format!("Frood ({installation})"),
            "redirect_uris": [redirect_uri],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none",
        }))
        .send()
        .await
        .map_err(|e| format!("registration did not answer: {}", e.without_url()))?;
    let status = response.status();
    let body: Value = response.json().await.unwrap_or_default();
    match body["client_id"].as_str() {
        Some(id) if status.is_success() => Ok((
            id.to_owned(),
            body["client_secret"].as_str().map(str::to_owned),
        )),
        _ => Err(format!(
            "the server did not register Frood ({status}): {}",
            body["error_description"]
                .as_str()
                .or(body["error"].as_str())
                .unwrap_or("no details")
        )),
    }
}

/// The address the person signs in at, and what waits for their return.
pub fn authorize_url(
    discovered: &Discovered,
    client_id: &str,
    client_secret: Option<String>,
    resource: &str,
    redirect_uri: &str,
    state: &str,
) -> Result<(String, Pending), String> {
    let verifier = random();
    let challenge = b64(&Sha256::digest(verifier.as_bytes()));
    let mut url = url::Url::parse(&discovered.authorization_endpoint)
        .map_err(|_| "the authorization endpoint is not an address".to_owned())?;
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("response_type", "code")
            .append_pair("client_id", client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("state", state)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("resource", resource);
        if !discovered.scopes.is_empty() {
            query.append_pair("scope", &discovered.scopes.join(" "));
        }
    }
    Ok((
        url.to_string(),
        Pending {
            token_endpoint: discovered.token_endpoint.clone(),
            client_id: client_id.to_owned(),
            client_secret,
            resource: resource.to_owned(),
            redirect_uri: redirect_uri.to_owned(),
            verifier,
        },
    ))
}

async fn token(
    endpoint: &str,
    form: &[(&str, &str)],
    now: DateTime<Utc>,
) -> Result<(String, Option<String>, Option<DateTime<Utc>>), String> {
    let response = client()
        .post(endpoint)
        .header("accept", "application/json")
        .form(form)
        .send()
        .await
        .map_err(|e| format!("the token endpoint did not answer: {}", e.without_url()))?;
    let status = response.status();
    let body: Value = response.json().await.unwrap_or_default();
    let Some(access) = body["access_token"]
        .as_str()
        .filter(|_| status.is_success())
    else {
        return Err(format!(
            "no token ({status}): {}",
            body["error_description"]
                .as_str()
                .or(body["error"].as_str())
                .unwrap_or("no details")
        ));
    };
    let expires_at = body["expires_in"]
        .as_i64()
        .map(|seconds| now + chrono::Duration::seconds(seconds));
    Ok((
        access.to_owned(),
        body["refresh_token"].as_str().map(str::to_owned),
        expires_at,
    ))
}

/// The person came back with a code: tokens for it.
pub async fn exchange(pending: &Pending, code: &str) -> Result<Grant, String> {
    let mut form = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", pending.redirect_uri.as_str()),
        ("client_id", pending.client_id.as_str()),
        ("code_verifier", pending.verifier.as_str()),
        ("resource", pending.resource.as_str()),
    ];
    if let Some(secret) = &pending.client_secret {
        form.push(("client_secret", secret));
    }
    let (access_token, refresh_token, expires_at) =
        token(&pending.token_endpoint, &form, Utc::now()).await?;
    Ok(Grant {
        token_endpoint: pending.token_endpoint.clone(),
        client_id: pending.client_id.clone(),
        client_secret: pending.client_secret.clone(),
        resource: pending.resource.clone(),
        access_token,
        refresh_token,
        expires_at,
    })
}

/// A fresh access token from the refresh token; the refresh token may change too.
pub async fn refresh(grant: &Grant) -> Result<Grant, String> {
    let refresh_token = grant
        .refresh_token
        .as_deref()
        .ok_or("the sign-in ran out and gave no refresh token: sign in again")?;
    let mut form = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", grant.client_id.as_str()),
        ("resource", grant.resource.as_str()),
    ];
    if let Some(secret) = &grant.client_secret {
        form.push(("client_secret", secret));
    }
    let (access_token, new_refresh, expires_at) =
        token(&grant.token_endpoint, &form, Utc::now()).await?;
    Ok(Grant {
        access_token,
        refresh_token: new_refresh.or_else(|| grant.refresh_token.clone()),
        expires_at,
        ..grant.clone()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn well_known_places_go_between_host_and_path() {
        let url = url::Url::parse("https://mcp.example.com/v1/mcp").unwrap();
        assert_eq!(
            well_known(&url, "oauth-protected-resource"),
            [
                "https://mcp.example.com/.well-known/oauth-protected-resource/v1/mcp",
                "https://mcp.example.com/.well-known/oauth-protected-resource"
            ]
        );
    }

    #[test]
    fn the_resource_metadata_is_read_from_the_challenge() {
        let header = r#"Bearer error="invalid_token", resource_metadata="https://mcp.example.com/.well-known/oauth-protected-resource""#;
        assert_eq!(
            parameter(header, "resource_metadata").as_deref(),
            Some("https://mcp.example.com/.well-known/oauth-protected-resource")
        );
        assert_eq!(
            parameter("Bearer realm=x, scope=read", "scope").as_deref(),
            Some("read")
        );
    }
}
