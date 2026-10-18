//! HTTP API of the Frood core (docs/12_api_mcp_auth.md): health, version and the Ledger.

mod error;
mod ledger;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use frood_ledger::Ledger;
use serde_json::{Value, json};

pub use error::ApiError;

#[derive(Clone)]
pub struct AppState {
    pub ledger: Ledger,
    pub installation: String,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/v1/version", get(version))
        .merge(ledger::routes())
        .with_state(state)
}

/// Liveness: the process is up.
async fn healthz() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

/// Readiness: the database answers.
async fn readyz(State(state): State<AppState>) -> (StatusCode, Json<Value>) {
    match frood_store::ping(state.ledger.pool()).await {
        Ok(()) => (StatusCode::OK, Json(json!({ "status": "ready" }))),
        Err(err) => {
            tracing::warn!(error = %err, "readiness check failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "status": "not_ready", "reason": "database is not reachable" })),
            )
        }
    }
}

async fn version(State(state): State<AppState>) -> Json<Value> {
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "api": "v1",
        "installation": state.installation,
    }))
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;

    fn app() -> Router {
        // Nothing listens on port 1, so readiness fails fast without a real database.
        let pool = frood_store::connect_lazy("postgres://frood:x@127.0.0.1:1/frood").unwrap();
        router(AppState {
            ledger: Ledger::new(pool, frood_ledger::PolicyConfig::default()),
            installation: "test".to_owned(),
        })
    }

    async fn get_json(path: &str) -> (StatusCode, Value) {
        let response = app()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn healthz_is_ok_without_database() {
        assert_eq!(
            get_json("/healthz").await,
            (StatusCode::OK, json!({ "status": "ok" }))
        );
    }

    #[tokio::test]
    async fn readyz_reports_unreachable_database() {
        let (status, body) = get_json("/readyz").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["status"], "not_ready");
    }

    #[tokio::test]
    async fn version_names_installation() {
        let (status, body) = get_json("/v1/version").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["installation"], "test");
        assert_eq!(body["api"], "v1");
    }
}
