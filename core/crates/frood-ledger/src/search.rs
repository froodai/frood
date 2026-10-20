//! Search over current knowledge (docs/14 §8): words through Postgres full-text search and
//! meaning through embeddings (pgvector), merged by reciprocal rank. Embeddings are optional:
//! without them search matches words only.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{PgPool, Postgres, QueryBuilder, Row};
use uuid::Uuid;

use crate::error::Result;
use crate::model::{AssertionVersion, Status};
use crate::rows::{self, version_columns};

/// Reciprocal rank fusion constant.
const RRF_K: f64 = 60.0;

/// A version, and its fused score with which lists found it (words, meaning).
type Ranked = ((Uuid, i32), (f64, bool, bool));

#[derive(Debug, Clone, Default)]
pub struct SearchQuery {
    pub text: String,
    /// Query embedding and the model that made it.
    pub embedding: Option<(String, Vec<f32>)>,
    pub kind: Option<String>,
    pub entity: Option<Uuid>,
    /// Default: active.
    pub status: Option<Status>,
    /// What was true at this time; default now.
    pub valid_at: Option<DateTime<Utc>>,
    pub limit: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    #[serde(flatten)]
    pub version: AssertionVersion,
    pub score: f64,
    /// `words`, `meaning` or `both`.
    pub matched: &'static str,
}

/// Words of a query as an OR tsquery. Words match as prefixes and long words lose their last
/// two letters: a crude stemmer that lets "релиза" find "релиз" and "deadlines" "deadline".
pub(crate) fn tsquery(text: &str) -> Option<String> {
    let mut words: Vec<String> = Vec::new();
    for word in text.split(|c: char| !c.is_alphanumeric()) {
        let word = word.to_lowercase();
        let chars: Vec<char> = word.chars().collect();
        if chars.len() < 2 {
            continue;
        }
        let term = match chars.len() {
            6.. => format!("{}:*", chars[..chars.len() - 2].iter().collect::<String>()),
            4..=5 => format!("{word}:*"),
            _ => word,
        };
        if !words.contains(&term) {
            words.push(term);
        }
        if words.len() == 32 {
            break;
        }
    }
    (!words.is_empty()).then(|| words.join(" | "))
}

fn vector_literal(vector: &[f32]) -> String {
    let items: Vec<String> = vector.iter().map(|v| v.to_string()).collect();
    format!("[{}]", items.join(","))
}

fn push_filters(builder: &mut QueryBuilder<Postgres>, query: &SearchQuery, table: &str) {
    builder
        .push(format!(
            " AND {table}.recorded_to IS NULL AND {table}.status = "
        ))
        .push_bind(query.status.unwrap_or(Status::Active).as_str());
    let at = query.valid_at.unwrap_or_else(Utc::now);
    builder
        .push(format!(" AND {table}.valid_from <= "))
        .push_bind(at)
        .push(format!(
            " AND ({table}.valid_to IS NULL OR {table}.valid_to > "
        ))
        .push_bind(at)
        .push(")");
    if let Some(kind) = &query.kind {
        builder
            .push(format!(" AND {table}.kind = "))
            .push_bind(kind.clone());
    }
    if let Some(entity) = query.entity {
        builder
            .push(" AND ")
            .push_bind(entity)
            .push(format!(" = ANY({table}.subjects)"));
    }
}

pub(crate) async fn search(pool: &PgPool, query: &SearchQuery) -> Result<Vec<SearchHit>> {
    let limit = query.limit.clamp(1, 100);
    let pool_size = limit * 3;
    let mut ranked: HashMap<(Uuid, i32), (f64, bool, bool)> = HashMap::new();

    if let Some(tsquery) = tsquery(&query.text) {
        let mut builder = QueryBuilder::<Postgres>::new(
            "SELECT v.assertion_id, v.version FROM assertion_versions v, to_tsquery('simple', ",
        );
        builder
            .push_bind(tsquery)
            .push(") q WHERE to_tsvector('simple', v.text) @@ q");
        push_filters(&mut builder, query, "v");
        builder
            .push(" ORDER BY ts_rank(to_tsvector('simple', v.text), q) DESC LIMIT ")
            .push_bind(pool_size);
        for (rank, row) in builder.build().fetch_all(pool).await?.iter().enumerate() {
            let key = (row.try_get("assertion_id")?, row.try_get("version")?);
            let entry = ranked.entry(key).or_insert((0.0, false, false));
            entry.0 += 1.0 / (RRF_K + rank as f64 + 1.0);
            entry.1 = true;
        }
    }

    if let Some((model, vector)) = &query.embedding {
        let literal = vector_literal(vector);
        let mut builder = QueryBuilder::<Postgres>::new(
            "SELECT v.assertion_id, v.version FROM assertion_embeddings e \
             JOIN assertion_versions v ON v.assertion_id = e.assertion_id AND v.version = e.version \
             WHERE e.model = ",
        );
        builder.push_bind(model.clone());
        push_filters(&mut builder, query, "v");
        builder
            .push(" ORDER BY e.embedding <=> CAST(")
            .push_bind(literal)
            .push(" AS vector) LIMIT ")
            .push_bind(pool_size);
        for (rank, row) in builder.build().fetch_all(pool).await?.iter().enumerate() {
            let key = (row.try_get("assertion_id")?, row.try_get("version")?);
            let entry = ranked.entry(key).or_insert((0.0, false, false));
            entry.0 += 1.0 / (RRF_K + rank as f64 + 1.0);
            entry.2 = true;
        }
    }

    let mut order: Vec<Ranked> = ranked.into_iter().collect();
    order.sort_by(|a, b| b.1.0.total_cmp(&a.1.0));
    order.truncate(limit as usize);
    if order.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<Uuid> = order.iter().map(|(k, _)| k.0).collect();
    let versions: Vec<i32> = order.iter().map(|(k, _)| k.1).collect();
    let rows = sqlx::query(concat!(
        "SELECT ",
        version_columns!(),
        " FROM assertion_versions WHERE (assertion_id, version) IN (SELECT * FROM unnest($1::uuid[], $2::int[]))"
    ))
    .bind(&ids)
    .bind(&versions)
    .fetch_all(pool)
    .await?;
    let mut found: HashMap<(Uuid, i32), AssertionVersion> = HashMap::new();
    for row in &rows {
        let version = rows::version(row)?;
        found.insert((version.assertion_id, version.version), version);
    }
    Ok(order
        .into_iter()
        .filter_map(|(key, (score, words, meaning))| {
            found.remove(&key).map(|version| SearchHit {
                version,
                score,
                matched: match (words, meaning) {
                    (true, true) => "both",
                    (true, false) => "words",
                    _ => "meaning",
                },
            })
        })
        .collect())
}

/// Current versions that have no embedding from `model` yet.
pub(crate) async fn missing_embeddings(
    pool: &PgPool,
    model: &str,
    limit: i64,
) -> Result<Vec<(Uuid, i32, String)>> {
    let rows = sqlx::query(
        "SELECT v.assertion_id, v.version, v.text FROM assertion_versions v \
         WHERE v.recorded_to IS NULL AND NOT EXISTS (SELECT 1 FROM assertion_embeddings e \
         WHERE e.assertion_id = v.assertion_id AND e.version = v.version AND e.model = $1) \
         ORDER BY v.recorded_from LIMIT $2",
    )
    .bind(model)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| {
            Ok((
                row.try_get("assertion_id")?,
                row.try_get("version")?,
                row.try_get("text")?,
            ))
        })
        .collect()
}

pub(crate) async fn store_embeddings(
    pool: &PgPool,
    model: &str,
    items: &[((Uuid, i32), Vec<f32>)],
) -> Result<()> {
    let mut tx = pool.begin().await?;
    for ((assertion_id, version), vector) in items {
        sqlx::query(
            "INSERT INTO assertion_embeddings (assertion_id, version, model, embedding) \
             VALUES ($1, $2, $3, CAST($4 AS vector)) ON CONFLICT DO NOTHING",
        )
        .bind(assertion_id)
        .bind(version)
        .bind(model)
        .bind(vector_literal(vector))
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Entities named in a text: exact names and aliases inside it, then similar names.
pub(crate) async fn entities_in(
    pool: &PgPool,
    text: &str,
    limit: i64,
) -> Result<Vec<crate::model::Entity>> {
    let rows = sqlx::query(
        "SELECT id, type, name, aliases, merged_into, created_by, created_at FROM entities e \
         WHERE e.merged_into IS NULL AND (position(lower(e.name) IN lower($1)) > 0 \
            OR EXISTS (SELECT 1 FROM unnest(e.aliases) a WHERE length(a) > 1 AND position(lower(a) IN lower($1)) > 0) \
            OR word_similarity(lower(e.name), lower($1)) > 0.6) \
         ORDER BY word_similarity(lower(e.name), lower($1)) DESC LIMIT $2",
    )
    .bind(text)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    rows.iter().map(rows::entity).collect()
}

/// Proposals that still wait for a verdict and whose texts share words with `text`, newest
/// first: what an agent should not propose again.
pub(crate) async fn pending_proposals(
    pool: &PgPool,
    text: &str,
    limit: i64,
) -> sqlx::Result<Vec<uuid::Uuid>> {
    let Some(query) = tsquery(text) else {
        return Ok(Vec::new());
    };
    sqlx::query_scalar(
        "SELECT id FROM proposals WHERE state IN ('open', 'deferred') \
           AND to_tsvector('simple', alternatives::text || ' ' || coalesce(rationale, '')) @@ to_tsquery('simple', $1) \
         ORDER BY created_at DESC LIMIT $2",
    )
    .bind(query)
    .bind(limit)
    .fetch_all(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_words_become_prefix_terms() {
        assert_eq!(
            tsquery("Релиз 2.4 — 14 ноября, deadlines!").as_deref(),
            Some("релиз:* | 14 | нояб:* | deadlin:*")
        );
        assert_eq!(tsquery("— ! ?"), None);
    }

    #[test]
    fn vectors_print_as_pgvector_literals() {
        assert_eq!(vector_literal(&[0.5, -1.0]), "[0.5,-1]");
    }
}
