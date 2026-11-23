//! Channel accounts linked to people (docs/15 §4): a person asks for a one-time code, the
//! channel module (Telegram, …) redeems it for the account that sent it.

use axum::extract::{DefaultBodyLimit, Path, RawForm, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post, put};
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
        .route("/v1/channels/{channel}/about", put(about))
        .route(
            "/v1/channels/{channel}/link",
            post(link).layer(DefaultBodyLimit::max(LINK_BODY_LIMIT)),
        )
        .route("/oauth/login/{provider}/ask", post(ask_sign_in))
        .route(
            "/v1/channels/{channel}/links/{external_id}/digest",
            put(digest_every),
        )
}

/// A forwarded Telegram update is small; the words' hashes are the largest call.
const LINK_BODY_LIMIT: usize = 256 * 1024;

/// One bot for two installations (docs/34 W4): the other installation's channel module calls
/// this one's through the core, which has the public address. The caller presents the link
/// token the two modules share; the core neither holds it nor reads the call, and passes it
/// on to the module's `channels.link@1`, which checks the token and answers. Nothing of the
/// call is logged or stored here.
async fn link(
    State(state): State<AppState>,
    Path(channel): Path<String>,
    headers: HeaderMap,
    Json(input): Json<Value>,
) -> Response {
    let presented = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|t| !t.is_empty());
    let Some(presented) = presented else {
        return ApiError::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "the link token is missing",
        )
        .into_response();
    };
    match state
        .dispatch
        .modules()
        .relay(&channel, "channels.link", presented, &input)
        .await
    {
        Ok((status, body)) => (
            StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
            Json(body),
        )
            .into_response(),
        Err(frood_modhost::ModuleError::Unknown(_) | frood_modhost::ModuleError::NoProvider(_)) => {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "not-found",
                format!("no module {channel} takes forwarded updates"),
            )
            .into_response()
        }
        Err(err) => {
            tracing::warn!(module = %channel, error = %err, "cannot pass a link call on");
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "module-unavailable",
                format!("the module {channel} does not answer now"),
            )
            .into_response()
        }
    }
}

#[derive(Deserialize)]
struct DigestEvery {
    every: String,
}

/// A channel's "less often" button (docs/31 D4): how often the linked person's digest comes,
/// daily, weekly or off. The rest of their settings stays as it is.
async fn digest_every(
    State(state): State<AppState>,
    caller: Caller<ChannelsAct>,
    Path((channel, external_id)): Path<(String, String)>,
    Json(input): Json<DigestEvery>,
) -> Result<Json<Value>, ApiError> {
    own_channel(&caller, &channel)?;
    if !["daily", "weekly", "off"].contains(&input.every.as_str()) {
        return Err(ApiError::bad_request("every is daily, weekly or off"));
    }
    let user = state
        .auth
        .linked_user(&channel, &external_id)
        .await
        .map_err(|err| auth_error(&state.auth, Resource::Api, err))?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::FORBIDDEN,
                "not-linked",
                format!("this {channel} account is not linked to a person"),
            )
        })?;
    let email = state
        .auth
        .user_email(user)
        .await
        .map_err(|err| auth_error(&state.auth, Resource::Api, err))?;
    let actor = format!("user:{user}/via/{channel}");
    state
        .dispatch
        .set_digest_every(&actor, &email, &input.every)
        .await
        .map_err(crate::connections::connect_error)?;
    Ok(Json(json!({ "every": input.every })))
}

/// Sign-in with a button in the person's channel (docs/28 §2): the auth side checks the
/// address and keeps the ask, the button goes out through the channel module, and the browser
/// goes on to wait for the press.
async fn ask_sign_in(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    headers: HeaderMap,
    RawForm(form): RawForm,
) -> Response {
    let (tx, ask) = match state.auth.ask_from_form(&provider, &headers, &form).await {
        Ok(asked) => asked,
        Err((status, message)) => return frood_auth::problem_page(status, &message),
    };
    if let Err(err) = state.dispatch.ask_sign_in(&ask).await {
        tracing::warn!(channel = %ask.channel, error = %err, "could not send a sign-in button");
        let _ = state.auth.drop_sign_in_ask(ask.id).await;
        return frood_auth::problem_page(
            StatusCode::BAD_GATEWAY,
            &format!("Could not send the button to {}: {err}", ask.channel),
        );
    }
    Redirect::to(&state.auth.sign_in_wait_path(&provider, &tx)).into_response()
}

#[derive(Deserialize)]
struct About {
    /// Where the channel opens with a link code appended; https only.
    start_url: Option<String>,
    /// The Telegram bot's id: Mini App launch data is signed over it (docs/32 M2).
    #[serde(default)]
    bot_id: Option<i64>,
    /// One bot for two installations (docs/34 W4): the labels and the other installation's
    /// Mini App, for the Mini App's switcher.
    #[serde(default)]
    link: Option<frood_auth::ChannelLinkInfo>,
}

/// The channel's module says where it opens with a link code, so the console can link an
/// account in one tap (Telegram: `https://t.me/<bot>?start=`), and, for Telegram, the bot's id
/// for the Mini App's sign-in.
async fn about(
    State(state): State<AppState>,
    caller: Caller<ChannelsAct>,
    Path(channel): Path<String>,
    Json(about): Json<About>,
) -> Result<StatusCode, ApiError> {
    own_channel(&caller, &channel)?;
    if let Some(link) = &about.link
        && let Err(problem) = link.check()
    {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid-request",
            problem,
        ));
    }
    let start_url = about.start_url.map(|u| u.trim().to_owned());
    if let Some(url) = &start_url
        && (!url.starts_with("https://") || url.len() > 300)
    {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid-request",
            "start_url is an https address of up to 300 characters",
        ));
    }
    state
        .auth
        .set_channel_start_url(&channel, start_url.as_deref())
        .await
        .map_err(|err| auth_error(&state.auth, Resource::Api, err))?;
    state
        .auth
        .set_channel_bot_id(&channel, about.bot_id)
        .await
        .map_err(|err| auth_error(&state.auth, Resource::Api, err))?;
    state
        .auth
        .set_channel_link(&channel, about.link.as_ref())
        .await
        .map_err(|err| auth_error(&state.auth, Resource::Api, err))?;
    Ok(StatusCode::NO_CONTENT)
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
