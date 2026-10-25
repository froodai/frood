//! Connecting sources from the console (docs/26 §4): the list, a check of an MCP server with a
//! token, and connecting or disconnecting one. Changing them needs `admin`, like `frood apply`.

use std::collections::BTreeMap;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use frood_dispatch::connections::{Checked, ConnectError, Connection, NewConnection};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::AppState;
use crate::auth::{Admin, Caller};
use crate::error::ApiError;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/connections", get(list))
        .route("/v1/connections/check", post(check))
        .route("/v1/connections/{name}", put(connect).delete(disconnect))
}

fn connect_error(err: ConnectError) -> ApiError {
    match err {
        ConnectError::Database(db) => ApiError::from(frood_ledger::LedgerError::from(db)),
        ConnectError::NotFound(_) => {
            ApiError::new(StatusCode::NOT_FOUND, "not-found", err.to_string())
        }
        ConnectError::InConfig(_) => {
            ApiError::new(StatusCode::CONFLICT, "in-config", err.to_string())
        }
        ConnectError::NoVault => ApiError::new(StatusCode::CONFLICT, "no-vault", err.to_string()),
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
            if let Some(source) = body.name.as_deref().and_then(|n| state.dispatch.source(n)) {
                headers = source.headers;
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
