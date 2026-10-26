//! A person's passkeys from the console (docs/28 §2): the list, a one-time link that adds one
//! on this device, and removing one.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use frood_auth::{AuthError, PasskeyInfo, Resource};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::AppState;
use crate::auth::{Admin, Caller, auth_error};
use crate::error::ApiError;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/me/passkeys", get(list))
        .route("/v1/me/passkeys/links", post(link))
        .route("/v1/me/passkeys/{id}", delete(remove))
}

fn refused(state: &AppState, err: AuthError) -> ApiError {
    match err {
        AuthError::Invalid(message) => ApiError::new(StatusCode::CONFLICT, "conflict", message),
        err => auth_error(&state.auth, Resource::Api, err),
    }
}

/// The caller's passkeys, and whether this installation signs in with them at all.
async fn list(
    State(state): State<AppState>,
    caller: Caller<Admin>,
) -> Result<Json<Value>, ApiError> {
    let passkeys: Vec<PasskeyInfo> = state
        .auth
        .passkeys_of(caller.user_id()?)
        .await
        .map_err(|err| refused(&state, err))?;
    Ok(Json(
        json!({ "on": state.auth.passkeys_on(), "passkeys": passkeys }),
    ))
}

/// A link that adds a passkey for the caller, working for a few minutes.
async fn link(
    State(state): State<AppState>,
    caller: Caller<Admin>,
) -> Result<Json<Value>, ApiError> {
    let (url, expires_at) = state
        .auth
        .passkey_link(caller.user_id()?)
        .await
        .map_err(|err| refused(&state, err))?;
    Ok(Json(json!({ "url": url, "expires_at": expires_at })))
}

async fn remove(
    State(state): State<AppState>,
    caller: Caller<Admin>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let removed = state
        .auth
        .remove_passkey(caller.user_id()?, id)
        .await
        .map_err(|err| refused(&state, err))?;
    if removed {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not-found",
            "no such passkey of yours",
        ))
    }
}
