//! Inbox (docs/22): situations that need a person, and acting on them from anywhere.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use frood_auth::scope;
use frood_dispatch::{Act, ActOutcome, Shown};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::AppState;
use crate::auth::{Caller, Read, Signed, auth_error};
use crate::error::ApiError;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/inbox", get(list))
        .route("/v1/inbox/{id}", get(one))
        .route("/v1/inbox/{id}/act", post(act))
        .route("/v1/inbox/{id}/undo", post(undo))
        .route("/v1/digest", get(digest))
}

#[derive(Deserialize)]
struct DigestQuery {
    /// RFC 3339; default: the person's last digest, or a day ago.
    since: Option<chrono::DateTime<chrono::Utc>>,
    /// For channel modules: whose digest.
    external_id: Option<String>,
}

/// "What is new since last time" for the caller, or for a channel's linked person.
async fn digest(
    State(state): State<AppState>,
    caller: Caller<Read>,
    Query(q): Query<DigestQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let principal = &caller.principal;
    let user = match (principal.user_id(), q.external_id.as_deref()) {
        (Some(user), None) => user,
        (None, Some(external_id)) if principal.has(scope::CHANNELS_ACT) => {
            let module = principal.module.as_deref().unwrap_or_default();
            state
                .auth
                .linked_user(module, external_id)
                .await
                .map_err(|err| auth_error(&state.auth, frood_auth::Resource::Api, err))?
                .ok_or_else(|| {
                    ApiError::new(
                        StatusCode::FORBIDDEN,
                        "not-linked",
                        format!("this {module} account is not linked to a person"),
                    )
                })?
        }
        _ => {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "person-required",
                "a digest is a person's: use a person's token, or a channel module with external_id",
            ));
        }
    };
    state
        .dispatch
        .digest_for(user, q.since)
        .await
        .map(Json)
        .map_err(db)
}

fn db(err: frood_store::sqlx::Error) -> ApiError {
    frood_ledger::LedgerError::from(err).into()
}

#[derive(Serialize)]
struct Entry {
    #[serde(flatten)]
    shown: Shown,
    /// The situation is the caller's (B1).
    for_me: bool,
    seen: bool,
}

#[derive(Deserialize)]
struct ListQuery {
    /// Include resolved items.
    all: Option<bool>,
    limit: Option<i64>,
}

async fn list(
    State(state): State<AppState>,
    caller: Caller<Read>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Vec<Entry>>, ApiError> {
    let dispatch = &state.dispatch;
    let items = dispatch
        .inbox(q.all.unwrap_or(false), q.limit.unwrap_or(50))
        .await
        .map_err(db)?;
    let user = caller.principal.user_id();
    let seen = match user {
        Some(user) => dispatch.inbox_seen_by(user).await.map_err(db)?,
        None => Default::default(),
    };
    let mut entries = Vec::with_capacity(items.len());
    for item in items {
        let for_me = user.is_some_and(|u| item.for_users.contains(&u));
        let seen = seen.contains(&item.id);
        let shown = dispatch.inbox_shown(item).await.map_err(db)?;
        entries.push(Entry {
            shown,
            for_me,
            seen,
        });
    }
    // The caller's own situations first; the order within stays.
    entries.sort_by_key(|e| (e.shown.item.state == "resolved", !e.for_me));
    Ok(Json(entries))
}

async fn one(
    State(state): State<AppState>,
    _caller: Caller<Read>,
    Path(id): Path<Uuid>,
) -> Result<Json<Shown>, ApiError> {
    let item = state
        .dispatch
        .inbox_item(id)
        .await
        .map_err(db)?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "not-found",
                format!("Inbox item {id} not found"),
            )
        })?;
    Ok(Json(state.dispatch.inbox_shown(item).await.map_err(db)?))
}

#[derive(Deserialize)]
struct ActBody {
    #[serde(flatten)]
    act: Act,
    /// For channel modules: the channel account of the person who pressed the button.
    external_id: Option<String>,
}

/// A person acts on an item: with their own token, or through a channel module for the person
/// whose channel account it is (R1′: `user:<id>/via/<channel>`). Buttons in channels decide at
/// once (QN1).
async fn act(
    State(state): State<AppState>,
    caller: Caller<Signed>,
    Path(id): Path<Uuid>,
    Json(body): Json<ActBody>,
) -> Result<Json<ActOutcome>, ApiError> {
    let needed = if matches!(body.act.action.as_str(), "accept" | "reject" | "defer") {
        scope::DECIDE
    } else {
        scope::CAPTURE
    };
    let (user, actor) = person(
        &state,
        &caller.principal,
        body.external_id.as_deref(),
        needed,
    )
    .await?;
    state
        .dispatch
        .inbox_act(id, user, &actor, &body.act)
        .await
        .map(Json)
        .map_err(|message| ApiError::new(StatusCode::CONFLICT, "conflict", message))
}

/// Who acts on the Inbox: the person whose token it is, or, for a channel module, the person
/// linked to the channel account that pressed the button (R1′: `user:<id>/via/<channel>`).
async fn person(
    state: &AppState,
    principal: &frood_auth::Principal,
    external_id: Option<&str>,
    needed: &'static str,
) -> Result<(Uuid, String), ApiError> {
    let forbidden = |code: &'static str, message: &str| {
        ApiError::new(StatusCode::FORBIDDEN, code, message.to_owned())
    };
    if principal.run.is_some() {
        return Err(forbidden(
            "person-required",
            "agents propose and ask; people act on the Inbox",
        ));
    }
    Ok(match (principal.user_id(), external_id) {
        (Some(user), None) => {
            if !principal.has(needed) {
                return Err(forbidden(
                    "insufficient-scope",
                    &format!("this needs the {needed} permission"),
                ));
            }
            (user, principal.actor())
        }
        (None, Some(external_id)) => {
            let module = principal.module.as_deref().unwrap_or_default();
            let user = state
                .auth
                .linked_user(module, external_id)
                .await
                .map_err(|err| auth_error(&state.auth, frood_auth::Resource::Api, err))?
                .ok_or_else(|| {
                    forbidden(
                        "not-linked",
                        &format!("this {module} account is not linked to a person; send the bot /link <code>"),
                    )
                })?;
            let actor = principal.acting_for(user).ok_or_else(|| {
                forbidden(
                    "insufficient-scope",
                    "acting for linked people needs the channels:act permission",
                )
            })?;
            (user, actor)
        }
        (Some(_), Some(_)) => {
            return Err(ApiError::bad_request(
                "external_id is for channel modules; a person's token acts as that person",
            ));
        }
        (None, None) => {
            return Err(forbidden(
                "person-required",
                "a module acts on the Inbox only for a linked person: give external_id",
            ));
        }
    })
}

#[derive(Deserialize, Default)]
struct UndoBody {
    /// For channel modules: the channel account of the person who pressed the button.
    external_id: Option<String>,
}

/// Takes back a decision on a proposal item (docs/25 §3.2); the item opens again.
async fn undo(
    State(state): State<AppState>,
    caller: Caller<Signed>,
    Path(id): Path<Uuid>,
    body: Option<Json<UndoBody>>,
) -> Result<Json<frood_dispatch::InboxItem>, ApiError> {
    let body = body.map(|Json(b)| b).unwrap_or_default();
    let (_, actor) = person(
        &state,
        &caller.principal,
        body.external_id.as_deref(),
        scope::DECIDE,
    )
    .await?;
    state
        .dispatch
        .inbox_undo(id, &actor)
        .await
        .map(Json)
        .map_err(|message| ApiError::new(StatusCode::CONFLICT, "conflict", message))
}
