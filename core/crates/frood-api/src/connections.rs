//! Connecting sources from the console (docs/26 §4): the list, a check of an MCP server with a
//! token, and connecting or disconnecting one. Changing them needs `admin`, like `frood apply`.

use std::collections::BTreeMap;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Redirect;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use frood_dispatch::connections::{
    Checked, ConnectError, Connection, JobChange, JobSetting, NewConnection, SignInSettings,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::AppState;
use crate::auth::{Admin, Caller};
use crate::error::ApiError;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/connections", get(list))
        .route("/v1/connections/check", post(check))
        .route("/v1/connections/signed-in", get(signed_in))
        .route("/v1/connections/{name}", put(connect).delete(disconnect))
        .route("/v1/connections/{name}/sign-in", post(sign_in))
        .route("/v1/jobs/settings", get(job_settings))
        .route("/v1/jobs/{name}", put(change_job).delete(reset_job))
}

/// Where the person comes back after signing in to a server.
fn return_address(state: &AppState) -> String {
    format!("{}/v1/connections/signed-in", state.auth.settings().issuer)
}

pub(crate) fn connect_error(err: ConnectError) -> ApiError {
    match err {
        ConnectError::Database(db) => ApiError::from(frood_ledger::LedgerError::from(db)),
        ConnectError::NotFound(_) => {
            ApiError::new(StatusCode::NOT_FOUND, "not-found", err.to_string())
        }
        ConnectError::InConfig(_) => {
            ApiError::new(StatusCode::CONFLICT, "in-config", err.to_string())
        }
        ConnectError::NoVault => ApiError::new(StatusCode::CONFLICT, "no-vault", err.to_string()),
        ConnectError::SignIn(_) => ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "sign-in-failed",
            err.to_string(),
        ),
        ConnectError::Invalid(_) => ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid-request",
            err.to_string(),
        ),
    }
}

/// Every source: `frood.yaml`'s and the console's, without tokens; and whether the console can
/// store tokens.
async fn list(
    State(state): State<AppState>,
    _caller: Caller<Admin>,
) -> Result<Json<Value>, ApiError> {
    let connections: Vec<Connection> = state
        .dispatch
        .connections()
        .await
        .map_err(|e| ApiError::from(frood_ledger::LedgerError::from(e)))?;
    Ok(Json(
        json!({ "sources": connections, "vault": state.dispatch.vault_open() }),
    ))
}

#[derive(Deserialize)]
struct CheckBody {
    server: String,
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    header: Option<String>,
    #[serde(default)]
    scheme: Option<String>,
    /// A source already connected from the console: its stored token is used when no token
    /// is given.
    #[serde(default)]
    name: Option<String>,
}

/// Connects to an MCP server and lists its tools, before anything is saved.
async fn check(
    State(state): State<AppState>,
    _caller: Caller<Admin>,
    Json(body): Json<CheckBody>,
) -> Result<Json<Checked>, ApiError> {
    let mut headers = BTreeMap::new();
    match body
        .token
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        Some(token) => {
            let header = body
                .header
                .filter(|h| !h.trim().is_empty())
                .unwrap_or_else(|| "Authorization".to_owned());
            let scheme = body.scheme.unwrap_or_else(|| {
                if header.eq_ignore_ascii_case("authorization") {
                    "Bearer ".to_owned()
                } else {
                    String::new()
                }
            });
            headers.insert(header, format!("{scheme}{token}"));
        }
        None => {
            if let Some(name) = body.name.as_deref()
                && let Some(source) = state.dispatch.signed_source(name).await
            {
                headers = source
                    .map_err(|e| {
                        ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "check-failed", e)
                    })?
                    .headers;
            }
        }
    }
    frood_dispatch::connections::check(body.server.trim(), &headers)
        .await
        .map(Json)
        .map_err(|e| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "check-failed", e))
}

async fn connect(
    State(state): State<AppState>,
    caller: Caller<Admin>,
    Path(name): Path<String>,
    Json(body): Json<NewConnection>,
) -> Result<Json<Value>, ApiError> {
    let revision = state
        .dispatch
        .connect(caller.actor().as_str(), &name, body)
        .await
        .map_err(connect_error)?;
    Ok(Json(json!({ "name": name, "revision": revision })))
}

async fn disconnect(
    State(state): State<AppState>,
    caller: Caller<Admin>,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let revision = state
        .dispatch
        .disconnect(caller.actor().as_str(), &name)
        .await
        .map_err(connect_error)?;
    Ok(Json(json!({ "name": name, "revision": revision })))
}

/// Starts signing in to the server (OAuth); the console sends the person to the address.
async fn sign_in(
    State(state): State<AppState>,
    caller: Caller<Admin>,
    Path(name): Path<String>,
    Json(body): Json<SignInSettings>,
) -> Result<Json<Value>, ApiError> {
    let url = state
        .dispatch
        .start_sign_in(
            caller.actor().as_str(),
            &name,
            body,
            &return_address(&state),
            &state.installation,
        )
        .await
        .map_err(connect_error)?;
    Ok(Json(json!({ "authorize_url": url })))
}

#[derive(Deserialize)]
struct SignedIn {
    state: Option<String>,
    code: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

/// Back from signing in: the source is connected, and the console opens it to choose tools.
/// The `state` the sign-in started with is the proof; no bearer token comes along.
async fn signed_in(State(state): State<AppState>, Query(back): Query<SignedIn>) -> Redirect {
    let console = format!("{}/admin/sources", state.auth.settings().issuer);
    let outcome = match (back.state, back.code, back.error) {
        (_, _, Some(error)) => Err(back.error_description.unwrap_or(error)),
        (Some(sign_in), Some(code), None) => state
            .dispatch
            .finish_sign_in(&sign_in, &code)
            .await
            .map_err(|e| e.to_string()),
        _ => Err("the server sent the person back without a code".to_owned()),
    };
    let query = match outcome {
        Ok(name) => format!("connected={}", encode(&name)),
        Err(error) => format!("connect_error={}", encode(&error)),
    };
    Redirect::to(&format!("{console}?{query}"))
}

fn encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

/// Every job as the console changes it, and the accounts that can pay for model calls.
async fn job_settings(
    State(state): State<AppState>,
    _caller: Caller<Admin>,
) -> Result<Json<Value>, ApiError> {
    let jobs: Vec<JobSetting> = state
        .dispatch
        .job_settings()
        .await
        .map_err(|e| ApiError::from(frood_ledger::LedgerError::from(e)))?;
    Ok(Json(json!({ "jobs": jobs, "accounts": state.accounts })))
}

/// Changes a job: on or off, schedules, model, account, budget. The answer says what keeps
/// it from running, if anything.
async fn change_job(
    State(state): State<AppState>,
    caller: Caller<Admin>,
    Path(name): Path<String>,
    Json(change): Json<JobChange>,
) -> Result<Json<Value>, ApiError> {
    let (revision, why) = state
        .dispatch
        .change_job(caller.actor().as_str(), &name, change)
        .await
        .map_err(connect_error)?;
    Ok(Json(
        json!({ "name": name, "revision": revision, "why": why }),
    ))
}

async fn reset_job(
    State(state): State<AppState>,
    caller: Caller<Admin>,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let revision = state
        .dispatch
        .reset_job(caller.actor().as_str(), &name)
        .await
        .map_err(connect_error)?;
    Ok(Json(json!({ "name": name, "revision": revision })))
}
