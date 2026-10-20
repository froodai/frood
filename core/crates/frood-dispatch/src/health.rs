//! How the installation is doing, for the admin panel and `/metrics` (docs/16 §9).

use chrono::Utc;
use serde::Serialize;

use crate::{Dispatch, runs};

#[derive(Debug, Serialize)]
pub struct Health {
    pub modules: Vec<ModuleHealth>,
    /// Age of the oldest journal event the dispatcher has not read, in seconds.
    pub dispatcher_lag_seconds: i64,
    pub undispatched_events: i64,
    pub runs: RunCounts,
    pub inbox_open: i64,
    pub alerts_open: i64,
    pub accounts: Vec<AccountSpend>,
    /// Per job and source: seconds since it was last read successfully.
    pub sources: Vec<SourceAge>,
}

#[derive(Debug, Serialize)]
pub struct ModuleHealth {
    pub name: String,
    pub up: bool,
    pub provides: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct RunCounts {
    pub queued: i64,
    pub running: i64,
    pub waiting_for_input: i64,
    pub failed_last_day: i64,
    pub succeeded_last_day: i64,
}

#[derive(Debug, Serialize)]
pub struct AccountSpend {
    pub account: String,
    pub spent_usd: f64,
    pub monthly_usd: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct SourceAge {
    pub job: String,
    pub source: String,
    pub seconds_since_read: Option<i64>,
}

impl Dispatch {
    pub async fn health(&self) -> sqlx::Result<Health> {
        let pool = &self.ctx.pool;
        let mut modules = Vec::new();
        for name in self.ctx.modules.names() {
            modules.push(ModuleHealth {
                name: name.to_owned(),
                up: self.ctx.modules.healthy(name).await,
                provides: self
                    .ctx
                    .modules
                    .manifest(name)
                    .map(|m| m.provides)
                    .unwrap_or_default(),
            });
        }
        let (undispatched, oldest): (i64, Option<chrono::DateTime<Utc>>) = sqlx::query_as(
            "SELECT count(*), min(occurred_at) FROM journal \
             WHERE position > coalesce((SELECT position FROM journal_checkpoints WHERE reader = 'dispatcher'), 0)",
        )
        .fetch_one(pool)
        .await?;
        let count = |status: &'static str| async move {
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM runs WHERE status = $1")
                .bind(status)
                .fetch_one(pool)
                .await
        };
        let finished = |status: &'static str| async move {
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM runs WHERE status = $1 AND finished_at > now() - interval '1 day'",
            )
            .bind(status)
            .fetch_one(pool)
            .await
        };
        let runs = RunCounts {
            queued: count("queued").await?,
            running: count("running").await?,
            waiting_for_input: count("waiting_for_input").await?,
            failed_last_day: finished("failed").await?,
            succeeded_last_day: finished("succeeded").await?,
        };
        let (inbox_open, alerts_open): (i64, i64) = sqlx::query_as(
            "SELECT count(*), count(*) FILTER (WHERE kind = 'alert') FROM inbox_items WHERE state <> 'resolved'",
        )
        .fetch_one(pool)
        .await?;
        let mut accounts = Vec::new();
        let names: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT account FROM runs WHERE account IS NOT NULL ORDER BY 1",
        )
        .fetch_all(pool)
        .await?;
        for account in names {
            accounts.push(AccountSpend {
                spent_usd: runs::spent_this_month(pool, &account).await?,
                monthly_usd: self
                    .ctx
                    .gateway
                    .credential(&account)
                    .and_then(|c| c.monthly_usd),
                account,
            });
        }
        let ages: Vec<(String, String, Option<i64>)> = sqlx::query_as(
            "SELECT reader, source, extract(epoch FROM now() - (checkpoint->>'read_until')::timestamptz)::bigint \
             FROM source_checkpoints ORDER BY reader, source",
        )
        .fetch_all(pool)
        .await?;
        Ok(Health {
            modules,
            dispatcher_lag_seconds: oldest.map_or(0, |at| (Utc::now() - at).num_seconds().max(0)),
            undispatched_events: undispatched,
            runs,
            inbox_open,
            alerts_open,
            accounts,
            sources: ages
                .into_iter()
                .map(|(job, source, seconds_since_read)| SourceAge {
                    job,
                    source,
                    seconds_since_read,
                })
                .collect(),
        })
    }
}
