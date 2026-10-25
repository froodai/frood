//! Attachments (docs/26 §2): photos, videos, documents and links kept with knowledge. Each is a
//! source of its own, attached to an assertion across its versions, or to a proposal while it
//! waits; accepting the proposal carries them to the knowledge it made or changed.

use serde_json::json;
use sqlx::{PgConnection, Row};
use uuid::Uuid;

use crate::error::{LedgerError, Result, invalid};
use crate::journal::{self, Event};
use crate::model::{Actor, AttachTo, Attachment, BlobInfo, ProposalState, Timestamp};
use crate::{Ledger, query};

/// An attachment with what its source is.
macro_rules! select {
    () => {
        "SELECT a.id, a.assertion_id, a.proposal_id, a.source_id, a.added_by, a.added_at, \
         s.kind, s.uri, s.title, s.content, b.id AS blob_id, b.content_type, b.filename, b.size_bytes \
         FROM attachments a JOIN sources s ON s.id = a.source_id LEFT JOIN blobs b ON b.id = s.blob_id"
    };
}

fn row(row: &sqlx::postgres::PgRow) -> Result<Attachment> {
    let blob_id: Option<Uuid> = row.try_get("blob_id")?;
    let file = match blob_id {
        Some(id) => Some(BlobInfo {
            id,
            content_type: row.try_get("content_type")?,
            filename: row.try_get("filename")?,
            size_bytes: row.try_get("size_bytes")?,
        }),
        None => None,
    };
    Ok(Attachment {
        id: row.try_get("id")?,
        assertion_id: row.try_get("assertion_id")?,
        proposal_id: row.try_get("proposal_id")?,
        source_id: row.try_get("source_id")?,
        kind: row.try_get("kind")?,
        uri: row.try_get("uri")?,
        title: row.try_get("title")?,
        text: row.try_get("content")?,
        file,
        added_by: row.try_get("added_by")?,
        added_at: row.try_get("added_at")?,
    })
}

impl Ledger {
    /// Attaches a captured source to an assertion or to a proposal. A proposal that was
    /// accepted passes it on to the knowledge it made or changed.
    pub async fn attach(
        &self,
        actor: &Actor,
        to: AttachTo,
        source_id: Uuid,
    ) -> Result<Vec<Attachment>> {
        let source = query::source(&self.pool, source_id).await?;
        if source.withdrawn_at.is_some() {
            return Err(invalid("the source was taken back"));
        }
        let mut tx = self.pool.begin().await?;
        let targets = match to {
            AttachTo::Assertion(id) => {
                let exists: bool =
                    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM assertions WHERE id = $1)")
                        .bind(id)
                        .fetch_one(&mut *tx)
                        .await?;
                if !exists {
                    return Err(LedgerError::NotFound(format!("assertion {id}")));
                }
                vec![AttachTo::Assertion(id)]
            }
            AttachTo::Proposal(id) => {
                let proposal = query::proposal(&mut tx, id).await?;
                match proposal.state {
                    ProposalState::Open | ProposalState::Deferred => vec![AttachTo::Proposal(id)],
                    ProposalState::Accepted => {
                        let made = recorded_by(&mut tx, id).await?;
                        if made.is_empty() {
                            return Err(invalid(
                                "the accepted alternative changed no knowledge to attach to",
                            ));
                        }
                        made.into_iter().map(AttachTo::Assertion).collect()
                    }
                    state => {
                        return Err(invalid(format!(
                            "the proposal is {state}: there is no knowledge to attach to"
                        )));
                    }
                }
            }
        };
        let at: Timestamp = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut *tx)
            .await?;
        let mut events = Vec::new();
        for &target in &targets {
            if let Some(id) = insert(&mut tx, target, source_id, actor.as_str(), at).await? {
                events.push(added(id, target, source_id));
            }
        }
        journal::append(&mut tx, actor.as_str(), &events).await?;
        tx.commit().await?;
        // New or already there: the source on each target.
        let on: Vec<Uuid> = targets.iter().map(|t| t.id()).collect();
        let rows = sqlx::query(concat!(
            select!(),
            " WHERE a.source_id = $1 AND a.removed_at IS NULL \
             AND coalesce(a.assertion_id, a.proposal_id) = ANY($2) ORDER BY a.added_at, a.id"
        ))
        .bind(source_id)
        .bind(&on)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row).collect()
    }

    /// What is attached to an assertion or a proposal, the oldest first; sources taken back
    /// are left out.
    pub async fn attachments(&self, of: AttachTo) -> Result<Vec<Attachment>> {
        let rows = sqlx::query(concat!(
            select!(),
            " WHERE coalesce(a.assertion_id, a.proposal_id) = $1 AND (a.assertion_id IS NOT NULL) = $2 \
             AND a.removed_at IS NULL AND s.withdrawn_at IS NULL ORDER BY a.added_at, a.id"
        ))
        .bind(of.id())
        .bind(matches!(of, AttachTo::Assertion(_)))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row).collect()
    }

    /// Takes an attachment off; the source stays.
    pub async fn detach(&self, actor: &Actor, id: Uuid) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let found = sqlx::query(
            "UPDATE attachments SET removed_at = clock_timestamp() WHERE id = $1 AND removed_at IS NULL \
             RETURNING assertion_id, proposal_id, source_id",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| LedgerError::NotFound(format!("attachment {id}")))?;
        let assertion: Option<Uuid> = found.try_get("assertion_id")?;
        let subject = match assertion {
            Some(a) => format!("assertion/{a}"),
            None => format!("proposal/{}", found.try_get::<Uuid, _>("proposal_id")?),
        };
        journal::append(
            &mut tx,
            actor.as_str(),
            &[Event::new(
                "attachment.removed",
                subject,
                json!({ "attachment_id": id, "source_id": found.try_get::<Uuid, _>("source_id")? }),
            )],
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }
}

/// Assertions a proposal recorded versions of.
async fn recorded_by(conn: &mut PgConnection, proposal_id: Uuid) -> Result<Vec<Uuid>> {
    Ok(sqlx::query_scalar(
        "SELECT DISTINCT assertion_id FROM assertion_versions WHERE proposal_id = $1",
    )
    .bind(proposal_id)
    .fetch_all(&mut *conn)
    .await?)
}

async fn insert(
    conn: &mut PgConnection,
    to: AttachTo,
    source_id: Uuid,
    actor: &str,
    at: Timestamp,
) -> Result<Option<Uuid>> {
    let (assertion, proposal) = match to {
        AttachTo::Assertion(id) => (Some(id), None),
        AttachTo::Proposal(id) => (None, Some(id)),
    };
    let id = Uuid::now_v7();
    let done = sqlx::query(
        "INSERT INTO attachments (id, assertion_id, proposal_id, source_id, added_by, added_at) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT ((coalesce(assertion_id, proposal_id)), source_id) WHERE removed_at IS NULL DO NOTHING",
    )
    .bind(id)
    .bind(assertion)
    .bind(proposal)
    .bind(source_id)
    .bind(actor)
    .bind(at)
    .execute(&mut *conn)
    .await?;
    Ok((done.rows_affected() == 1).then_some(id))
}

fn added(id: Uuid, to: AttachTo, source_id: Uuid) -> Event {
    let subject = match to {
        AttachTo::Assertion(a) => format!("assertion/{a}"),
        AttachTo::Proposal(p) => format!("proposal/{p}"),
    };
    Event::new(
        "attachment.added",
        subject,
        json!({ "attachment_id": id, "source_id": source_id }),
    )
}

/// When a proposal is accepted: what waited with it goes to the knowledge it made or changed.
pub(crate) async fn carry(
    conn: &mut PgConnection,
    proposal_id: Uuid,
    at: Timestamp,
) -> Result<Vec<Event>> {
    let sources: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT source_id, added_by FROM attachments WHERE proposal_id = $1 AND removed_at IS NULL \
         ORDER BY added_at",
    )
    .bind(proposal_id)
    .fetch_all(&mut *conn)
    .await?;
    if sources.is_empty() {
        return Ok(Vec::new());
    }
    let mut events = Vec::new();
    for assertion in recorded_by(conn, proposal_id).await? {
        for (source_id, added_by) in &sources {
            let to = AttachTo::Assertion(assertion);
            if let Some(id) = insert(conn, to, *source_id, added_by, at).await? {
                events.push(added(id, to, *source_id));
            }
        }
    }
    Ok(events)
}
