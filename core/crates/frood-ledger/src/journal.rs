//! Appending to and reading the journal (docs/11 §10).

use serde_json::Value;
use sqlx::{PgConnection, PgPool, Row};
use uuid::Uuid;

use crate::error::Result;
use crate::model::JournalEvent;

/// Key of the advisory lock that orders journal appends.
const JOURNAL_LOCK: i64 = 0x0f00_d001;

pub(crate) struct Event {
    pub event_type: &'static str,
    pub subject: Option<String>,
    pub data: Value,
}

impl Event {
    pub fn new(event_type: &'static str, subject: impl Into<Option<String>>, data: Value) -> Self {
        Self {
            event_type,
            subject: subject.into(),
            data,
        }
    }
}

/// Appends events inside the caller's transaction. The advisory lock is held until commit, so
/// positions become visible in increasing order.
pub(crate) async fn append(conn: &mut PgConnection, actor: &str, events: &[Event]) -> Result<()> {
    if events.is_empty() {
        return Ok(());
    }
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(JOURNAL_LOCK)
        .execute(&mut *conn)
        .await?;
    for event in events {
        sqlx::query(
            "INSERT INTO journal (id, type, subject, actor, data) VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(Uuid::now_v7())
        .bind(event.event_type)
        .bind(&event.subject)
        .bind(actor)
        .bind(&event.data)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

pub(crate) async fn read(pool: &PgPool, after: i64, limit: i64) -> Result<Vec<JournalEvent>> {
    let rows = sqlx::query(
        "SELECT position, id, type, subject, actor, data, occurred_at FROM journal \
         WHERE position > $1 ORDER BY position LIMIT $2",
    )
    .bind(after)
    .bind(limit.clamp(1, 1000))
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| {
            Ok(JournalEvent {
                position: row.try_get("position")?,
                id: row.try_get("id")?,
                event_type: row.try_get("type")?,
                subject: row.try_get("subject")?,
                actor: row.try_get("actor")?,
                data: row.try_get("data")?,
                occurred_at: row.try_get("occurred_at")?,
            })
        })
        .collect()
}
