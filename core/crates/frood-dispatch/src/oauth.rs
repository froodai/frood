//! Signing in to an MCP server (the MCP authorization spec, 2025-06-18 and 2025-11-25): the
//! server names its authorization server (RFC 9728), Frood finds that server's endpoints
//! (RFC 8414), registers itself there as a public client (RFC 7591), the person signs in with
//! PKCE, and the tokens are refreshed when they run out, the refresh token rotating.

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
    /// When the server refused to refresh it: the person must sign in again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lapsed_at: Option<DateTime<Utc>>,
}

impl Grant {
    /// Runs out within a minute.
    pub fn stale(&self, now: DateTime<Utc>) -> bool {
        self.expires_at
            .is_some_and(|at| at - chrono::Duration::seconds(60) <= now)
    }

    /// Only a new sign-in helps: the server refused the refresh, or the token ran out and
    /// there is nothing to refresh it with.
    pub fn needs_sign_in(&self, now: DateTime<Utc>) -> bool {
        self.lapsed_at.is_some() || (self.refresh_token.is_none() && self.stale(now))
    }
}

/// Why a refresh did not give a token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshError {
    /// The server refused the refresh token (it ran out or was revoked): sign in again.
    Lapsed(String),
    /// The server could not be reached or failed: the next try may work.
    Failed(String),
}

impl std::fmt::Display for RefreshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Lapsed(why) | Self::Failed(why) => f.write_str(why),
        }
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

/// Where to sign in for an MCP server: its authorization server's endpoints, and what to ask
/// for.
#[derive(Debug, Clone)]
pub struct Discovered {
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub registration_endpoint: Option<String>,
    /// The scopes to request; none: the request names no scope.
    pub scopes: Vec<String>,
    /// The server's resource identifier, for the `resource` parameter (RFC 8707).
    pub resource: String,
}

/// The same address, but for a trailing slash.
fn same_address(a: &str, b: &str) -> bool {
    a.trim_end_matches('/') == b.trim_end_matches('/')
}

pub async fn discover(server: &str) -> Result<Discovered, String> {
    let client = client();
    let url = url::Url::parse(server).map_err(|_| format!("{server:?} is not an address"))?;
    // The server names its resource metadata and the scope it wants in the 401's
    // WWW-Authenticate, or the metadata is at the well-known place.
    let mut candidates = Vec::new();
    let mut challenged = Vec::new();
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
        && let Some(header) = response
            .headers()
            .get("www-authenticate")
            .and_then(|v| v.to_str().ok())
    {
        if let Some(named) = parameter(header, "resource_metadata") {
            candidates.push(named);
        }
        if let Some(scope) = parameter(header, "scope") {
            challenged = scope.split_whitespace().map(str::to_owned).collect();
        }
    }
    candidates.extend(well_known(&url, "oauth-protected-resource"));
    let mut issuer = None;
    let mut resource = server.to_owned();
    let mut scopes = Vec::new();
    for candidate in candidates {
        let Some(meta) = get_json(&client, &candidate).await else {
            continue;
        };
        // Metadata for another resource is not this server's (RFC 9728 §3.3).
        if let Some(named) = meta["resource"].as_str()
            && !same_address(named, server)
        {
            continue;
        }
        if let Some(first) = meta["authorization_servers"][0].as_str() {
            issuer = Some(first.to_owned());
            if let Some(named) = meta["resource"].as_str() {
                resource = named.to_owned();
            }
            scopes = strings(&meta["scopes_supported"]);
            break;
        }
    }
    // The scope the server asked for in its challenge, else what its metadata lists, else
    // none (MCP authorization, scope selection). Never all an authorization server knows: it
    // serves other resources too.
    if !challenged.is_empty() {
        scopes = challenged;
    }
    // An older server is its own authorization server.
    let issuer = issuer.unwrap_or_else(|| url.origin().ascii_serialization());
    let issuer_url =
        url::Url::parse(&issuer).map_err(|_| format!("{issuer:?} is not an address"))?;
    let mut places = well_known(&issuer_url, "oauth-authorization-server");
    places.extend(well_known(&issuer_url, "openid-configuration"));
    // OpenID Connect also appends its well-known part to an issuer with a path.
    if !issuer_url.path().trim_matches('/').is_empty() {
        places.push(format!(
            "{}/.well-known/openid-configuration",
            issuer.trim_end_matches('/')
        ));
    }
    for place in places {
        let Some(meta) = get_json(&client, &place).await else {
            continue;
        };
        // Metadata of another issuer is not this one's (RFC 8414 §3.3).
        if let Some(named) = meta["issuer"].as_str()
            && !same_address(named, &issuer)
        {
            continue;
        }
        let (Some(authorize), Some(token)) = (
            meta["authorization_endpoint"].as_str(),
            meta["token_endpoint"].as_str(),
        ) else {
            continue;
        };
        let methods = strings(&meta["code_challenge_methods_supported"]);
        if !methods.is_empty() && !methods.iter().any(|m| m == "S256") {
            return Err(format!(
                "{issuer} does not take PKCE with S256, which signing in needs; connect it with a token"
            ));
        }
        // A refresh token, where the server gives one only for `offline_access` (OpenID
        // Connect §11): asked for when the server lists it and refreshes.
        let grants = strings(&meta["grant_types_supported"]);
        if strings(&meta["scopes_supported"])
            .iter()
            .any(|s| s == "offline_access")
            && (grants.is_empty() || grants.iter().any(|g| g == "refresh_token"))
            && !scopes.is_empty()
            && !scopes.iter().any(|s| s == "offline_access")
        {
            scopes.push("offline_access".to_owned());
        }
        return Ok(Discovered {
            authorization_endpoint: authorize.to_owned(),
            token_endpoint: token.to_owned(),
            registration_endpoint: meta["registration_endpoint"].as_str().map(str::to_owned),
            scopes,
            resource,
        });
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

/// `key="value"` in a WWW-Authenticate header; `key` whole, not the end of another name.
fn parameter(header: &str, key: &str) -> Option<String> {
    let needle = format!("{key}=");
    let at = header.match_indices(&needle).find_map(|(at, _)| {
        let before = header[..at].chars().next_back();
        (!before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')).then_some(at)
    })? + needle.len();
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
    redirect_uri: &str,
    state: &str,
) -> Result<(String, Pending), String> {
    let resource = discovered.resource.as_str();
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

/// What a token endpoint answered instead of a token.
struct Refused {
    status: reqwest::StatusCode,
    /// The OAuth error code, e.g. `invalid_grant`.
    code: Option<String>,
    message: String,
}

type Tokens = (String, Option<String>, Option<DateTime<Utc>>);

async fn token(
    endpoint: &str,
    form: &[(&str, &str)],
    now: DateTime<Utc>,
) -> Result<Tokens, Refused> {
    let response = client()
        .post(endpoint)
        .header("accept", "application/json")
        .form(form)
        .send()
        .await
        .map_err(|e| Refused {
            status: reqwest::StatusCode::BAD_GATEWAY,
            code: None,
            message: format!("the token endpoint did not answer: {}", e.without_url()),
        })?;
    let status = response.status();
    let body: Value = response.json().await.unwrap_or_default();
    let Some(access) = body["access_token"]
        .as_str()
        .filter(|_| status.is_success())
    else {
        return Err(Refused {
            status,
            code: body["error"].as_str().map(str::to_owned),
            message: format!(
                "no token ({status}): {}",
                body["error_description"]
                    .as_str()
                    .or(body["error"].as_str())
                    .unwrap_or("no details")
            ),
        });
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
        token(&pending.token_endpoint, &form, Utc::now())
            .await
            .map_err(|r| r.message)?;
    Ok(Grant {
        token_endpoint: pending.token_endpoint.clone(),
        client_id: pending.client_id.clone(),
        client_secret: pending.client_secret.clone(),
        resource: pending.resource.clone(),
        access_token,
        refresh_token,
        expires_at,
        lapsed_at: None,
    })
}

/// A fresh access token from the refresh token. A server that rotates refresh tokens gives a
/// new one each time, and the old one stops working: the caller keeps the new grant.
pub async fn refresh(grant: &Grant) -> Result<Grant, RefreshError> {
    let refresh_token = grant.refresh_token.as_deref().ok_or_else(|| {
        RefreshError::Lapsed("the sign-in ran out and gave no refresh token".to_owned())
    })?;
    let mut form = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", grant.client_id.as_str()),
        ("resource", grant.resource.as_str()),
    ];
    if let Some(secret) = &grant.client_secret {
        form.push(("client_secret", secret));
    }
    let (access_token, new_refresh, expires_at) = token(&grant.token_endpoint, &form, Utc::now())
        .await
        .map_err(|refused| {
            // RFC 6749 §5.2: the grant or the client is no longer good; retrying cannot help.
            let lapsed = matches!(
                refused.code.as_deref(),
                Some("invalid_grant" | "invalid_client" | "unauthorized_client")
            ) || refused.status == reqwest::StatusCode::UNAUTHORIZED;
            if lapsed {
                RefreshError::Lapsed(refused.message)
            } else {
                RefreshError::Failed(refused.message)
            }
        })?;
    Ok(Grant {
        access_token,
        refresh_token: new_refresh.or_else(|| grant.refresh_token.clone()),
        expires_at,
        lapsed_at: None,
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

    #[test]
    fn a_parameter_is_not_read_from_the_end_of_another() {
        let header = r#"Bearer resource_metadata="https://h/.well-known/oauth-protected-resource/mcp/default", scope="mcp""#;
        assert_eq!(parameter(header, "scope").as_deref(), Some("mcp"));
        assert_eq!(
            parameter("Bearer xscope=a, scope=b", "scope").as_deref(),
            Some("b")
        );
        assert_eq!(parameter("Bearer xscope=a", "scope"), None);
    }

    /// An enterprise search's MCP server, shaped like one seen in the wild: its resource at a
    /// path, the resource metadata named in the challenge, the authorization server at a path
    /// of the same host with path-inserted metadata, open registration for public clients,
    /// PKCE with S256 only, many scopes, and rotating refresh tokens.
    mod search_server {
        use std::collections::BTreeMap;
        use std::sync::{Arc, Mutex};

        use axum::extract::{Form, State};
        use axum::http::StatusCode;
        use axum::response::IntoResponse;
        use axum::routing::{get, post};
        use axum::{Json, Router};
        use serde_json::json;

        #[derive(Default)]
        pub struct Seen {
            pub base: String,
            pub registration: serde_json::Value,
            pub token_forms: Vec<BTreeMap<String, String>>,
            /// The refresh token that works now; each refresh gives a new one.
            pub refresh: Option<String>,
            pub refreshes: u32,
        }

        pub type Shared = Arc<Mutex<Seen>>;

        pub async fn start() -> (String, Shared) {
            let shared: Shared = Arc::default();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            shared.lock().unwrap().base = base.clone();
            let app = Router::new()
                .route(
                    "/mcp/default",
                    post(|State(s): State<Shared>| async move {
                        let base = s.lock().unwrap().base.clone();
                        (
                            StatusCode::UNAUTHORIZED,
                            [(
                                "www-authenticate",
                                format!(
                                    "Bearer resource_metadata=\"{base}/.well-known/oauth-protected-resource/mcp/default\", scope=\"mcp\""
                                ),
                            )],
                        )
                    }),
                )
                .route(
                    "/.well-known/oauth-protected-resource/mcp/default",
                    get(|State(s): State<Shared>| async move {
                        let base = s.lock().unwrap().base.clone();
                        Json(json!({
                            "resource": format!("{base}/mcp/default"),
                            "authorization_servers": [format!("{base}/oauth")],
                            "scopes_supported": ["mcp"],
                            "bearer_methods_supported": ["header"],
                        }))
                    }),
                )
                .route(
                    "/.well-known/oauth-authorization-server/oauth",
                    get(|State(s): State<Shared>| async move {
                        let base = s.lock().unwrap().base.clone();
                        Json(json!({
                            "issuer": format!("{base}/oauth"),
                            "authorization_endpoint": format!("{base}/oauth/authorize"),
                            "token_endpoint": format!("{base}/oauth/token"),
                            "registration_endpoint": format!("{base}/oauth/register"),
                            "code_challenge_methods_supported": ["S256"],
                            "grant_types_supported": ["authorization_code", "client_credentials", "refresh_token"],
                            "response_types_supported": ["code"],
                            "token_endpoint_auth_methods_supported": ["client_secret_basic", "client_secret_post", "none"],
                            "scopes_supported": ["openid", "email", "profile", "offline_access", "mcp", "search", "chat", "documents", "people"],
                        }))
                    }),
                )
                // A root document of another issuer, which must not be taken for this one.
                .route(
                    "/.well-known/oauth-authorization-server",
                    get(|| async {
                        Json(json!({
                            "issuer": "https://elsewhere.example",
                            "authorization_endpoint": "https://elsewhere.example/authorize",
                            "token_endpoint": "https://elsewhere.example/token",
                        }))
                    }),
                )
                .route(
                    "/oauth/register",
                    post(|State(s): State<Shared>, Json(body): Json<serde_json::Value>| async move {
                        s.lock().unwrap().registration = body.clone();
                        (
                            StatusCode::CREATED,
                            Json(json!({
                                "client_id": "public-client-1",
                                "token_endpoint_auth_method": body["token_endpoint_auth_method"],
                                "redirect_uris": body["redirect_uris"],
                            })),
                        )
                    }),
                )
                .route(
                    "/oauth/token",
                    post(|State(s): State<Shared>, Form(form): Form<BTreeMap<String, String>>| async move {
                        let mut seen = s.lock().unwrap();
                        seen.token_forms.push(form.clone());
                        let bad = |error: &str| {
                            (StatusCode::BAD_REQUEST, Json(json!({ "error": error, "error_description": "refresh token is not valid" })))
                                .into_response()
                        };
                        match form["grant_type"].as_str() {
                            "authorization_code" => {
                                seen.refresh = Some("refresh-1".to_owned());
                                Json(json!({ "access_token": "access-1", "refresh_token": "refresh-1", "expires_in": 1, "token_type": "Bearer" }))
                                    .into_response()
                            }
                            "refresh_token" if seen.refresh.as_deref() == Some(form["refresh_token"].as_str()) => {
                                seen.refreshes += 1;
                                let next = format!("refresh-{}", seen.refreshes + 1);
                                seen.refresh = Some(next.clone());
                                Json(json!({ "access_token": format!("access-{}", seen.refreshes + 1), "refresh_token": next, "expires_in": 3600, "token_type": "Bearer" }))
                                    .into_response()
                            }
                            "refresh_token" => bad("invalid_grant"),
                            _ => bad("unsupported_grant_type"),
                        }
                    }),
                )
                .with_state(shared.clone());
            tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            (base, shared)
        }
    }

    #[tokio::test]
    async fn signs_in_to_a_search_server_with_its_metadata() {
        let (base, seen) = search_server::start().await;
        let server = format!("{base}/mcp/default");

        let found = discover(&server).await.unwrap();
        assert_eq!(
            found.authorization_endpoint,
            format!("{base}/oauth/authorize")
        );
        assert_eq!(found.token_endpoint, format!("{base}/oauth/token"));
        assert_eq!(
            found.registration_endpoint.as_deref(),
            Some(format!("{base}/oauth/register").as_str())
        );
        // What the challenge asks for, and a refresh token; not every scope the server knows.
        assert_eq!(found.scopes, ["mcp", "offline_access"]);
        assert_eq!(found.resource, server);

        let redirect = "https://frood.example/v1/connections/signed-in";
        let (client_id, secret) = register(
            found.registration_endpoint.as_deref().unwrap(),
            redirect,
            "work",
        )
        .await
        .unwrap();
        assert_eq!((client_id.as_str(), secret), ("public-client-1", None));
        let registration = seen.lock().unwrap().registration.clone();
        assert_eq!(registration["token_endpoint_auth_method"], "none");
        assert_eq!(
            registration["grant_types"],
            serde_json::json!(["authorization_code", "refresh_token"])
        );
        assert_eq!(registration["redirect_uris"], serde_json::json!([redirect]));

        let (address, pending) =
            authorize_url(&found, &client_id, None, redirect, "state-1").unwrap();
        let address = url::Url::parse(&address).unwrap();
        let query: std::collections::BTreeMap<String, String> =
            address.query_pairs().into_owned().collect();
        assert_eq!(query["response_type"], "code");
        assert_eq!(query["client_id"], "public-client-1");
        assert_eq!(query["code_challenge_method"], "S256");
        assert_eq!(
            query["code_challenge"],
            b64(&Sha256::digest(pending.verifier.as_bytes()))
        );
        assert_eq!(query["resource"], server);
        assert_eq!(query["scope"], "mcp offline_access");
        assert_eq!(query["state"], "state-1");

        let grant = exchange(&pending, "code-1").await.unwrap();
        assert_eq!(grant.access_token, "access-1");
        assert_eq!(grant.refresh_token.as_deref(), Some("refresh-1"));
        assert!(grant.stale(Utc::now()));
        {
            let seen = seen.lock().unwrap();
            let form = &seen.token_forms[0];
            assert_eq!(form["code_verifier"], pending.verifier);
            assert_eq!(form["resource"], server);
            assert_eq!(form["client_id"], "public-client-1");
            assert!(!form.contains_key("client_secret"));
        }

        // Refreshing rotates the refresh token: the new grant carries the new one.
        let fresh = refresh(&grant).await.unwrap();
        assert_eq!(fresh.access_token, "access-2");
        assert_eq!(fresh.refresh_token.as_deref(), Some("refresh-2"));
        assert!(!fresh.stale(Utc::now()));
        assert_eq!(seen.lock().unwrap().token_forms[1]["resource"], server);
        let again = refresh(&fresh).await.unwrap();
        assert_eq!(again.refresh_token.as_deref(), Some("refresh-3"));

        // The old refresh token no longer works: only a new sign-in helps.
        match refresh(&grant).await {
            Err(RefreshError::Lapsed(why)) => assert!(
                why.contains("invalid_grant") || why.contains("not valid"),
                "{why}"
            ),
            other => panic!("expected a lapsed sign-in, got {other:?}"),
        }
        let none = Grant {
            refresh_token: None,
            ..grant.clone()
        };
        assert!(none.needs_sign_in(Utc::now()));
        assert!(matches!(refresh(&none).await, Err(RefreshError::Lapsed(_))));
    }

    #[tokio::test]
    async fn an_unreachable_token_endpoint_is_not_a_lapsed_sign_in() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let grant = Grant {
            token_endpoint: format!("http://{address}/token"),
            client_id: "c".to_owned(),
            client_secret: None,
            resource: "https://search.example/mcp/default".to_owned(),
            access_token: "a".to_owned(),
            refresh_token: Some("r".to_owned()),
            expires_at: Some(Utc::now()),
            lapsed_at: None,
        };
        assert!(matches!(
            refresh(&grant).await,
            Err(RefreshError::Failed(_))
        ));
        assert!(!grant.needs_sign_in(Utc::now()));
    }
}
