//! Channel accounts linked to people (docs/15 §4): a person asks for a one-time code, the
//! channel module (Telegram, …) redeems it for the account that sent it.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use frood_auth::{LinkCode, Resource};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::AppState;
use crate::auth::{Admin, Caller, ChannelsAct, auth_error};
use crate::error::ApiError;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/channels/{channel}/link-codes", post(create_code))
        .route("/v1/channels/{channel}/links", post(redeem))
        .route(
            "/v1/channels/{channel}/links/{external_id}",
            get(check).delete(unlink),
        )
        .route(
            "/v1/channels/{channel}/messages/{message_ref}",
            get(delivered),
        )
}

/// Which Inbox item a channel's message showed: a reply to it is about that item.
async fn delivered(
    State(state): State<AppState>,
    caller: Caller<ChannelsAct>,
    Path((channel, message_ref)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    own_channel(&caller, &channel)?;
    let item = state
        .dispatch
        .delivered_in(&channel, &message_ref)
        .await
        .map_err(|e| ApiError::from(frood_ledger::LedgerError::from(e)))?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "not-found",
                "no Inbox item was shown in that message",
            )
        })?;
    Ok(Json(json!({ "item": item })))
}

/// A module may only manage the links of its own channel (the module's name in `frood.yaml`).
pub(crate) fn own_channel<P>(caller: &Caller<P>, channel: &str) -> Result<(), ApiError> {
    match caller.principal.module.as_deref() {
        Some(module) if module == channel => Ok(()),
        _ => Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "wrong-channel",
            format!("only the {channel} module manages {channel} links"),
        )),
    }
}

/// The person links one of their channel accounts. Needs `admin`, like personal tokens:
/// whoever redeems the code can write as this person.
async fn create_code(
    State(state): State<AppState>,
    caller: Caller<Admin>,
    Path(channel): Path<String>,
) -> Result<(StatusCode, Json<LinkCode>), ApiError> {
    let code = state
        .auth
        .create_link_code(caller.user_id()?, &channel)
        .await
        .map_err(|err| auth_error(&state.auth, Resource::Api, err))?;
    Ok((StatusCode::CREATED, Json(code)))
}

#[derive(Deserialize)]
struct Redeem {
    code: String,
    external_id: String,
    #[serde(default)]
    label: Option<String>,
}

async fn redeem(
    State(state): State<AppState>,
    caller: Caller<ChannelsAct>,
    Path(channel): Path<String>,
    Json(input): Json<Redeem>,
) -> Result<Json<Value>, ApiError> {
    own_channel(&caller, &channel)?;
    let user_id = state
        .auth
        .redeem_link_code(
            &channel,
            &input.code,
            &input.external_id,
            input.label.as_deref(),
        )
        .await
        .map_err(|err| auth_error(&state.auth, Resource::Api, err))?;
    let email = state
        .auth
        .user_email(user_id)
        .await
        .map_err(|err| auth_error(&state.auth, Resource::Api, err))?;
    Ok(Json(
        json!({ "linked": true, "user_id": user_id, "email": email }),
    ))
}

async fn check(
    State(state): State<AppState>,
    caller: Caller<ChannelsAct>,
    Path((channel, external_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    own_channel(&caller, &channel)?;
    let user = state
        .auth
        .linked_user(&channel, &external_id)
        .await
        .map_err(|err| auth_error(&state.auth, Resource::Api, err))?;
    Ok(Json(json!({ "linked": user.is_some(), "user_id": user })))
}

async fn unlink(
    State(state): State<AppState>,
    caller: Caller<ChannelsAct>,
    Path((channel, external_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    own_channel(&caller, &channel)?;
    let removed = state
        .auth
        .unlink(&channel, &external_id)
        .await
        .map_err(|err| auth_error(&state.auth, Resource::Api, err))?;
    Ok(if removed {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    })
}
