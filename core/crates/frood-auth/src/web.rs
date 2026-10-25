//! HTTP endpoints of the authorization server: discovery metadata, /oauth/authorize with
//! sign-in and consent, /oauth/token, /oauth/register, /oauth/revoke.

use axum::extract::{Path, Query, RawForm, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Row;
use url::Url;
use uuid::Uuid;

use crate::clients::{RegistrationRequest, redirect_allowed};
use crate::oidc::OidcProvider;
use crate::pages::{self, Consent};
use crate::secret::{hash, new_secret, pkce_challenge, pkce_matches};
use crate::{Auth, AuthError, Resource, scope};

const SESSION_COOKIE: &str = "frood_session";
const SESSION_DAYS: i32 = 7;

pub fn routes(auth: Auth) -> Router {
    Router::new()
        .route(
            "/.well-known/oauth-authorization-server",
            get(server_metadata),
        )
        .route(
            "/.well-known/oauth-authorization-server/{*issuer_path}",
            get(server_metadata_at),
        )
        .route(
            "/.well-known/oauth-protected-resource",
            get(api_resource_metadata),
        )
        .route(
            "/.well-known/oauth-protected-resource/{*resource}",
            get(resource_metadata_at),
        )
        .route("/oauth/authorize", get(authorize))
        .route("/oauth/login", get(choose_provider))
        .route(
            "/oauth/login/{provider}",
            get(provider_start).post(dev_login),
        )
        .route("/oauth/callback/{provider}", get(provider_callback))
        .route("/oauth/consent", get(consent_page).post(consent_submit))
        .route("/oauth/token", post(token))
        .route("/oauth/register", post(register))
        .route("/oauth/revoke", post(revoke))
        .with_state(auth)
}

async fn server_metadata(State(auth): State<Auth>) -> Json<Value> {
    let issuer = &auth.settings().issuer;
    Json(json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{issuer}/oauth/authorize"),
        "token_endpoint": format!("{issuer}/oauth/token"),
        "registration_endpoint": format!("{issuer}/oauth/register"),
        "revocation_endpoint": format!("{issuer}/oauth/revoke"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "revocation_endpoint_auth_methods_supported": ["none"],
        "scopes_supported": scope::ALL,
        "client_id_metadata_document_supported": true,
        "authorization_response_iss_parameter_supported": true,
    }))
}

fn resource_metadata(auth: &Auth, resource: Resource) -> Json<Value> {
    let settings = auth.settings();
    // Clients such as Claude Code request every scope listed here: list only what the
    // resource uses. MCP tools need no journal or admin rights.
    let scopes: &[&str] = match resource {
        Resource::Api => &scope::ALL,
        Resource::Mcp => &[scope::READ, scope::CAPTURE, scope::PROPOSE, scope::DECIDE],
    };
    Json(json!({
        "resource": settings.resource_url(resource),
        "authorization_servers": [settings.issuer],
        "scopes_supported": scopes,
        "bearer_methods_supported": ["header"],
        "resource_name": format!("Frood {}", settings.installation),
    }))
}

/// RFC 8414 for an issuer with a path: `/.well-known/oauth-authorization-server/frood`
/// reaches the server at the root of the host, without the proxy stripping the path.
async fn server_metadata_at(State(auth): State<Auth>, Path(issuer_path): Path<String>) -> Response {
    if issuer_path.trim_end_matches('/') != auth.settings().base_path.trim_start_matches('/') {
        return StatusCode::NOT_FOUND.into_response();
    }
    server_metadata(State(auth)).await.into_response()
}

async fn api_resource_metadata(State(auth): State<Auth>) -> Json<Value> {
    resource_metadata(&auth, Resource::Api)
}

/// `…/v1` and `…/mcp`, with the issuer's path in front (RFC 9728, from the root of the host)
/// or without it (behind the proxy, which stripped it).
async fn resource_metadata_at(State(auth): State<Auth>, Path(resource): Path<String>) -> Response {
    let base = auth.settings().base_path.trim_start_matches('/');
    let name = match resource
        .strip_prefix(base)
        .and_then(|r| r.strip_prefix('/'))
    {
        Some(rest) if !base.is_empty() => rest,
        _ => resource.as_str(),
    };
    match name {
        "v1" => resource_metadata(&auth, Resource::Api).into_response(),
        "mcp" => resource_metadata(&auth, Resource::Mcp).into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

#[derive(Deserialize)]
struct AuthorizeQuery {
    response_type: Option<String>,
    client_id: Option<String>,
    redirect_uri: Option<String>,
    code_challenge: Option<String>,
    code_challenge_method: Option<String>,
    scope: Option<String>,
    state: Option<String>,
    resource: Option<String>,
}

fn error_page(status: StatusCode, message: &str) -> Response {
    (status, pages::error(message)).into_response()
}

/// Redirects back to the client with an OAuth error (only after the redirect URI is trusted).
fn client_error(
    auth: &Auth,
    redirect_uri: &str,
    state: Option<&str>,
    error: &str,
    description: &str,
) -> Response {
    match Url::parse(redirect_uri) {
        Ok(mut url) => {
            {
                let mut pairs = url.query_pairs_mut();
                pairs
                    .append_pair("error", error)
                    .append_pair("error_description", description);
                if let Some(state) = state {
                    pairs.append_pair("state", state);
                }
                pairs.append_pair("iss", &auth.settings().issuer);
            }
            Redirect::to(url.as_str()).into_response()
        }
        Err(_) => error_page(StatusCode::BAD_REQUEST, description),
    }
}

async fn authorize(
    State(auth): State<Auth>,
    headers: HeaderMap,
    Query(query): Query<AuthorizeQuery>,
) -> Response {
    let (Some(client_id), Some(redirect_uri)) =
        (query.client_id.as_deref(), query.redirect_uri.as_deref())
    else {
        return error_page(
            StatusCode::BAD_REQUEST,
            "The request has no client_id or redirect_uri.",
        );
    };
    let client = match auth.resolve_client(client_id).await {
        Ok(client) => client,
        Err(err) => return error_page(StatusCode::BAD_REQUEST, &err.to_string()),
    };
    if !redirect_allowed(&client.redirect_uris, redirect_uri) {
        return error_page(
            StatusCode::BAD_REQUEST,
            "The redirect URI is not registered for this client.",
        );
    }
    let state = query.state.as_deref();
    let fail = |error: &str, description: &str| {
        client_error(&auth, redirect_uri, state, error, description)
    };

    if query.response_type.as_deref() != Some("code") {
        return fail(
            "unsupported_response_type",
            "only response_type=code is supported",
        );
    }
    let Some(challenge) = query
        .code_challenge
        .as_deref()
        .filter(|c| (43..=128).contains(&c.len()))
    else {
        return fail("invalid_request", "PKCE code_challenge is required");
    };
    if query.code_challenge_method.as_deref() != Some("S256") {
        return fail("invalid_request", "code_challenge_method must be S256");
    }
    let requested: Vec<String> = match query
        .scope
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        None => scope::CLIENT_DEFAULT
            .iter()
            .map(|s| (*s).to_owned())
            .collect(),
        Some(list) => list.split_whitespace().map(str::to_owned).collect(),
    };
    if let Some(unknown) = requested.iter().find(|s| !scope::ALL.contains(&s.as_str())) {
        return fail("invalid_scope", &format!("unknown scope {unknown}"));
    }
    let settings = auth.settings();
    if let Some(resource) = &query.resource {
        let known = [
            settings.issuer.clone(),
            settings.resource_url(Resource::Api),
            settings.resource_url(Resource::Mcp),
        ];
        if !known.contains(&resource.trim_end_matches('/').to_owned()) {
            return fail("invalid_target", "unknown resource");
        }
    }

    let tx = new_secret("tx_");
    let session_user = auth.session_user(&headers).await.ok().flatten();
    let stored = sqlx::query(
        "INSERT INTO auth_requests (id_hash, client_id, client_name, redirect_uri, code_challenge, scopes, resource, state, user_id, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, now() + interval '10 minutes')",
    )
    .bind(hash(&tx))
    .bind(client_id)
    .bind(&client.name)
    .bind(redirect_uri)
    .bind(challenge)
    .bind(&requested)
    .bind(query.resource.as_deref().map(|r| r.trim_end_matches('/')))
    .bind(state)
    .bind(session_user)
    .execute(auth.pool())
    .await;
    if let Err(err) = stored {
        tracing::error!(error = %err, "cannot store authorization request");
        return fail("server_error", "internal error");
    }
    let _ = sqlx::query("DELETE FROM auth_requests WHERE expires_at < now() - interval '1 hour'")
        .execute(auth.pool())
        .await;
    let next = if session_user.is_some() {
        "consent"
    } else {
        "login"
    };
    Redirect::to(&auth.settings().path(&format!("/oauth/{next}?tx={tx}"))).into_response()
}

struct PendingRequest {
    client_id: String,
    client_name: Option<String>,
    redirect_uri: String,
    code_challenge: String,
    scopes: Vec<String>,
    resource: Option<String>,
    state: Option<String>,
    user_id: Option<Uuid>,
    provider: Option<String>,
    upstream_nonce: Option<String>,
    upstream_verifier: Option<String>,
}

impl Auth {
    async fn pending(&self, column: &str, secret: &str) -> Result<PendingRequest, AuthError> {
        let sql = match column {
            "tx" => "SELECT * FROM auth_requests WHERE id_hash = $1 AND expires_at > now()",
            _ => {
                "SELECT * FROM auth_requests WHERE upstream_state_hash = $1 AND expires_at > now()"
            }
        };
        let row = sqlx::query(sql)
            .bind(hash(secret))
            .fetch_optional(self.pool())
            .await?
            .ok_or_else(|| {
                AuthError::Invalid("This sign-in request expired or is unknown.".into())
            })?;
        Ok(PendingRequest {
            client_id: row.try_get("client_id")?,
            client_name: row.try_get("client_name")?,
            redirect_uri: row.try_get("redirect_uri")?,
            code_challenge: row.try_get("code_challenge")?,
            scopes: row.try_get("scopes")?,
            resource: row.try_get("resource")?,
            state: row.try_get("state")?,
            user_id: row.try_get("user_id")?,
            provider: row.try_get("provider")?,
            upstream_nonce: row.try_get("upstream_nonce")?,
            upstream_verifier: row.try_get("upstream_verifier")?,
        })
    }

    async fn create_session(&self, user_id: Uuid) -> Result<HeaderValue, AuthError> {
        let secret = new_secret("frood_sess_");
        sqlx::query("INSERT INTO sessions (id_hash, user_id, expires_at) VALUES ($1, $2, now() + make_interval(days => $3))")
            .bind(hash(&secret))
            .bind(user_id)
            .bind(SESSION_DAYS)
            .execute(self.pool())
            .await?;
        let secure = if self.settings().secure_cookies() {
            "; Secure"
        } else {
            ""
        };
        let cookie = format!(
            "{SESSION_COOKIE}={secret}; Path={}; HttpOnly; SameSite=Lax; Max-Age={}{secure}",
            self.settings().path("/oauth"),
            SESSION_DAYS * 86_400
        );
        HeaderValue::from_str(&cookie)
            .map_err(|_| AuthError::Invalid("cannot build session cookie".into()))
    }

    async fn session_user(&self, headers: &HeaderMap) -> Result<Option<Uuid>, AuthError> {
        let Some(secret) = headers
            .get_all(header::COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(';'))
            .filter_map(|pair| pair.trim().split_once('='))
            .find(|(name, _)| *name == SESSION_COOKIE)
            .map(|(_, value)| value.to_owned())
        else {
            return Ok(None);
        };
        Ok(sqlx::query_scalar(
            "SELECT user_id FROM sessions WHERE id_hash = $1 AND expires_at > now()",
        )
        .bind(hash(&secret))
        .fetch_optional(self.pool())
        .await?)
    }

    /// Browser POSTs must come from this installation's own pages.
    fn same_origin(&self, headers: &HeaderMap) -> bool {
        match headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
            None => true,
            Some(origin) => Url::parse(&self.settings().issuer)
                .map(|issuer| issuer.origin().ascii_serialization() == origin)
                .unwrap_or(false),
        }
    }

    async fn attach_user(&self, tx: &str, user_id: Uuid) -> Result<(), AuthError> {
        sqlx::query("UPDATE auth_requests SET user_id = $2 WHERE id_hash = $1")
            .bind(hash(tx))
            .bind(user_id)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    fn provider(&self, id: &str) -> Option<&frood_config::Provider> {
        self.settings().providers.iter().find(|p| p.id() == id)
    }
}

#[derive(Deserialize)]
struct TxQuery {
    tx: String,
}

async fn choose_provider(
    State(auth): State<Auth>,
    Query(TxQuery { tx }): Query<TxQuery>,
) -> Response {
    if let Err(err) = auth.pending("tx", &tx).await {
        return error_page(StatusCode::BAD_REQUEST, &err.to_string());
    }
    let providers: Vec<(String, String)> = auth
        .settings()
        .providers
        .iter()
        .map(|p| (p.id().to_owned(), p.label().to_owned()))
        .collect();
    match providers.as_slice() {
        [] => error_page(
            StatusCode::SERVICE_UNAVAILABLE,
            "No sign-in providers are configured (auth.providers in frood.yaml).",
        ),
        [(only, _)] => Redirect::to(
            &auth
                .settings()
                .path(&format!("/oauth/login/{only}?tx={tx}")),
        )
        .into_response(),
        _ => pages::choose_provider(
            &auth.settings().installation,
            &auth.settings().base_path,
            &providers,
            &tx,
        )
        .into_response(),
    }
}

async fn provider_start(
    State(auth): State<Auth>,
    Path(provider_id): Path<String>,
    Query(TxQuery { tx }): Query<TxQuery>,
) -> Response {
    if let Err(err) = auth.pending("tx", &tx).await {
        return error_page(StatusCode::BAD_REQUEST, &err.to_string());
    }
    match auth.provider(&provider_id) {
        None => error_page(StatusCode::NOT_FOUND, "Unknown sign-in provider."),
        Some(frood_config::Provider::Dev { id, .. }) => pages::dev_form(
            &auth.settings().installation,
            &auth.settings().base_path,
            id,
            &tx,
        )
        .into_response(),
        Some(frood_config::Provider::Local { id, .. }) => {
            let settings = auth.settings();
            // One person: their address is already filled in.
            let only = match settings.allow.as_slice() {
                [only] => only.as_str(),
                _ => "",
            };
            pages::local_form(&settings.installation, &settings.base_path, id, &tx, only)
                .into_response()
        }
        Some(frood_config::Provider::Oidc {
            id,
            issuer,
            client_id,
            client_secret,
            ..
        }) => {
            let provider = OidcProvider {
                id,
                issuer,
                client_id,
                client_secret,
            };
            let meta = match auth.provider_metadata(&provider).await {
                Ok(meta) => meta,
                Err(err) => return error_page(StatusCode::BAD_GATEWAY, &err.to_string()),
            };
            let (state, nonce, verifier) = (new_secret("st_"), new_secret("n_"), new_secret("v_"));
            let saved = sqlx::query(
                "UPDATE auth_requests SET provider = $2, upstream_state_hash = $3, upstream_nonce = $4, upstream_verifier = $5 WHERE id_hash = $1",
            )
            .bind(hash(&tx))
            .bind(id)
            .bind(hash(&state))
            .bind(&nonce)
            .bind(&verifier)
            .execute(auth.pool())
            .await;
            if saved.is_err() {
                return error_page(StatusCode::INTERNAL_SERVER_ERROR, "Cannot start sign-in.");
            }
            let callback = format!("{}/oauth/callback/{id}", auth.settings().issuer);
            match Auth::authorization_url(
                &meta,
                &provider,
                &callback,
                &state,
                &nonce,
                &pkce_challenge(&verifier),
            ) {
                Ok(url) => Redirect::to(&url).into_response(),
                Err(err) => error_page(StatusCode::BAD_GATEWAY, &err.to_string()),
            }
        }
    }
}

fn form_values(form: &[u8]) -> Vec<(String, String)> {
    url::form_urlencoded::parse(form).into_owned().collect()
}

fn form_value<'a>(values: &'a [(String, String)], key: &str) -> Option<&'a str> {
    values
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

async fn dev_login(
    State(auth): State<Auth>,
    Path(provider_id): Path<String>,
    headers: HeaderMap,
    RawForm(form): RawForm,
) -> Response {
    let local = match auth.provider(&provider_id) {
        Some(frood_config::Provider::Dev { .. }) => false,
        Some(frood_config::Provider::Local { .. }) => true,
        _ => return error_page(StatusCode::NOT_FOUND, "Unknown sign-in provider."),
    };
    if !auth.same_origin(&headers) {
        return error_page(StatusCode::FORBIDDEN, "Cross-site request refused.");
    }
    let values = form_values(&form);
    let (Some(tx), Some(email)) = (form_value(&values, "tx"), form_value(&values, "email")) else {
        return error_page(StatusCode::BAD_REQUEST, "The form is incomplete.");
    };
    if let Err(err) = auth.pending("tx", tx).await {
        return error_page(StatusCode::BAD_REQUEST, &err.to_string());
    }
    // On this computer only the listed addresses, not whole domains.
    if local && !auth.settings().allow.contains(&email.trim().to_lowercase()) {
        return error_page(
            StatusCode::FORBIDDEN,
            "This address is not in auth.allow of this installation.",
        );
    }
    finish_sign_in(&auth, tx, email, None).await
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

async fn provider_callback(
    State(auth): State<Auth>,
    Path(provider_id): Path<String>,
    Query(query): Query<CallbackQuery>,
) -> Response {
    let Some(state) = query.state.as_deref() else {
        return error_page(StatusCode::BAD_REQUEST, "The provider returned no state.");
    };
    let request = match auth.pending("upstream", state).await {
        Ok(request) if request.provider.as_deref() == Some(provider_id.as_str()) => request,
        Ok(_) => {
            return error_page(
                StatusCode::BAD_REQUEST,
                "The provider does not match this sign-in.",
            );
        }
        Err(err) => return error_page(StatusCode::BAD_REQUEST, &err.to_string()),
    };
    if let Some(error) = &query.error {
        let description = query.error_description.as_deref().unwrap_or(error);
        return error_page(
            StatusCode::FORBIDDEN,
            &format!("The provider refused the sign-in: {description}"),
        );
    }
    let Some(frood_config::Provider::Oidc {
        id,
        issuer,
        client_id,
        client_secret,
        ..
    }) = auth.provider(&provider_id)
    else {
        return error_page(StatusCode::NOT_FOUND, "Unknown sign-in provider.");
    };
    let provider = OidcProvider {
        id,
        issuer,
        client_id,
        client_secret,
    };
    let (Some(code), Some(nonce), Some(verifier)) = (
        query.code.as_deref(),
        request.upstream_nonce.as_deref(),
        request.upstream_verifier.as_deref(),
    ) else {
        return error_page(
            StatusCode::BAD_REQUEST,
            "The sign-in response is incomplete.",
        );
    };
    let callback = format!("{}/oauth/callback/{id}", auth.settings().issuer);
    let identity = match auth
        .finish_provider_login(&provider, code, &callback, verifier, nonce)
        .await
    {
        Ok(identity) => identity,
        Err(err) => return error_page(StatusCode::BAD_GATEWAY, &err.to_string()),
    };
    // The request is found by the upstream state; consent continues with a fresh tx for it.
    let tx = new_secret("tx_");
    if sqlx::query("UPDATE auth_requests SET id_hash = $2, upstream_state_hash = NULL WHERE upstream_state_hash = $1")
        .bind(hash(state))
        .bind(hash(&tx))
        .execute(auth.pool())
        .await
        .is_err()
    {
        return error_page(StatusCode::INTERNAL_SERVER_ERROR, "Cannot continue sign-in.");
    }
    finish_sign_in(&auth, &tx, &identity.email, identity.name.as_deref()).await
}

async fn finish_sign_in(auth: &Auth, tx: &str, email: &str, name: Option<&str>) -> Response {
    let user_id = match auth.sign_in(email, name).await {
        Ok(id) => id,
        Err(AuthError::Invalid(message)) => return error_page(StatusCode::FORBIDDEN, &message),
        Err(err) => return error_page(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()),
    };
    let cookie = match auth.create_session(user_id).await {
        Ok(cookie) => cookie,
        Err(err) => return error_page(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()),
    };
    if auth.attach_user(tx, user_id).await.is_err() {
        return error_page(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Cannot continue sign-in.",
        );
    }
    let mut response =
        Redirect::to(&auth.settings().path(&format!("/oauth/consent?tx={tx}"))).into_response();
    response.headers_mut().append(header::SET_COOKIE, cookie);
    response
}

async fn consent_page(
    State(auth): State<Auth>,
    headers: HeaderMap,
    Query(TxQuery { tx }): Query<TxQuery>,
) -> Response {
    let request = match auth.pending("tx", &tx).await {
        Ok(request) => request,
        Err(err) => return error_page(StatusCode::BAD_REQUEST, &err.to_string()),
    };
    let session = auth.session_user(&headers).await.ok().flatten();
    let Some(user_id) = session.filter(|s| Some(*s) == request.user_id) else {
        return Redirect::to(&auth.settings().path(&format!("/oauth/login?tx={tx}")))
            .into_response();
    };
    let email: String = match sqlx::query_scalar("SELECT email FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_one(auth.pool())
        .await
    {
        Ok(email) => email,
        Err(_) => return error_page(StatusCode::INTERNAL_SERVER_ERROR, "Unknown user."),
    };
    pages::consent(&Consent {
        installation: &auth.settings().installation,
        base: &auth.settings().base_path,
        client: request.client_name.as_deref().unwrap_or(&request.client_id),
        client_id: &request.client_id,
        email: &email,
        requested: &request.scopes,
        tx: &tx,
    })
    .into_response()
}

async fn consent_submit(
    State(auth): State<Auth>,
    headers: HeaderMap,
    RawForm(form): RawForm,
) -> Response {
    if !auth.same_origin(&headers) {
        return error_page(StatusCode::FORBIDDEN, "Cross-site request refused.");
    }
    let values = form_values(&form);
    let Some(tx) = form_value(&values, "tx") else {
        return error_page(StatusCode::BAD_REQUEST, "The form is incomplete.");
    };
    let request = match auth.pending("tx", tx).await {
        Ok(request) => request,
        Err(err) => return error_page(StatusCode::BAD_REQUEST, &err.to_string()),
    };
    let session = auth.session_user(&headers).await.ok().flatten();
    let Some(user_id) = session.filter(|s| Some(*s) == request.user_id) else {
        return error_page(StatusCode::FORBIDDEN, "Sign in first.");
    };
    let _ = sqlx::query("DELETE FROM auth_requests WHERE id_hash = $1")
        .bind(hash(tx))
        .execute(auth.pool())
        .await;
    let state = request.state.as_deref();
    if form_value(&values, "decision") != Some("allow") {
        return client_error(
            &auth,
            &request.redirect_uri,
            state,
            "access_denied",
            "the person denied access",
        );
    }
    let admin_requested = request.scopes.iter().any(|s| s == scope::ADMIN);
    let mut granted: Vec<String> = values
        .iter()
        .filter(|(k, _)| k == "scope")
        .map(|(_, v)| v.clone())
        .filter(|s| scope::ALL.contains(&s.as_str()) && (s != scope::ADMIN || admin_requested))
        .collect();
    granted.sort();
    granted.dedup();
    if granted.is_empty() {
        return client_error(
            &auth,
            &request.redirect_uri,
            state,
            "access_denied",
            "no permissions were granted",
        );
    }
    let code = new_secret("frood_code_");
    let stored = sqlx::query(
        "INSERT INTO auth_codes (code_hash, client_id, redirect_uri, code_challenge, scopes, resource, user_id, family, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, now() + interval '60 seconds')",
    )
    .bind(hash(&code))
    .bind(&request.client_id)
    .bind(&request.redirect_uri)
    .bind(&request.code_challenge)
    .bind(&granted)
    .bind(&request.resource)
    .bind(user_id)
    .bind(Uuid::now_v7())
    .execute(auth.pool())
    .await;
    if stored.is_err() {
        return client_error(
            &auth,
            &request.redirect_uri,
            state,
            "server_error",
            "internal error",
        );
    }
    let mut url = match Url::parse(&request.redirect_uri) {
        Ok(url) => url,
        Err(_) => return error_page(StatusCode::BAD_REQUEST, "Invalid redirect URI."),
    };
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("code", &code);
        if let Some(state) = state {
            pairs.append_pair("state", state);
        }
        pairs.append_pair("iss", &auth.settings().issuer);
    }
    Redirect::to(url.as_str()).into_response()
}

fn token_error(error: &str, description: &str) -> Response {
    let mut response = (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": error, "error_description": description })),
    )
        .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn token(State(auth): State<Auth>, RawForm(form): RawForm) -> Response {
    let values = form_values(&form);
    let get = |key: &str| form_value(&values, key);
    let result = match get("grant_type") {
        Some("authorization_code") => exchange_code(&auth, &values).await,
        Some("refresh_token") => match (get("refresh_token"), get("client_id")) {
            (Some(refresh), Some(client_id)) => auth.refresh(refresh, client_id).await,
            _ => {
                return token_error(
                    "invalid_request",
                    "refresh_token and client_id are required",
                );
            }
        },
        _ => {
            return token_error(
                "unsupported_grant_type",
                "use authorization_code or refresh_token",
            );
        }
    };
    match result {
        Ok(pair) => {
            let mut response = Json(pair).into_response();
            response
                .headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            response
        }
        Err(AuthError::Invalid(message)) => {
            let (code, description) = message
                .split_once(": ")
                .unwrap_or(("invalid_request", message.as_str()));
            token_error(code, description)
        }
        Err(err) => {
            tracing::error!(error = %err, "token endpoint failure");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "server_error" })),
            )
                .into_response()
        }
    }
}

async fn exchange_code(
    auth: &Auth,
    values: &[(String, String)],
) -> Result<crate::TokenPair, AuthError> {
    let get = |key: &str| form_value(values, key);
    let (Some(code), Some(redirect_uri), Some(client_id), Some(verifier)) = (
        get("code"),
        get("redirect_uri"),
        get("client_id"),
        get("code_verifier"),
    ) else {
        return Err(AuthError::Invalid(
            "invalid_request: code, redirect_uri, client_id and code_verifier are required".into(),
        ));
    };
    let mut tx = auth.pool().begin().await?;
    let row = sqlx::query(
        "SELECT client_id, redirect_uri, code_challenge, scopes, resource, user_id, family, used_at IS NOT NULL AS used, \
         expires_at <= now() AS expired FROM auth_codes WHERE code_hash = $1 FOR UPDATE",
    )
    .bind(hash(code))
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AuthError::Invalid("invalid_grant: unknown code".into()))?;
    let family: Uuid = row.try_get("family")?;
    if row.try_get::<bool, _>("used")? {
        // A replayed code: revoke whatever it produced.
        sqlx::query(
            "UPDATE tokens SET revoked_at = now() WHERE family = $1 AND revoked_at IS NULL",
        )
        .bind(family)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        return Err(AuthError::Invalid(
            "invalid_grant: code was already used".into(),
        ));
    }
    if row.try_get::<bool, _>("expired")? {
        return Err(AuthError::Invalid("invalid_grant: code expired".into()));
    }
    if row.try_get::<String, _>("client_id")? != client_id
        || row.try_get::<String, _>("redirect_uri")? != redirect_uri
    {
        return Err(AuthError::Invalid(
            "invalid_grant: client_id or redirect_uri does not match".into(),
        ));
    }
    if !pkce_matches(verifier, &row.try_get::<String, _>("code_challenge")?) {
        return Err(AuthError::Invalid(
            "invalid_grant: PKCE verification failed".into(),
        ));
    }
    let mut resource: Option<String> = row.try_get("resource")?;
    if let Some(requested) = get("resource").map(|r| r.trim_end_matches('/').to_owned()) {
        match &resource {
            Some(bound) if bound != &requested => {
                return Err(AuthError::Invalid(
                    "invalid_target: resource differs from the authorization request".into(),
                ));
            }
            _ => resource = Some(requested),
        }
    }
    sqlx::query("UPDATE auth_codes SET used_at = now() WHERE code_hash = $1")
        .bind(hash(code))
        .execute(&mut *tx)
        .await?;
    let user_id: Uuid = row.try_get("user_id")?;
    let scopes: Vec<String> = row.try_get("scopes")?;
    let pair = auth
        .issue_pair(
            &mut tx,
            user_id,
            client_id,
            &scopes,
            resource.as_deref(),
            family,
        )
        .await?;
    tx.commit().await?;
    Ok(pair)
}

async fn register(State(auth): State<Auth>, Json(request): Json<RegistrationRequest>) -> Response {
    match auth.register_client(request).await {
        Ok(client) => (
            StatusCode::CREATED,
            Json(json!({
                "client_id": client.client_id,
                "client_id_issued_at": chrono::Utc::now().timestamp(),
                "client_name": client.name,
                "redirect_uris": client.redirect_uris,
                "token_endpoint_auth_method": "none",
                "grant_types": ["authorization_code", "refresh_token"],
                "response_types": ["code"],
            })),
        )
            .into_response(),
        Err(AuthError::Invalid(message)) => {
            let (code, description) = message
                .split_once(": ")
                .unwrap_or(("invalid_client_metadata", message.as_str()));
            (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": code, "error_description": description })),
            )
                .into_response()
        }
        Err(err) => {
            tracing::error!(error = %err, "client registration failure");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "server_error" })),
            )
                .into_response()
        }
    }
}

async fn revoke(State(auth): State<Auth>, RawForm(form): RawForm) -> Response {
    let values = form_values(&form);
    if let Some(token) = form_value(&values, "token")
        && let Err(err) = auth.revoke_secret(token).await
    {
        tracing::error!(error = %err, "revocation failure");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    StatusCode::OK.into_response()
}
