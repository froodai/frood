//! Search and processing status (docs/12 §2, docs/14 §2).

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use frood_dispatch::runs::RunEvent;
use frood_dispatch::{Question, Run, SourceStatus};
use frood_ledger::{SearchHit, SearchQuery};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::AppState;
use crate::auth::{Admin, Caller, Capture, Read};
use crate::error::ApiError;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/search", get(search))
        .route("/v1/sources/{id}/status", get(source_status))
        .route("/v1/runs", get(list_runs).post(start_run))
        .route("/v1/runs/{id}", get(run))
        .route("/v1/runs/{id}/events", get(run_events))
        .route("/v1/runs/{id}/messages", post(send_message))
        .route("/v1/runs/{id}/cancel", post(cancel_run))
        .route("/v1/questions", get(list_questions))
        .route("/v1/questions/{id}", get(question))
        .route("/v1/questions/{id}/answer", post(answer_question))
}

fn conflict(message: String) -> ApiError {
    ApiError::new(StatusCode::CONFLICT, "conflict", message)
}

fn db(err: frood_store::sqlx::Error) -> ApiError {
    frood_ledger::LedgerError::from(err).into()
}

#[derive(Deserialize)]
struct RunQuery {
    job: Option<String>,
    status: Option<String>,
    limit: Option<i64>,
}

async fn list_runs(
    State(state): State<AppState>,
    _caller: Caller<Read>,
    Query(q): Query<RunQuery>,
) -> Result<Json<Vec<Run>>, ApiError> {
    state
        .dispatch
        .runs(q.job.as_deref(), q.status.as_deref(), q.limit.unwrap_or(50))
        .await
        .map(Json)
        .map_err(db)
}

#[derive(Deserialize)]
struct NewRun {
    job: String,
    #[serde(default)]
    input: Value,
}

/// A run of a job by hand. Runs cost money, so it takes `admin`.
async fn start_run(
    State(state): State<AppState>,
    caller: Caller<Admin>,
    Json(new): Json<NewRun>,
) -> Result<(StatusCode, Json<Run>), ApiError> {
    let input = json!({ "manual": { "by": caller.principal.actor(), "input": new.input } });
    state
        .dispatch
        .start_manual(&new.job, &input)
        .await
        .map(|run| (StatusCode::ACCEPTED, Json(run)))
        .map_err(ApiError::bad_request)
}

#[derive(Deserialize)]
struct After {
    after: Option<i64>,
}

async fn run_events(
    State(state): State<AppState>,
    _caller: Caller<Read>,
    Path(id): Path<Uuid>,
    Query(q): Query<After>,
) -> Result<Json<Vec<RunEvent>>, ApiError> {
    state
        .dispatch
        .run_events(id, q.after.unwrap_or(0))
        .await
        .map(Json)
        .map_err(db)
}

#[derive(Deserialize)]
struct Message {
    text: String,
}

/// Steering: a message to a running or waiting agent.
async fn send_message(
    State(state): State<AppState>,
    _caller: Caller<Admin>,
    Path(id): Path<Uuid>,
    Json(message): Json<Message>,
) -> Result<Json<Value>, ApiError> {
    state
        .dispatch
        .send_message(id, &message.text)
        .await
        .map(|outcome| Json(json!({ "outcome": outcome })))
        .map_err(conflict)
}

async fn cancel_run(
    State(state): State<AppState>,
    _caller: Caller<Admin>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    state
        .dispatch
        .cancel_run(id)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(conflict)
}

#[derive(Deserialize)]
struct QuestionQuery {
    status: Option<String>,
    limit: Option<i64>,
}

async fn list_questions(
    State(state): State<AppState>,
    _caller: Caller<Read>,
    Query(q): Query<QuestionQuery>,
) -> Result<Json<Vec<Question>>, ApiError> {
    state
        .dispatch
        .questions(q.status.as_deref(), q.limit.unwrap_or(50))
        .await
        .map(Json)
        .map_err(db)
}

async fn question(
    State(state): State<AppState>,
    _caller: Caller<Read>,
    Path(id): Path<Uuid>,
) -> Result<Json<Question>, ApiError> {
    state
        .dispatch
        .question(id)
        .await
        .map_err(db)?
        .map(Json)
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "not-found",
                format!("question {id} not found"),
            )
        })
}

#[derive(Deserialize)]
struct Answer {
    answer: String,
}

/// A person answers an agent's question; the agent continues in the same session.
async fn answer_question(
    State(state): State<AppState>,
    caller: Caller<Capture>,
    Path(id): Path<Uuid>,
    Json(answer): Json<Answer>,
) -> Result<Json<Question>, ApiError> {
    caller.user_id()?;
    state
        .dispatch
        .answer_question(id, &answer.answer, &caller.principal.actor())
        .await
        .map(Json)
        .map_err(conflict)
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
