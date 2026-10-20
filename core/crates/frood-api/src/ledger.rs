//! `/v1` routes over the Ledger (docs/12_api_mcp_auth.md §2). Every route needs a bearer token
//! with the scope named by its `Caller<…>` argument.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, NaiveDate, Utc};
use frood_ledger::model::*;
use serde::Deserialize;
use uuid::Uuid;

use crate::AppState;
use crate::auth::{Caller, Capture, Decide, Journal, Propose, Read};
use crate::error::ApiError;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/sources", post(capture))
        .route("/v1/sources/{id}", get(source))
        .route("/v1/evidence", post(add_evidence))
        .route("/v1/observations", post(add_observation))
        .route("/v1/assertions", get(list_assertions))
        .route("/v1/assertions/{id}", get(assertion))
        .route("/v1/assertions/{id}/history", get(history))
        .route("/v1/entities", get(list_entities))
        .route("/v1/entities/{id}", get(entity))
        .route("/v1/proposals", post(open_proposal).get(list_proposals))
        .route("/v1/proposals/{id}", get(proposal))
        .route("/v1/proposals/{id}/verdicts", post(submit_verdict))
        .route("/v1/journal", get(journal))
}

type ApiResult<T> = Result<Json<T>, ApiError>;

async fn capture(
    State(state): State<AppState>,
    caller: Caller<Capture>,
    Json(input): Json<NewSource>,
) -> Result<(StatusCode, Json<CaptureResult>), ApiError> {
    let result = state.ledger.capture_source(&caller.actor(), input).await?;
    let status = if result.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(result)))
}

async fn source(
    State(state): State<AppState>,
    _caller: Caller<Read>,
    Path(id): Path<Uuid>,
) -> ApiResult<Source> {
    Ok(Json(state.ledger.source(id).await?))
}

async fn add_evidence(
    State(state): State<AppState>,
    _caller: Caller<Capture>,
    Json(input): Json<NewEvidence>,
) -> Result<(StatusCode, Json<Evidence>), ApiError> {
    Ok((
        StatusCode::CREATED,
        Json(state.ledger.add_evidence(input).await?),
    ))
}

async fn add_observation(
    State(state): State<AppState>,
    caller: Caller<Propose>,
    Json(input): Json<NewObservation>,
) -> Result<(StatusCode, Json<Observation>), ApiError> {
    Ok((
        StatusCode::CREATED,
        Json(state.ledger.add_observation(&caller.actor(), input).await?),
    ))
}

#[derive(Deserialize)]
struct PointInTime {
    as_of: Option<String>,
    valid_at: Option<String>,
}

async fn assertion(
    State(state): State<AppState>,
    _caller: Caller<Read>,
    Path(id): Path<Uuid>,
    Query(point): Query<PointInTime>,
) -> ApiResult<AssertionView> {
    let as_of = point.as_of.as_deref().map(parse_time).transpose()?;
    let valid_at = point.valid_at.as_deref().map(parse_time).transpose()?;
    Ok(Json(state.ledger.assertion(id, as_of, valid_at).await?))
}

async fn history(
    State(state): State<AppState>,
    _caller: Caller<Read>,
    Path(id): Path<Uuid>,
) -> ApiResult<Vec<AssertionVersion>> {
    Ok(Json(state.ledger.history(id).await?))
}

/// `GET /v1/assertions?kind=&entity=&status=&text=&valid_at=&as_of=&limit=&after=&slot.<name>.<op>=`
async fn list_assertions(
    State(state): State<AppState>,
    _caller: Caller<Read>,
    Query(params): Query<Vec<(String, String)>>,
) -> ApiResult<Vec<AssertionVersion>> {
    let mut filter = AssertionFilter::default();
    for (key, value) in params {
        match key.as_str() {
            "kind" => filter.kind = Some(value),
            "entity" => filter.entity = Some(parse_uuid(&value)?),
            "status" => filter.status = Some(value.parse().map_err(ApiError::bad_request)?),
            "text" => filter.text = Some(value),
            "valid_at" => filter.valid_at = Some(parse_time(&value)?),
            "as_of" => filter.as_of = Some(parse_time(&value)?),
            "limit" => {
                filter.limit = Some(
                    value
                        .parse()
                        .map_err(|_| ApiError::bad_request("limit must be a number"))?,
                )
            }
            "after" => filter.after = Some(parse_uuid(&value)?),
            other => match other
                .strip_prefix("slot.")
                .and_then(|rest| rest.rsplit_once('.'))
            {
                Some((name, op)) => filter.slots.push(SlotFilter {
                    key: name.to_owned(),
                    op: match op {
                        "eq" => SlotOp::Eq,
                        "gte" => SlotOp::Gte,
                        "lt" => SlotOp::Lt,
                        "in" => SlotOp::In,
                        _ => {
                            return Err(ApiError::bad_request(format!(
                                "slot operator {op:?} must be eq, gte, lt or in"
                            )));
                        }
                    },
                    value,
                }),
                None => {
                    return Err(ApiError::bad_request(format!(
                        "unknown query parameter {other:?}"
                    )));
                }
            },
        }
    }
    Ok(Json(state.ledger.assertions(&filter).await?))
}

#[derive(Deserialize)]
struct EntityQuery {
    name: Option<String>,
    #[serde(rename = "type")]
    entity_type: Option<String>,
    limit: Option<i64>,
}

async fn list_entities(
    State(state): State<AppState>,
    _caller: Caller<Read>,
    Query(query): Query<EntityQuery>,
) -> ApiResult<Vec<Entity>> {
    Ok(Json(
        state
            .ledger
            .entities(
                query.name.as_deref(),
                query.entity_type.as_deref(),
                query.limit.unwrap_or(50),
            )
            .await?,
    ))
}

async fn entity(
    State(state): State<AppState>,
    _caller: Caller<Read>,
    Path(id): Path<Uuid>,
) -> ApiResult<EntityView> {
    Ok(Json(state.ledger.entity(id).await?))
}

async fn open_proposal(
    State(state): State<AppState>,
    caller: Caller<Propose>,
    headers: HeaderMap,
    Json(mut input): Json<NewProposal>,
) -> Result<(StatusCode, Json<Proposal>), ApiError> {
    if let Some(key) = headers.get("idempotency-key") {
        let key = key
            .to_str()
            .map_err(|_| ApiError::bad_request("Idempotency-Key must be ASCII"))?;
        input.idempotency_key = Some(key.to_owned());
    }
    Ok((
        StatusCode::CREATED,
        Json(state.ledger.open_proposal(&caller.actor(), input).await?),
    ))
}

#[derive(Deserialize)]
struct ProposalQuery {
    state: Option<String>,
    after: Option<Uuid>,
    limit: Option<i64>,
}

async fn list_proposals(
    State(state): State<AppState>,
    _caller: Caller<Read>,
    Query(query): Query<ProposalQuery>,
) -> ApiResult<Vec<Proposal>> {
    let proposal_state = query
        .state
        .as_deref()
        .map(str::parse)
        .transpose()
        .map_err(ApiError::bad_request)?;
    Ok(Json(
        state
            .ledger
            .proposals(proposal_state, query.after, query.limit.unwrap_or(50))
            .await?,
    ))
}

async fn proposal(
    State(state): State<AppState>,
    _caller: Caller<Read>,
    Path(id): Path<Uuid>,
) -> ApiResult<Proposal> {
    Ok(Json(state.ledger.proposal(id).await?))
}

async fn submit_verdict(
    State(state): State<AppState>,
    caller: Caller<Decide>,
    Path(id): Path<Uuid>,
    Json(input): Json<VerdictInput>,
) -> ApiResult<Proposal> {
    Ok(Json(
        state
            .ledger
            .submit_verdict(&caller.actor(), id, input)
            .await?,
    ))
}

#[derive(Deserialize)]
struct JournalQuery {
    after: Option<i64>,
    limit: Option<i64>,
    /// Instead of `after`: the newest this many events.
    last: Option<i64>,
}

async fn journal(
    State(state): State<AppState>,
    _caller: Caller<Journal>,
    Query(query): Query<JournalQuery>,
) -> ApiResult<Vec<JournalEvent>> {
    if let Some(last) = query.last {
        let last = last.clamp(1, 1000);
        let head = state.ledger.journal_head().await?;
        return Ok(Json(
            state.ledger.journal((head - last).max(0), last).await?,
        ));
    }
    Ok(Json(
        state
            .ledger
            .journal(query.after.unwrap_or(0), query.limit.unwrap_or(100))
            .await?,
    ))
}

/// RFC 3339 timestamp, or a bare date meaning its start in UTC.
fn parse_time(value: &str) -> Result<DateTime<Utc>, ApiError> {
    if let Ok(at) = DateTime::parse_from_rfc3339(value) {
        return Ok(at.with_timezone(&Utc));
    }
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .ok()
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|naive| naive.and_utc())
        .ok_or_else(|| {
            ApiError::bad_request(format!(
                "{value:?} is not an RFC 3339 time or YYYY-MM-DD date"
            ))
        })
}

fn parse_uuid(value: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(value).map_err(|_| ApiError::bad_request(format!("{value:?} is not an id")))
}
