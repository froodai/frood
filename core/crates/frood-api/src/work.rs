//! Search and processing status (docs/12 §2, docs/14 §2).

use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use frood_dispatch::{Run, SourceStatus};
use frood_ledger::{SearchHit, SearchQuery};
use serde::Deserialize;
use uuid::Uuid;

use crate::AppState;
use crate::auth::{Caller, Read};
use crate::error::ApiError;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/search", get(search))
        .route("/v1/sources/{id}/status", get(source_status))
        .route("/v1/runs/{id}", get(run))
}

#[derive(Deserialize)]
struct SearchParams {
    q: String,
    kind: Option<String>,
    entity: Option<Uuid>,
    status: Option<String>,
    valid_at: Option<DateTime<Utc>>,
    limit: Option<i64>,
}

/// `GET /v1/search?q=…`: words and, when embeddings are set up, meaning across languages.
async fn search(
    State(state): State<AppState>,
    _caller: Caller<Read>,
    Query(params): Query<SearchParams>,
) -> Result<Json<Vec<SearchHit>>, ApiError> {
    let query = SearchQuery {
        text: params.q,
        embedding: None,
        kind: params.kind,
        entity: params.entity,
        status: params
            .status
            .as_deref()
            .map(str::parse)
            .transpose()
            .map_err(ApiError::bad_request)?,
        valid_at: params.valid_at,
        limit: params.limit.unwrap_or(20),
    };
    state
        .searcher
        .search(query)
        .await
        .map(Json)
        .map_err(|err| match err {
            frood_search::SearchError::Ledger(e) => e.into(),
            frood_search::SearchError::Llm(e) => ApiError::new(
                axum::http::StatusCode::BAD_GATEWAY,
                "upstream",
                e.to_string(),
            ),
        })
}

async fn source_status(
    State(state): State<AppState>,
    _caller: Caller<Read>,
    Path(id): Path<Uuid>,
) -> Result<Json<SourceStatus>, ApiError> {
    Ok(Json(state.dispatch.source_status(id).await?))
}

async fn run(
    State(state): State<AppState>,
    _caller: Caller<Read>,
    Path(id): Path<Uuid>,
) -> Result<Json<Run>, ApiError> {
    state
        .dispatch
        .run(id)
        .await
        .map_err(|e| ApiError::from(frood_ledger::LedgerError::from(e)))?
        .map(Json)
        .ok_or_else(|| {
            ApiError::new(
                axum::http::StatusCode::NOT_FOUND,
                "not-found",
                format!("run {id} not found"),
            )
        })
}
