//! Errors as RFC 9457 problem details (`application/problem+json`).

use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use frood_ledger::LedgerError;
use serde_json::{Value, json};

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    detail: String,
    extra: Value,
    challenge: Option<String>,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            status,
            code,
            detail: detail.into(),
            extra: json!({}),
            challenge: None,
        }
    }

    /// Adds a `WWW-Authenticate` challenge to the response.
    pub fn with_challenge(mut self, challenge: String) -> Self {
        self.challenge = Some(challenge);
        self
    }

    pub fn bad_request(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid-request", detail)
    }
}

impl From<LedgerError> for ApiError {
    fn from(err: LedgerError) -> Self {
        match err {
            LedgerError::NotFound(_) => {
                Self::new(StatusCode::NOT_FOUND, "not-found", err.to_string())
            }
            LedgerError::Invalid(_) => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid-request",
                err.to_string(),
            ),
            LedgerError::Stale {
                proposal,
                assertion,
                expected,
                actual,
            } => Self {
                status: StatusCode::CONFLICT,
                code: "stale-base",
                detail: err.to_string(),
                extra: json!({ "proposal_id": proposal, "assertion_id": assertion, "expected_version": expected, "current_version": actual }),
                challenge: None,
            },
            LedgerError::AlreadyDecided(..) => {
                Self::new(StatusCode::CONFLICT, "already-decided", err.to_string())
            }
            LedgerError::Database(db) => {
                tracing::error!(error = %db, "database error");
                Self::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    "internal error; see server logs",
                )
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut body = json!({
            "type": format!("https://frood.dev/problems/{}", self.code),
            "title": self.status.canonical_reason().unwrap_or("Error"),
            "status": self.status.as_u16(),
            "detail": self.detail,
        });
        if let (Some(body), Some(extra)) = (body.as_object_mut(), self.extra.as_object()) {
            body.extend(extra.clone());
        }
        let mut response = (self.status, Json(body)).into_response();
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json"),
        );
        if let Some(challenge) = self.challenge.and_then(|c| HeaderValue::from_str(&c).ok()) {
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, challenge);
        }
        response
    }
}
