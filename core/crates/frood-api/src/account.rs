//! `/v1/me` and the caller's personal tokens (docs/12_api_mcp_auth.md §2).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get};
use axum::{Json, Router};
use frood_auth::{PersonalToken, Resource};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::AppState;
use crate::auth::{Admin, Caller, Signed, auth_error};
use crate::error::ApiError;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/me", get(me))
        .route("/v1/tokens", get(list_tokens).post(create_token))
        .route("/v1/tokens/{id}", delete(revoke_token))
}

async fn me(caller: Caller<Signed>) -> Json<Value> {
    let principal = &caller.principal;
    Json(json!({
        "user_id": principal.user_id,
        "email": principal.email,
        "actor": principal.actor(),
        "scopes": principal.scopes,
        "client_id": principal.client_id,
    }))
}

async fn list_tokens(
    State(state): State<AppState>,
    caller: Caller<Admin>,
) -> Result<Json<Vec<PersonalToken>>, ApiError> {
    state
        .auth
        .personal_tokens(caller.principal.user_id)
        .await
        .map(Json)
        .map_err(|err| auth_error(&state.auth, Resource::Api, err))
}

#[derive(Deserialize)]
struct NewToken {
    label: String,
    scopes: Vec<String>,
    expires_in_days: Option<u32>,
}

/// Creates a personal token. The secret is in the response only; the server keeps its hash.
async fn create_token(
    State(state): State<AppState>,
    caller: Caller<Admin>,
    Json(input): Json<NewToken>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let (id, token) = state
        .auth
        .create_personal_token(
            caller.principal.user_id,
            &input.label,
            &input.scopes,
            input.expires_in_days,
        )
        .await
        .map_err(|err| auth_error(&state.auth, Resource::Api, err))?;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "id": id, "token": token, "label": input.label, "scopes": input.scopes })),
    ))
}

async fn revoke_token(
    State(state): State<AppState>,
    caller: Caller<Admin>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    state
        .auth
        .revoke_token(caller.principal.user_id, id)
        .await
        .map_err(|err| auth_error(&state.auth, Resource::Api, err))?;
    Ok(StatusCode::NO_CONTENT)
}
