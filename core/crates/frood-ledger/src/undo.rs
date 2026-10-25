//! Undo (docs/25 §3.2: every action can be taken back). The last verdict on a proposal is
//! undone by a verdict of its own: a rejected or deferred proposal opens again; an accepted one
//! puts back the versions it replaced and opens again, its base moved to the restored versions.
//! The versions the undo writes carry the undo verdict, so history keeps both decisions.

use std::collections::BTreeMap;

use serde_json::json;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::error::{LedgerError, Result, invalid, parse_text};
use crate::journal::{self, Event};
use crate::model::{
    Actor, AssertionVersion, BaseRef, Decision, Proposal, ProposalState, Risk, Timestamp,
    VerdictInput,
};
use crate::rows::{self, version_columns};
use crate::{Ledger, apply, query};

impl Ledger {
    pub async fn undo(&self, actor: &Actor, proposal_id: Uuid) -> Result<Proposal> {
        let mut tx = self.pool.begin().await?;
        let locked: Option<String> =
            sqlx::query_scalar("SELECT state FROM proposals WHERE id = $1 FOR UPDATE")
                .bind(proposal_id)
                .fetch_optional(&mut *tx)
                .await?;
        let state: ProposalState = match locked {
            Some(state) => parse_text(&state)?,
            None => return Err(LedgerError::NotFound(format!("proposal {proposal_id}"))),
        };
        let proposal = query::proposal(&mut tx, proposal_id).await?;
        let Some(last) = proposal
            .verdicts
            .iter()
            .rev()
            .find(|v| v.decision != Decision::Investigate)
            .filter(|v| v.decision != Decision::Undo)
        else {
            return Err(invalid("the proposal has no verdict to undo"));
        };
        if !matches!(
            state,
            ProposalState::Accepted | ProposalState::Rejected | ProposalState::Deferred
        ) {
            return Err(invalid(format!(
                "nothing to undo: the proposal is {}",
                state.as_str()
            )));
        }

        let at: Timestamp = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut *tx)
            .await?;
        let undo_id = Uuid::now_v7();
        // First: the versions the undo writes refer to it.
        let input = VerdictInput {
            decision: Decision::Undo,
            alternative: None,
            operations: None,
            reason: Some(format!("undoes verdict {}", last.id)),
            deferred_until: None,
        };
        self.insert_verdict(&mut tx, undo_id, proposal_id, &input, None, None, actor, at)
            .await?;
        let mut events = Vec::new();
        let mut base = proposal.base.clone();
        if state == ProposalState::Accepted {
            let written = written_by(&mut tx, last.id).await?;
            for (assertion_id, rows) in written {
                restore(
                    &mut tx,
                    assertion_id,
                    &rows,
                    at,
                    proposal_id,
                    undo_id,
                    &mut events,
                )
                .await?;
                if let Some(current) = apply::current_version(&mut tx, assertion_id).await? {
                    for b in base.iter_mut().filter(|b| b.assertion_id == assertion_id) {
                        *b = BaseRef {
                            assertion_id,
                            version: current.version,
                        };
                    }
                }
            }
        }

        sqlx::query(
            "UPDATE proposals SET state = 'open', deferred_until = NULL, decided_at = NULL, base = $2 WHERE id = $1",
        )
        .bind(proposal_id)
        .bind(serde_json::to_value(&base).expect("base serializes"))
        .execute(&mut *tx)
        .await?;
        if proposal.risk == Risk::Conflict {
            for b in &base {
                sqlx::query(
                    "INSERT INTO assertion_flags (assertion_id, flag, reason) VALUES ($1, 'contested', $2) \
                     ON CONFLICT (assertion_id, flag) DO NOTHING",
                )
                .bind(b.assertion_id)
                .bind(format!("proposal {proposal_id}"))
                .execute(&mut *tx)
                .await?;
            }
        }
        events.insert(
            0,
            Event::new(
                "proposal.reopened",
                format!("proposal/{proposal_id}"),
                json!({ "proposal_id": proposal_id, "verdict_id": undo_id, "undoes": last.id, "was": state.as_str() }),
            ),
        );
        journal::append(&mut tx, actor.as_str(), &events).await?;
        tx.commit().await?;
        self.proposal(proposal_id).await
    }
}

/// The rows a verdict wrote, per assertion.
async fn written_by(
    conn: &mut PgConnection,
    verdict_id: Uuid,
) -> Result<BTreeMap<Uuid, Vec<AssertionVersion>>> {
    let rows = sqlx::query(concat!(
        "SELECT ",
        version_columns!(),
        " FROM assertion_versions WHERE verdict_id = $1 ORDER BY assertion_id, version"
    ))
    .bind(verdict_id)
    .fetch_all(&mut *conn)
    .await?;
    let mut written: BTreeMap<Uuid, Vec<AssertionVersion>> = BTreeMap::new();
    for row in &rows {
        let version = rows::version(row)?;
        written
            .entry(version.assertion_id)
            .or_default()
            .push(version);
    }
    Ok(written)
}

/// Takes the verdict's rows out of current knowledge and writes again the rows they replaced.
async fn restore(
    conn: &mut PgConnection,
    assertion_id: Uuid,
    rows: &[AssertionVersion],
    at: Timestamp,
    proposal_id: Uuid,
    undo_id: Uuid,
    events: &mut Vec<Event>,
) -> Result<()> {
    sqlx::query("SELECT id FROM assertions WHERE id = $1 FOR UPDATE")
        .bind(assertion_id)
        .execute(&mut *conn)
        .await?;
    // Something changed it since: undoing would throw that away.
    if let Some(changed) = rows.iter().find(|r| r.recorded_to.is_some()) {
        let actual: i32 = sqlx::query_scalar(
            "SELECT max(version) FROM assertion_versions WHERE assertion_id = $1",
        )
        .bind(assertion_id)
        .fetch_one(&mut *conn)
        .await?;
        return Err(LedgerError::Stale {
            proposal: Some(proposal_id),
            assertion: assertion_id,
            expected: changed.version,
            actual,
        });
    }
    let applied_at = rows[0].recorded_from;
    let verdict_id = rows[0].verdict_id;
    let replaced = sqlx::query(concat!(
        "SELECT ",
        version_columns!(),
        " FROM assertion_versions WHERE assertion_id = $1 AND recorded_to = $2 AND verdict_id <> $3 ORDER BY version"
    ))
    .bind(assertion_id)
    .bind(applied_at)
    .bind(verdict_id)
    .fetch_all(&mut *conn)
    .await?
    .iter()
    .map(rows::version)
    .collect::<Result<Vec<_>>>()?;

    sqlx::query("UPDATE assertion_versions SET recorded_to = $3 WHERE assertion_id = $1 AND verdict_id = $2 AND recorded_to IS NULL")
        .bind(assertion_id)
        .bind(verdict_id)
        .bind(at)
        .execute(&mut *conn)
        .await?;
    let from_version = rows.iter().map(|r| r.version).max().unwrap_or_default();
    if replaced.is_empty() {
        // The verdict created it: it leaves current knowledge.
        events.push(Event::new(
            "assertion.closed",
            format!("assertion/{assertion_id}"),
            json!({ "assertion_id": assertion_id, "version": from_version, "status": "retired", "undone": true }),
        ));
        return Ok(());
    }
    let mut next = from_version + 1;
    for row in &replaced {
        sqlx::query(
            "INSERT INTO assertion_versions (assertion_id, version, kind, title, sphere, text, subjects, slots, \
             modality, authority, said_at, valid_from, valid_to, recorded_from, status, review_after, evidence, \
             proposal_id, verdict_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19)",
        )
        .bind(assertion_id)
        .bind(next)
        .bind(&row.kind)
        .bind(&row.title)
        .bind(&row.sphere)
        .bind(&row.text)
        .bind(&row.subjects)
        .bind(&row.slots)
        .bind(row.modality.as_str())
        .bind(&row.authority)
        .bind(row.said_at)
        .bind(row.valid_from)
        .bind(row.valid_to)
        .bind(at)
        .bind(row.status.as_str())
        .bind(row.review_after)
        .bind(&row.evidence)
        .bind(proposal_id)
        .bind(undo_id)
        .execute(&mut *conn)
        .await?;
        next += 1;
    }
    events.push(Event::new(
        "assertion.superseded",
        format!("assertion/{assertion_id}"),
        json!({ "assertion_id": assertion_id, "from_version": from_version, "to_version": next - 1, "undone": true }),
    ));
    Ok(())
}
