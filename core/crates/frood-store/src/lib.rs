//! PostgreSQL connection, schema migrations and the core's own tables.
//!
//! All core migrations live in one sequence under `migrations/`; later milestones (the Ledger in
//! M1) add files there.

use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

pub use sqlx;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
}

/// Pool that connects on first use, so the server can start before the database is up.
pub fn connect_lazy(url: &str) -> Result<PgPool, StoreError> {
    Ok(PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(Duration::from_secs(3))
        .connect_lazy(url)?)
}

/// Connects now, retrying while the database is still starting.
pub async fn connect(url: &str, wait: Duration) -> Result<PgPool, StoreError> {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        match PgPoolOptions::new()
            .max_connections(10)
            .acquire_timeout(Duration::from_secs(3))
            .connect(url)
            .await
        {
            Ok(pool) => return Ok(pool),
            Err(err) if tokio::time::Instant::now() < deadline => {
                tracing::debug!(error = %err, "database not reachable yet, retrying");
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            Err(err) => return Err(err.into()),
        }
    }
}

/// Applies pending migrations. Safe to run concurrently: sqlx takes an advisory lock.
pub async fn migrate(pool: &PgPool) -> Result<(), StoreError> {
    sqlx::migrate!("./migrations").run(pool).await?;
    Ok(())
}

/// Readiness probe: the database answers.
pub async fn ping(pool: &PgPool) -> Result<(), StoreError> {
    sqlx::query("SELECT 1").execute(pool).await?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigRevision {
    pub id: i64,
    pub sha256: String,
    pub source: String,
    pub applied_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordOutcome {
    /// The config differs from the last applied one; a new revision was written.
    Recorded(ConfigRevision),
    /// The config is identical to the last applied revision; nothing was written.
    Unchanged(ConfigRevision),
}

pub async fn last_config_revision(pool: &PgPool) -> Result<Option<ConfigRevision>, StoreError> {
    let row: Option<(i64, String, String, DateTime<Utc>)> = sqlx::query_as(
        "SELECT id, sha256, source, applied_at FROM config_revisions ORDER BY id DESC LIMIT 1",
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(id, sha256, source, applied_at)| ConfigRevision {
        id,
        sha256,
        source,
        applied_at,
    }))
}

/// Records an applied config unless it equals the last revision.
pub async fn record_config_revision(
    pool: &PgPool,
    sha256: &str,
    content: &str,
    source: &str,
) -> Result<RecordOutcome, StoreError> {
    let mut tx = pool.begin().await?;
    // Serialize concurrent applies so two identical configs cannot both be recorded.
    sqlx::query("LOCK TABLE config_revisions IN SHARE ROW EXCLUSIVE MODE")
        .execute(&mut *tx)
        .await?;
    let last: Option<(i64, String, String, DateTime<Utc>)> = sqlx::query_as(
        "SELECT id, sha256, source, applied_at FROM config_revisions ORDER BY id DESC LIMIT 1",
    )
    .fetch_optional(&mut *tx)
    .await?;
    if let Some((id, last_sha, last_source, applied_at)) = last
        && last_sha == sha256
    {
        tx.commit().await?;
        return Ok(RecordOutcome::Unchanged(ConfigRevision {
            id,
            sha256: last_sha,
            source: last_source,
            applied_at,
        }));
    }
    let (id, applied_at): (i64, DateTime<Utc>) = sqlx::query_as(
        "INSERT INTO config_revisions (sha256, content, source) VALUES ($1, $2, $3) RETURNING id, applied_at",
    )
    .bind(sha256)
    .bind(content)
    .bind(source)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(RecordOutcome::Recorded(ConfigRevision {
        id,
        sha256: sha256.to_owned(),
        source: source.to_owned(),
        applied_at,
    }))
}

/// Test helpers shared with other crates' integration tests.
pub mod testing {
    use super::*;

    /// Database URL for integration tests; tests skip themselves when it is unset.
    pub fn database_url() -> Option<String> {
        std::env::var("FROOD_TEST_DATABASE_URL").ok()
    }

    /// Creates a fresh, migrated database named after `prefix` and returns a pool to it.
    pub async fn fresh_database(admin_url: &str, prefix: &str) -> Result<PgPool, StoreError> {
        let admin = connect(admin_url, Duration::from_secs(10)).await?;
        let name = format!(
            "{prefix}_{}_{}",
            std::process::id(),
            Utc::now().timestamp_micros()
        );
        // The name is generated above from digits and a fixed prefix, so it is safe to splice.
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE \"{name}\"")))
            .execute(&admin)
            .await?;
        let url = match admin_url.rsplit_once('/') {
            Some((base, _)) => format!("{base}/{name}"),
            None => admin_url.to_owned(),
        };
        let pool = connect(&url, Duration::from_secs(10)).await?;
        migrate(&pool).await?;
        Ok(pool)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn revisions_are_recorded_once_per_content() {
        let Some(url) = testing::database_url() else {
            eprintln!("FROOD_TEST_DATABASE_URL is not set; skipping");
            return;
        };
        let pool = testing::fresh_database(&url, "frood_store").await.unwrap();
        assert_eq!(last_config_revision(&pool).await.unwrap(), None);

        let sha_a = "a".repeat(64);
        let first = record_config_revision(&pool, &sha_a, "version: 1", "file")
            .await
            .unwrap();
        let RecordOutcome::Recorded(first) = first else {
            panic!("expected a new revision")
        };

        let again = record_config_revision(&pool, &sha_a, "version: 1", "file")
            .await
            .unwrap();
        assert_eq!(again, RecordOutcome::Unchanged(first.clone()));

        let sha_b = "b".repeat(64);
        let RecordOutcome::Recorded(second) =
            record_config_revision(&pool, &sha_b, "version: 1 # changed", "git")
                .await
                .unwrap()
        else {
            panic!("expected a new revision")
        };
        assert!(second.id > first.id);
        assert_eq!(
            last_config_revision(&pool).await.unwrap().unwrap().source,
            "git"
        );

        // Migrations are idempotent.
        migrate(&pool).await.unwrap();
    }
}
