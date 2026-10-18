//! Mapping database rows to ledger types.

use sqlx::Row;
use sqlx::postgres::PgRow;

use crate::error::{Result, parse_text};
use crate::model::{AssertionVersion, Entity, Evidence, Observation, Proposal, Verdict};

macro_rules! version_columns {
    () => {
        "assertion_id, version, kind, text, subjects, slots, modality, authority, said_at, \
         valid_from, valid_to, recorded_from, recorded_to, status, review_after, evidence, \
         proposal_id, verdict_id"
    };
}
pub(crate) use version_columns;

pub(crate) fn version(row: &PgRow) -> Result<AssertionVersion> {
    Ok(AssertionVersion {
        assertion_id: row.try_get("assertion_id")?,
        version: row.try_get("version")?,
        kind: row.try_get("kind")?,
        text: row.try_get("text")?,
        subjects: row.try_get("subjects")?,
        slots: row.try_get("slots")?,
        modality: parse_text(row.try_get::<&str, _>("modality")?)?,
        authority: row.try_get("authority")?,
        said_at: row.try_get("said_at")?,
        valid_from: row.try_get("valid_from")?,
        valid_to: row.try_get("valid_to")?,
        recorded_from: row.try_get("recorded_from")?,
        recorded_to: row.try_get("recorded_to")?,
        status: parse_text(row.try_get::<&str, _>("status")?)?,
        review_after: row.try_get("review_after")?,
        evidence: row.try_get("evidence")?,
        proposal_id: row.try_get("proposal_id")?,
        verdict_id: row.try_get("verdict_id")?,
    })
}

pub(crate) fn evidence(row: &PgRow) -> Result<Evidence> {
    Ok(Evidence {
        id: row.try_get("id")?,
        source_id: row.try_get("source_id")?,
        anchor: row.try_get("anchor")?,
        quote: row.try_get("quote")?,
        created_at: row.try_get("created_at")?,
    })
}

pub(crate) fn observation(row: &PgRow) -> Result<Observation> {
    Ok(Observation {
        id: row.try_get("id")?,
        source_id: row.try_get("source_id")?,
        evidence_id: row.try_get("evidence_id")?,
        text: row.try_get("text")?,
        modality: parse_text(row.try_get::<&str, _>("modality")?)?,
        said_at: row.try_get("said_at")?,
        extracted_by: row.try_get("extracted_by")?,
        confidence: row.try_get("confidence")?,
        flags: row.try_get("flags")?,
        created_at: row.try_get("created_at")?,
    })
}

pub(crate) fn entity(row: &PgRow) -> Result<Entity> {
    Ok(Entity {
        id: row.try_get("id")?,
        entity_type: row.try_get("type")?,
        name: row.try_get("name")?,
        aliases: row.try_get("aliases")?,
        merged_into: row.try_get("merged_into")?,
        created_by: row.try_get("created_by")?,
        created_at: row.try_get("created_at")?,
    })
}

pub(crate) fn verdict(row: &PgRow) -> Result<Verdict> {
    Ok(Verdict {
        id: row.try_get("id")?,
        proposal_id: row.try_get("proposal_id")?,
        decision: parse_text(row.try_get::<&str, _>("decision")?)?,
        alternative: row.try_get("alternative")?,
        edits: row.try_get("edits")?,
        reason: row.try_get("reason")?,
        decided_by: row.try_get("decided_by")?,
        created_at: row.try_get("created_at")?,
    })
}

/// A proposal without its verdicts; callers attach them.
pub(crate) fn proposal(row: &PgRow) -> Result<Proposal> {
    let base: serde_json::Value = row.try_get("base")?;
    let alternatives: serde_json::Value = row.try_get("alternatives")?;
    Ok(Proposal {
        id: row.try_get("id")?,
        idempotency_key: row.try_get("idempotency_key")?,
        author: row.try_get("author")?,
        rationale: row.try_get("rationale")?,
        risk: parse_text(row.try_get::<&str, _>("risk")?)?,
        confidence: row.try_get("confidence")?,
        base: serde_json::from_value(base).map_err(|e| sqlx::Error::Decode(e.into()))?,
        alternatives: serde_json::from_value(alternatives)
            .map_err(|e| sqlx::Error::Decode(e.into()))?,
        evidence: row.try_get("evidence")?,
        state: parse_text(row.try_get::<&str, _>("state")?)?,
        deferred_until: row.try_get("deferred_until")?,
        created_at: row.try_get("created_at")?,
        decided_at: row.try_get("decided_at")?,
        verdicts: Vec::new(),
    })
}
