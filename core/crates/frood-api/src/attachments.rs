//! Attachments (docs/26 §2): photos, videos, documents and links kept with knowledge or with a
//! change that waits, and the file of any source.

use axum::extract::{DefaultBodyLimit, FromRequest, Multipart, Path, Request, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get};
use axum::{Json, Router};
use chrono::Utc;
use frood_ledger::model::{AttachTo, Attachment, NewBlob, NewSource};
use serde_json::json;
use uuid::Uuid;

use crate::AppState;
use crate::auth::{Caller, Capture, Read};
use crate::capture::{Fields, capture_actor, read_multipart};
use crate::error::ApiError;
use crate::links;

pub fn routes() -> Router<AppState> {
    let limit = DefaultBodyLimit::max(frood_ledger::MAX_BLOB_BYTES + (1 << 20));
    Router::new()
        .route(
            "/v1/assertions/{id}/attachments",
            get(of_assertion).post(to_assertion).layer(limit),
        )
        .route(
            "/v1/proposals/{id}/attachments",
            get(of_proposal).post(to_proposal).layer(limit),
        )
        .route("/v1/attachments/{id}", delete(remove))
        .route("/v1/sources/{id}/file", get(file))
}

async fn of_assertion(
    State(state): State<AppState>,
    _caller: Caller<Read>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<Attachment>>, ApiError> {
    Ok(Json(
        state.ledger.attachments(AttachTo::Assertion(id)).await?,
    ))
}

async fn of_proposal(
    State(state): State<AppState>,
    _caller: Caller<Read>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<Attachment>>, ApiError> {
    Ok(Json(
        state.ledger.attachments(AttachTo::Proposal(id)).await?,
    ))
}

async fn to_assertion(
    state: State<AppState>,
    caller: Caller<Capture>,
    Path(id): Path<Uuid>,
    request: Request,
) -> Result<(StatusCode, Json<Vec<Attachment>>), ApiError> {
    add(state, caller, AttachTo::Assertion(id), request).await
}

async fn to_proposal(
    state: State<AppState>,
    caller: Caller<Capture>,
    Path(id): Path<Uuid>,
    request: Request,
) -> Result<(StatusCode, Json<Vec<Attachment>>), ApiError> {
    add(state, caller, AttachTo::Proposal(id), request).await
}

/// A file (multipart `file`, with `text` for a caption) or a link (`uri`, `title`), sent by a
/// person or, with `external_id`, by a channel module for a linked person.
async fn add(
    State(state): State<AppState>,
    caller: Caller<Capture>,
    to: AttachTo,
    request: Request,
) -> Result<(StatusCode, Json<Vec<Attachment>>), ApiError> {
    let is_multipart = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("multipart/form-data"));
    let (fields, blob) = if is_multipart {
        let multipart = Multipart::from_request(request, &state)
            .await
            .map_err(|e| ApiError::bad_request(e.body_text()))?;
        read_multipart(multipart).await?
    } else {
        let Json(fields) = Json::<Fields>::from_request(request, &state)
            .await
            .map_err(|e| ApiError::bad_request(e.body_text()))?;
        (fields, None)
    };
    let actor = capture_actor(&state, &caller, fields.external_id.as_deref()).await?;
    let uri = fields
        .uri
        .map(|u| u.trim().to_owned())
        .filter(|u| !u.is_empty());
    let kind = match (&blob, &uri) {
        (Some(blob), _) => kind_of(blob),
        (None, Some(_)) => "link",
        (None, None) => {
            return Err(ApiError::bad_request(
                "attach a file (multipart `file`) or a link (`uri`)",
            ));
        }
    };
    let title = match (fields.title.filter(|t| !t.trim().is_empty()), &blob, &uri) {
        (Some(title), ..) => Some(title.trim().to_owned()),
        (None, None, Some(uri)) => links::title(uri).await,
        _ => None,
    };
    let input = NewSource {
        kind: kind.to_owned(),
        origin: "capture".into(),
        uri,
        title,
        content: fields.text.unwrap_or_default(),
        said_at: Some(fields.said_at.unwrap_or_else(Utc::now)),
        metadata: json!({ "attachment": true }),
    };
    let captured = state.ledger.capture(&actor, input, blob).await?;
    let attached = state.ledger.attach(&actor, to, captured.source.id).await?;
    Ok((StatusCode::CREATED, Json(attached)))
}

fn kind_of(blob: &NewBlob) -> &'static str {
    let t = blob.content_type.to_ascii_lowercase();
    if t.starts_with("image/") {
        "photo"
    } else if t.starts_with("video/") {
        "video"
    } else if blob.is_media() {
        "audio"
    } else {
        "file"
    }
}

async fn remove(
    State(state): State<AppState>,
    caller: Caller<Capture>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    state.ledger.detach(&caller.actor(), id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The file of a source. Pictures, video and sound open in place; anything else downloads, so
/// a page someone uploaded never runs in the console's origin.
async fn file(
    State(state): State<AppState>,
    _caller: Caller<Read>,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let (info, data) = state.ledger.source_file(id).await?;
    let t = info.content_type.to_ascii_lowercase();
    let shown = (t.starts_with("image/") && t != "image/svg+xml")
        || t.starts_with("video/")
        || t.starts_with("audio/");
    let content_type = if shown {
        info.content_type.clone()
    } else {
        "application/octet-stream".to_owned()
    };
    let name = info.filename.as_deref().unwrap_or("file");
    let disposition = format!(
        "{}; filename*=UTF-8''{}",
        if shown { "inline" } else { "attachment" },
        url::form_urlencoded::byte_serialize(name.as_bytes())
            .collect::<String>()
            .replace('+', "%20")
    );
    Ok((
        [
            (header::CONTENT_TYPE, content_type),
            (header::CONTENT_DISPOSITION, disposition),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
            (header::CONTENT_SECURITY_POLICY, "sandbox".to_owned()),
            (header::CACHE_CONTROL, "private, max-age=3600".to_owned()),
        ],
        data,
    )
        .into_response())
}
