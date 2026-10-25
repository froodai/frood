//! Attention from the console (docs/26 §4, QY1): the installation's rules — how urgent each
//! situation is, what is looked into first, the push fuse, reminders, the stall detectors —
//! need `admin`; a person's own settings and channel accounts are theirs.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get};
use axum::{Json, Router};
use frood_auth::Resource;
use frood_dispatch::attention::{AttentionChange, AttentionSettings, PersonalChange};
use serde_json::{Value, json};

use crate::AppState;
use crate::auth::{Admin, Caller, auth_error};
use crate::connections::connect_error;
use crate::error::ApiError;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/attention", get(settings).put(change))
        .route("/v1/me/settings", get(mine).put(change_mine))
        .route("/v1/me/links/{channel}/{external_id}", delete(unlink))
}

fn database(err: frood_store::sqlx::Error) -> ApiError {
    ApiError::from(frood_ledger::LedgerError::from(err))
}

async fn settings(
    State(state): State<AppState>,
    _caller: Caller<Admin>,
) -> Result<Json<AttentionSettings>, ApiError> {
    state
        .dispatch
        .attention_settings()
        .await
        .map(Json)
        .map_err(database)
}

/// Replaces the console's attention settings; what `frood.yaml` sets stays as it is.
async fn change(
    State(state): State<AppState>,
    caller: Caller<Admin>,
    Json(change): Json<AttentionChange>,
) -> Result<Json<Value>, ApiError> {
    let revision = state
        .dispatch
        .change_attention(caller.actor().as_str(), change)
        .await
        .map_err(connect_error)?;
    Ok(Json(json!({ "revision": revision })))
}

fn email_of(caller: &Caller<Admin>) -> Result<String, ApiError> {
    caller.user_id()?;
    caller
        .principal
        .user
        .as_ref()
        .map(|u| u.email.clone())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::FORBIDDEN,
                "person-required",
                "this needs a person's token",
            )
        })
}

/// The caller's own settings: quiet hours, push limit, reminders, and the channels they can
/// link accounts of, with those linked.
async fn mine(
    State(state): State<AppState>,
    caller: Caller<Admin>,
) -> Result<Json<Value>, ApiError> {
    let email = email_of(&caller)?;
    let settings = state
        .dispatch
        .personal_settings(&email)
        .await
        .map_err(database)?;
    let links = state
        .auth
        .links_of(caller.user_id()?)
        .await
        .map_err(|err| auth_error(&state.auth, Resource::Api, err))?;
    let mut channels = Vec::new();
    for channel in state.dispatch.channels() {
        let linked: Vec<_> = links.iter().filter(|l| l.channel == channel).collect();
        let start_url = state
            .auth
            .channel_start_url(&channel)
            .await
            .map_err(|err| auth_error(&state.auth, Resource::Api, err))?;
        channels.push(json!({ "channel": channel, "links": linked, "start_url": start_url }));
    }
    let mut out = serde_json::to_value(settings).unwrap_or_default();
    out["channels"] = json!(channels);
    Ok(Json(out))
}

async fn change_mine(
    State(state): State<AppState>,
    caller: Caller<Admin>,
    Json(change): Json<PersonalChange>,
) -> Result<Json<Value>, ApiError> {
    let email = email_of(&caller)?;
    let revision = state
        .dispatch
        .change_personal(caller.actor().as_str(), &email, change)
        .await
        .map_err(connect_error)?;
    Ok(Json(json!({ "revision": revision })))
}

/// The caller unlinks one of their channel accounts; the channel stops acting as them.
async fn unlink(
    State(state): State<AppState>,
    caller: Caller<Admin>,
    Path((channel, external_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let done = state
        .auth
        .unlink_own(caller.user_id()?, &channel, &external_id)
        .await
        .map_err(|err| auth_error(&state.auth, Resource::Api, err))?;
    if done {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not-found",
            "no such account of yours is linked",
        ))
    }
}
