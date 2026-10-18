//! Reads: sources, assertions at a point in time, history, entities, proposals.

use sqlx::{PgConnection, PgPool, Postgres, QueryBuilder, Row};
use uuid::Uuid;

use crate::error::{LedgerError, Result, invalid, parse_text};
use crate::model::{
    AssertionFilter, AssertionVersion, AssertionView, Entity, EntityView, Flag, Link, Proposal,
    ProposalState, SlotOp, Source, Timestamp,
};
use crate::rows::{self, version_columns};

const MAX_LIMIT: i64 = 200;

pub(crate) async fn source(pool: &PgPool, id: Uuid) -> Result<Source> {
    let row = sqlx::query(
        "SELECT id, kind, origin, uri, title, content, content_sha256, said_at, captured_by, captured_at, metadata \
         FROM sources WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| LedgerError::NotFound(format!("source {id}")))?;
    let evidence = sqlx::query("SELECT id, source_id, anchor, quote, created_at FROM evidence WHERE source_id = $1 ORDER BY id")
        .bind(id)
        .fetch_all(pool)
        .await?
        .iter()
        .map(rows::evidence)
        .collect::<Result<Vec<_>>>()?;
    Ok(Source {
        id: row.try_get("id")?,
        kind: row.try_get("kind")?,
        origin: row.try_get("origin")?,
        uri: row.try_get("uri")?,
        title: row.try_get("title")?,
        content: row.try_get("content")?,
        content_sha256: row.try_get("content_sha256")?,
        said_at: row.try_get("said_at")?,
        captured_by: row.try_get("captured_by")?,
        captured_at: row.try_get("captured_at")?,
        metadata: row.try_get("metadata")?,
        evidence,
    })
}

/// Rows of the chosen recorded snapshot (`as_of`) and validity point (`valid_at`).
fn push_snapshot(
    builder: &mut QueryBuilder<Postgres>,
    as_of: Option<Timestamp>,
    valid_at: Option<Timestamp>,
) {
    match as_of {
        None => {
            builder.push(" recorded_to IS NULL");
        }
        Some(at) => {
            builder.push(" recorded_from <= ").push_bind(at);
            builder
                .push(" AND (recorded_to IS NULL OR recorded_to > ")
                .push_bind(at)
                .push(")");
        }
    }
    if let Some(at) = valid_at {
        builder.push(" AND valid_from <= ").push_bind(at);
        builder
            .push(" AND (valid_to IS NULL OR valid_to > ")
            .push_bind(at)
            .push(")");
    }
}

pub(crate) async fn assertion(
    pool: &PgPool,
    id: Uuid,
    as_of: Option<Timestamp>,
    valid_at: Option<Timestamp>,
) -> Result<AssertionView> {
    let mut builder = QueryBuilder::new(concat!(
        "SELECT ",
        version_columns!(),
        " FROM assertion_versions WHERE assertion_id = "
    ));
    builder.push_bind(id).push(" AND");
    push_snapshot(&mut builder, as_of, valid_at);
    builder.push(" ORDER BY valid_from DESC LIMIT 1");
    let row =
        builder.build().fetch_optional(pool).await?.ok_or_else(|| {
            LedgerError::NotFound(format!("assertion {id} at the requested time"))
        })?;
    let version = rows::version(&row)?;

    let flags = sqlx::query_scalar::<_, String>(
        "SELECT flag FROM assertion_flags WHERE assertion_id = $1 ORDER BY flag",
    )
    .bind(id)
    .fetch_all(pool)
    .await?
    .iter()
    .map(|f| parse_text::<Flag>(f))
    .collect::<Result<Vec<_>>>()?;
    let links = sqlx::query(
        "SELECT from_assertion, to_assertion, kind, recorded_from FROM assertion_links \
         WHERE (from_assertion = $1 OR to_assertion = $1) AND recorded_to IS NULL ORDER BY recorded_from",
    )
    .bind(id)
    .fetch_all(pool)
    .await?
    .iter()
    .map(|row| {
        Ok(Link {
            from: row.try_get("from_assertion")?,
            to: row.try_get("to_assertion")?,
            kind: parse_text(row.try_get::<&str, _>("kind")?)?,
            recorded_from: row.try_get("recorded_from")?,
        })
    })
    .collect::<Result<Vec<_>>>()?;
    Ok(AssertionView {
        version,
        flags,
        links,
    })
}

pub(crate) async fn history(pool: &PgPool, id: Uuid) -> Result<Vec<AssertionVersion>> {
    let versions = sqlx::query(concat!(
        "SELECT ",
        version_columns!(),
        " FROM assertion_versions WHERE assertion_id = $1 ORDER BY version"
    ))
    .bind(id)
    .fetch_all(pool)
    .await?
    .iter()
    .map(rows::version)
    .collect::<Result<Vec<_>>>()?;
    if versions.is_empty() {
        return Err(LedgerError::NotFound(format!("assertion {id}")));
    }
    Ok(versions)
}

pub(crate) async fn assertions(
    pool: &PgPool,
    filter: &AssertionFilter,
) -> Result<Vec<AssertionVersion>> {
    let mut builder = QueryBuilder::new(concat!(
        "SELECT * FROM (SELECT DISTINCT ON (assertion_id) ",
        version_columns!(),
        " FROM assertion_versions WHERE"
    ));
    push_snapshot(&mut builder, filter.as_of, filter.valid_at);
    builder.push(" ORDER BY assertion_id, valid_from DESC) v WHERE TRUE");

    if let Some(kind) = &filter.kind {
        builder.push(" AND kind = ").push_bind(kind.clone());
    }
    if let Some(status) = filter.status {
        builder.push(" AND status = ").push_bind(status.as_str());
    }
    if let Some(text) = &filter.text {
        let pattern = format!(
            "%{}%",
            text.replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        builder.push(" AND text ILIKE ").push_bind(pattern);
    }
    if let Some(entity) = filter.entity {
        let group = entity_group(pool, entity).await?;
        builder.push(" AND subjects && ").push_bind(group);
    }
    for slot in &filter.slots {
        if !is_slot_key(&slot.key) {
            return Err(invalid(format!(
                "slot name {:?} must be lowercase letters, digits and underscores",
                slot.key
            )));
        }
        builder.push(" AND slots ->> ").push_bind(slot.key.clone());
        match slot.op {
            SlotOp::Eq => builder.push(" = ").push_bind(slot.value.clone()),
            SlotOp::Gte => builder.push(" >= ").push_bind(slot.value.clone()),
            SlotOp::Lt => builder.push(" < ").push_bind(slot.value.clone()),
            SlotOp::In => {
                let values: Vec<String> =
                    slot.value.split(',').map(|v| v.trim().to_owned()).collect();
                builder.push(" = ANY(").push_bind(values).push(")")
            }
        };
    }
    if let Some(after) = filter.after {
        builder.push(" AND assertion_id > ").push_bind(after);
    }
    builder
        .push(" ORDER BY assertion_id LIMIT ")
        .push_bind(filter.limit.unwrap_or(50).clamp(1, MAX_LIMIT));

    builder
        .build()
        .fetch_all(pool)
        .await?
        .iter()
        .map(rows::version)
        .collect()
}

fn is_slot_key(key: &str) -> bool {
    let mut chars = key.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c == '_')
        && key.len() <= 64
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// The entity, its merge target and everything merged into that target.
async fn entity_group(pool: &PgPool, id: Uuid) -> Result<Vec<Uuid>> {
    let root: Option<Option<Uuid>> =
        sqlx::query_scalar("SELECT merged_into FROM entities WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await?;
    let root = match root {
        Some(target) => target.unwrap_or(id),
        None => return Err(LedgerError::NotFound(format!("entity {id}"))),
    };
    Ok(
        sqlx::query_scalar("SELECT id FROM entities WHERE id = $1 OR merged_into = $1 ORDER BY id")
            .bind(root)
            .fetch_all(pool)
            .await?,
    )
}

pub(crate) async fn entity(pool: &PgPool, id: Uuid) -> Result<EntityView> {
    let row = sqlx::query("SELECT id, type, name, aliases, merged_into, created_by, created_at FROM entities WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| LedgerError::NotFound(format!("entity {id}")))?;
    let entity = rows::entity(&row)?;
    let merged = sqlx::query_scalar("SELECT id FROM entities WHERE merged_into = $1 ORDER BY id")
        .bind(id)
        .fetch_all(pool)
        .await?;
    let assertions = assertions(
        pool,
        &AssertionFilter {
            entity: Some(id),
            limit: Some(MAX_LIMIT),
            ..Default::default()
        },
    )
    .await?;
    Ok(EntityView {
        entity,
        merged,
        assertions,
    })
}

pub(crate) async fn entities(
    pool: &PgPool,
    name: Option<&str>,
    entity_type: Option<&str>,
    limit: i64,
) -> Result<Vec<Entity>> {
    let mut builder = QueryBuilder::new(
        "SELECT id, type, name, aliases, merged_into, created_by, created_at FROM entities WHERE merged_into IS NULL",
    );
    if let Some(name) = name {
        let pattern = format!("%{}%", name.to_lowercase());
        builder
            .push(" AND (lower(name) LIKE ")
            .push_bind(pattern.clone());
        builder
            .push(" OR EXISTS (SELECT 1 FROM unnest(aliases) a WHERE lower(a) LIKE ")
            .push_bind(pattern)
            .push("))");
    }
    if let Some(entity_type) = entity_type {
        builder
            .push(" AND type = ")
            .push_bind(entity_type.to_owned());
    }
    builder
        .push(" ORDER BY name LIMIT ")
        .push_bind(limit.clamp(1, MAX_LIMIT));
    builder
        .build()
        .fetch_all(pool)
        .await?
        .iter()
        .map(rows::entity)
        .collect()
}

pub(crate) async fn proposal(conn: &mut PgConnection, id: Uuid) -> Result<Proposal> {
    let row = sqlx::query(
        "SELECT id, idempotency_key, author, rationale, risk, confidence, base, alternatives, evidence, state, \
         deferred_until, created_at, decided_at FROM proposals WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| LedgerError::NotFound(format!("proposal {id}")))?;
    let mut proposal = rows::proposal(&row)?;
    proposal.verdicts = sqlx::query(
        "SELECT id, proposal_id, decision, alternative, edits, reason, decided_by, created_at FROM verdicts \
         WHERE proposal_id = $1 ORDER BY created_at, id",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?
    .iter()
    .map(rows::verdict)
    .collect::<Result<Vec<_>>>()?;
    Ok(proposal)
}

pub(crate) async fn proposals(
    pool: &PgPool,
    state: Option<ProposalState>,
    after: Option<Uuid>,
    limit: i64,
) -> Result<Vec<Proposal>> {
    let mut builder = QueryBuilder::new(
        "SELECT id, idempotency_key, author, rationale, risk, confidence, base, alternatives, evidence, state, \
         deferred_until, created_at, decided_at FROM proposals WHERE TRUE",
    );
    if let Some(state) = state {
        builder.push(" AND state = ").push_bind(state.as_str());
    }
    if let Some(after) = after {
        builder.push(" AND id > ").push_bind(after);
    }
    builder
        .push(" ORDER BY id LIMIT ")
        .push_bind(limit.clamp(1, MAX_LIMIT));
    builder
        .build()
        .fetch_all(pool)
        .await?
        .iter()
        .map(rows::proposal)
        .collect()
}
