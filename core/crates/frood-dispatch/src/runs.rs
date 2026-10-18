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
}

macro_rules! columns {
    () => {
        "id, job, subject, status, attempts, input, output, error, created_at, started_at, finished_at"
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

/// Runs left `running` by a process that stopped go back to the queue.
pub(crate) async fn requeue_interrupted(pool: &PgPool) -> sqlx::Result<u64> {
    Ok(
        sqlx::query(
            "UPDATE runs SET status = 'queued', run_after = now() WHERE status = 'running'",
        )
        .execute(pool)
        .await?
        .rows_affected(),
    )
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
