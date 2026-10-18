//! `POST /v1/capture` (docs/14 §2): the one way in for notes, voice messages and files. It
//! answers at once; transcription and extraction follow in the background.
//!
//! JSON for text; multipart for a file (`file`) with optional text fields. A channel module
//! names the sender's account in `external_id` and the capture is that linked person's.

use axum::extract::{DefaultBodyLimit, FromRequest, Multipart, Request, State};
use axum::http::{StatusCode, header};
use axum::routing::post;
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use frood_auth::Resource;
use frood_ledger::model::{Actor, NewBlob, NewSource};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::AppState;
use crate::auth::{Caller, Capture, auth_error};
use crate::error::ApiError;

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/v1/capture",
        post(capture).layer(DefaultBodyLimit::max(
            frood_ledger::MAX_BLOB_BYTES + (1 << 20),
        )),
    )
}

#[derive(Debug, Default, Deserialize)]
struct Fields {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    said_at: Option<DateTime<Utc>>,
    /// What it is about: a project or a person (a hint for extraction).
    #[serde(default)]
    about: Option<String>,
    /// Language hint, e.g. `ru`.
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    uri: Option<String>,
    /// Channel modules: the sender's account in the channel.
    #[serde(default)]
    external_id: Option<String>,
}

async fn capture(
    State(state): State<AppState>,
    caller: Caller<Capture>,
    request: Request,
) -> Result<(StatusCode, Json<Value>), ApiError> {
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

    let text = fields.text.unwrap_or_default();
    let kind = fields.kind.unwrap_or_else(|| {
        match &blob {
            Some(blob) if blob.is_media() => "audio",
            Some(_) => "file",
            None => "note",
        }
        .to_owned()
    });
    let mut metadata = serde_json::Map::new();
    if let Some(about) = fields.about {
        metadata.insert("about".into(), json!(about));
    }
    if let Some(language) = fields.language {
        metadata.insert("language".into(), json!(language));
    }
    let input = NewSource {
        kind,
        origin: "capture".into(),
        uri: fields.uri,
        title: fields.title,
        content: text,
        said_at: Some(fields.said_at.unwrap_or_else(Utc::now)),
        metadata: Value::Object(metadata),
    };
    let captured = state.ledger.capture(&actor, input, blob).await?;
    let source = &captured.source;
    let status = if captured.created {
        StatusCode::ACCEPTED
    } else {
        StatusCode::OK
    };
    Ok((
        status,
        Json(json!({
            "source_id": source.id,
            "created": captured.created,
            "kind": source.kind,
            "file": source.file,
            "status_url": format!("/v1/sources/{}/status", source.id),
        })),
    ))
}

/// Who the capture belongs to: the person holding the token, or, for a channel module, the
/// person whose channel account sent it.
async fn capture_actor(
    state: &AppState,
    caller: &Caller<Capture>,
    external_id: Option<&str>,
) -> Result<Actor, ApiError> {
    let principal = &caller.principal;
    match (principal.user_id(), external_id) {
        (_, None) => Ok(caller.actor()),
        (Some(_), Some(_)) => Err(ApiError::bad_request(
            "external_id is for channel modules; a person's token captures as that person",
        )),
        (None, Some(external_id)) => {
            let module = principal.module.as_deref().unwrap_or_default();
            let user = state
                .auth
                .linked_user(module, external_id)
                .await
                .map_err(|err| auth_error(&state.auth, Resource::Api, err))?
                .ok_or_else(|| {
                    ApiError::new(
                        StatusCode::FORBIDDEN,
                        "not-linked",
                        format!("this {module} account is not linked to a person; send the bot /link <code>"),
                    )
                })?;
            let actor = principal.acting_for(user).ok_or_else(|| {
                ApiError::new(
                    StatusCode::FORBIDDEN,
                    "insufficient-scope",
                    "acting for linked people needs the channels:act permission",
                )
            })?;
            Ok(Actor::parse(&actor).expect("channel actors are well-formed"))
        }
    }
}

async fn read_multipart(mut multipart: Multipart) -> Result<(Fields, Option<NewBlob>), ApiError> {
    let mut fields = Fields::default();
    let mut blob = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::bad_request(e.body_text()))?
    {
        let name = field.name().unwrap_or_default().to_owned();
        if name == "file" {
            let filename = field.file_name().map(str::to_owned);
            let declared = field.content_type().map(str::to_owned);
            let data = field
                .bytes()
                .await
                .map_err(|e| ApiError::bad_request(e.body_text()))?;
            let content_type = match declared.as_deref() {
                Some(t) if t != "application/octet-stream" => t.to_owned(),
                _ => guess_type(filename.as_deref()),
            };
            blob = Some(NewBlob {
                content_type,
                filename,
                data: data.to_vec(),
            });
            continue;
        }
        let value = field
            .text()
            .await
            .map_err(|e| ApiError::bad_request(e.body_text()))?;
        match name.as_str() {
            "text" => fields.text = Some(value),
            "kind" => fields.kind = Some(value),
            "title" => fields.title = Some(value),
            "about" => fields.about = Some(value),
            "language" => fields.language = Some(value),
            "uri" => fields.uri = Some(value),
            "external_id" => fields.external_id = Some(value),
            "said_at" => {
                fields.said_at = Some(
                    DateTime::parse_from_rfc3339(&value)
                        .map_err(|_| ApiError::bad_request("said_at must be an RFC 3339 time"))?
                        .with_timezone(&Utc),
                );
            }
            other => {
                return Err(ApiError::bad_request(format!("unknown field {other:?}")));
            }
        }
    }
    Ok((fields, blob))
}

/// Content type from a file name when the client sent none.
fn guess_type(filename: Option<&str>) -> String {
    let extension = filename
        .and_then(|f| f.rsplit_once('.'))
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "ogg" | "oga" | "opus" => "audio/ogg",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "wav" => "audio/wav",
        "webm" => "audio/webm",
        "mp4" => "video/mp4",
        "pdf" => "application/pdf",
        "txt" | "md" => "text/plain",
        _ => "application/octet-stream",
    }
    .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_type_from_file_name() {
        assert_eq!(guess_type(Some("voice.OGA")), "audio/ogg");
        assert_eq!(guess_type(Some("talk.m4a")), "audio/mp4");
        assert_eq!(guess_type(None), "application/octet-stream");
    }
}
