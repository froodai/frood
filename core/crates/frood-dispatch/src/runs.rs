//! The run queue: one row per (job, event), claimed with `SKIP LOCKED`, retried with growing
//! delays.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use sqlx::{PgPool, Row};
use uuid::Uuid;

/// Attempts before a run with transient failures gives up.
pub const MAX_ATTEMPTS: i32 = 5;

#[derive(Debug, Clone, Serialize)]
pub struct Run {
    pub id: Uuid,
    pub job: String,
    pub subject: Option<String>,
    pub status: String,
    pub attempts: i32,
    pub input: Value,
    pub output: Option<Value>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    /// Agent runs: the module running it, its session, what it cost so far.
    pub runtime: Option<String>,
    pub session_ref: Option<String>,
    pub cost_usd: f64,
    pub on_behalf_of: Option<Uuid>,
    pub pending_messages: Value,
}

macro_rules! columns {
    () => {
        "id, job, subject, status, attempts, input, output, error, created_at, started_at, finished_at, \
         runtime, session_ref, cost_usd, on_behalf_of, pending_messages"
    };
}

fn run(row: &sqlx::postgres::PgRow) -> sqlx::Result<Run> {
    Ok(Run {
        id: row.try_get("id")?,
        job: row.try_get("job")?,
        subject: row.try_get("subject")?,
        status: row.try_get("status")?,
        attempts: row.try_get("attempts")?,
        input: row.try_get("input")?,
        output: row.try_get("output")?,
        error: row.try_get("error")?,
        created_at: row.try_get("created_at")?,
        started_at: row.try_get("started_at")?,
        finished_at: row.try_get("finished_at")?,
        runtime: row.try_get("runtime")?,
        session_ref: row.try_get("session_ref")?,
        cost_usd: row.try_get("cost_usd")?,
        on_behalf_of: row.try_get("on_behalf_of")?,
        pending_messages: row.try_get("pending_messages")?,
    })
}

/// Takes the next due run and marks it running.
pub(crate) async fn claim(pool: &PgPool) -> sqlx::Result<Option<Run>> {
    let row = sqlx::query(concat!(
        "UPDATE runs SET status = 'running', attempts = attempts + 1, started_at = now() \
         WHERE id = (SELECT id FROM runs WHERE status = 'queued' AND run_after <= now() \
                     ORDER BY run_after, created_at LIMIT 1 FOR UPDATE SKIP LOCKED) RETURNING ",
        columns!()
    ))
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(run).transpose()
}

/// Keeps what a run produced so far, so a retry continues instead of starting over.
pub(crate) async fn save_progress(pool: &PgPool, id: Uuid, output: &Value) -> sqlx::Result<()> {
    sqlx::query("UPDATE runs SET output = $2 WHERE id = $1")
        .bind(id)
        .bind(output)
        .execute(pool)
        .await?;
    Ok(())
}

pub(crate) async fn succeed(pool: &PgPool, id: Uuid, output: &Value) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE runs SET status = 'succeeded', output = $2, error = NULL, finished_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(output)
    .execute(pool)
    .await?;
    Ok(())
}

/// A failed attempt: queued again after a delay if `retry`, otherwise failed for good.
pub(crate) async fn fail(
    pool: &PgPool,
    id: Uuid,
    error: &str,
    retry: Option<i64>,
) -> sqlx::Result<()> {
    match retry {
        Some(seconds) => sqlx::query(
            "UPDATE runs SET status = 'queued', error = $2, run_after = now() + make_interval(secs => $3) WHERE id = $1",
        )
        .bind(id)
        .bind(error)
        .bind(seconds as f64)
        .execute(pool)
        .await?,
        None => sqlx::query(
            "UPDATE runs SET status = 'failed', error = $2, finished_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(error)
        .execute(pool)
        .await?,
    };
    Ok(())
}

pub(crate) async fn cancel(pool: &PgPool, id: Uuid, reason: &str) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE runs SET status = 'cancelled', error = $2, finished_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(reason)
    .execute(pool)
    .await?;
    Ok(())
}

/// Runs left `running` by a process that stopped go back to the queue. Agent runs keep
/// running in their runtime: the watcher picks them up again.
pub(crate) async fn requeue_interrupted(pool: &PgPool) -> sqlx::Result<u64> {
    Ok(sqlx::query(
        "UPDATE runs SET status = 'queued', run_after = now() WHERE status = 'running' AND runtime IS NULL",
    )
    .execute(pool)
    .await?
    .rows_affected())
}

/// An agent run handed to its runtime: it stays `running` until the watcher sees it end.
pub(crate) async fn set_started(
    pool: &PgPool,
    id: Uuid,
    runtime: &str,
    session_ref: Option<&str>,
    on_behalf_of: Option<Uuid>,
) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE runs SET runtime = $2, session_ref = coalesce($3, session_ref), \
         on_behalf_of = coalesce($4, on_behalf_of), pending_messages = '[]', error = NULL WHERE id = $1",
    )
    .bind(id)
    .bind(runtime)
    .bind(session_ref)
    .bind(on_behalf_of)
    .execute(pool)
    .await?;
    Ok(())
}

pub(crate) async fn set_waiting(pool: &PgPool, id: Uuid) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE runs SET status = 'waiting_for_input' WHERE id = $1 AND status = 'running'",
    )
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// A waiting run whose questions are settled goes back to the queue to be resumed.
pub(crate) async fn requeue_for_resume(pool: &PgPool, id: Uuid) -> sqlx::Result<bool> {
    Ok(sqlx::query(
        "UPDATE runs SET status = 'queued', run_after = now() WHERE id = $1 AND status = 'waiting_for_input'",
    )
    .bind(id)
    .execute(pool)
    .await?
    .rows_affected()
        > 0)
}

pub(crate) async fn set_progress(
    pool: &PgPool,
    id: Uuid,
    cost_usd: f64,
    session_ref: Option<&str>,
) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE runs SET cost_usd = greatest(cost_usd, $2), session_ref = coalesce($3, session_ref) WHERE id = $1",
    )
    .bind(id)
    .bind(cost_usd)
    .bind(session_ref)
    .execute(pool)
    .await?;
    Ok(())
}

/// A steering message for a run that cannot take it now; delivered when it resumes.
pub(crate) async fn push_message(pool: &PgPool, id: Uuid, text: &str) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE runs SET pending_messages = pending_messages || jsonb_build_array($2::text) WHERE id = $1",
    )
    .bind(id)
    .bind(text)
    .execute(pool)
    .await?;
    Ok(())
}

/// Agent runs the watcher looks after.
pub(crate) async fn active(pool: &PgPool) -> sqlx::Result<Vec<Run>> {
    let rows = sqlx::query(concat!(
        "SELECT ",
        columns!(),
        " FROM runs WHERE runtime IS NOT NULL AND status IN ('running', 'waiting_for_input') ORDER BY created_at"
    ))
    .fetch_all(pool)
    .await?;
    rows.iter().map(run).collect()
}

#[derive(Debug, Clone, Serialize)]
pub struct RunEvent {
    pub seq: i64,
    pub kind: String,
    pub data: Value,
    pub at: DateTime<Utc>,
}

pub(crate) async fn add_event(
    pool: &PgPool,
    run_id: Uuid,
    kind: &str,
    data: &Value,
) -> sqlx::Result<()> {
    sqlx::query("INSERT INTO run_events (run_id, kind, data) VALUES ($1, $2, $3)")
        .bind(run_id)
        .bind(kind)
        .bind(data)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn events(
    pool: &PgPool,
    run_id: Uuid,
    after: i64,
    limit: i64,
) -> sqlx::Result<Vec<RunEvent>> {
    let rows = sqlx::query(
        "SELECT seq, kind, data, at FROM run_events WHERE run_id = $1 AND seq > $2 ORDER BY seq LIMIT $3",
    )
    .bind(run_id)
    .bind(after)
    .bind(limit.clamp(1, 1000))
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| {
            Ok(RunEvent {
                seq: row.try_get("seq")?,
                kind: row.try_get("kind")?,
                data: row.try_get("data")?,
                at: row.try_get("at")?,
            })
        })
        .collect()
}

/// How many runtime events of a run are stored (the runtime numbers its events from 1).
pub(crate) async fn runtime_events_seen(pool: &PgPool, run_id: Uuid) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "SELECT count(*) FROM run_events WHERE run_id = $1 AND kind LIKE 'runtime.%'",
    )
    .bind(run_id)
    .fetch_one(pool)
    .await
}

pub async fn list(
    pool: &PgPool,
    job: Option<&str>,
    status: Option<&str>,
    limit: i64,
) -> sqlx::Result<Vec<Run>> {
    let rows = sqlx::query(concat!(
        "SELECT ",
        columns!(),
        " FROM runs WHERE ($1::text IS NULL OR job = $1) AND ($2::text IS NULL OR status = $2) \
          ORDER BY created_at DESC LIMIT $3"
    ))
    .bind(job)
    .bind(status)
    .bind(limit.clamp(1, 200))
    .fetch_all(pool)
    .await?;
    rows.iter().map(run).collect()
}

/// A run created by hand (`POST /v1/runs`), not by an event.
pub(crate) async fn create_manual(pool: &PgPool, job: &str, input: &Value) -> sqlx::Result<Run> {
    let id = Uuid::now_v7();
    let row = sqlx::query(concat!(
        "INSERT INTO runs (id, job, idempotency_key, subject, input, status) \
         VALUES ($1, $2, $3, $4, $5, 'queued') RETURNING ",
        columns!()
    ))
    .bind(id)
    .bind(job)
    .bind(format!("{job}:manual:{id}"))
    .bind(format!("run/{id}"))
    .bind(input)
    .fetch_one(pool)
    .await?;
    run(&row)
}

pub async fn for_subject(pool: &PgPool, subject: &str) -> sqlx::Result<Vec<Run>> {
    let rows = sqlx::query(concat!(
        "SELECT ",
        columns!(),
        " FROM runs WHERE subject = $1 ORDER BY created_at"
    ))
    .bind(subject)
    .fetch_all(pool)
    .await?;
    rows.iter().map(run).collect()
}

pub async fn get(pool: &PgPool, id: Uuid) -> sqlx::Result<Option<Run>> {
    let row = sqlx::query(concat!("SELECT ", columns!(), " FROM runs WHERE id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await?;
    row.as_ref().map(run).transpose()
}

/// Delay before attempt `attempts + 1`: 10 s, 20 s, 40 s, 80 s…
pub(crate) fn backoff(attempts: i32) -> i64 {
    10 * 2_i64.pow(attempts.clamp(1, 10) as u32 - 1)
}
