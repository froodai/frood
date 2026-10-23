//! Taking a note back (docs/25 §4: "Записал" comes with "Отменить"). The note stays in history,
//! marked withdrawn; proposals made from it are closed, and the ones already accepted are
//! undone first, unless someone changed that knowledge since. The same note sent again
//! comes back to life.

use serde_json::json;
use uuid::Uuid;

use crate::error::{LedgerError, Result};
use crate::journal::{self, Event};
use crate::model::{
    Actor, CaptureResult, Decision, ProposalState, Source, VerdictInput, Withdrawn, is_media_type,
};
use crate::{Ledger, query};

const REASON: &str = "the note it rests on was taken back";

impl Ledger {
    pub async fn withdraw_source(&self, actor: &Actor, source_id: Uuid) -> Result<Withdrawn> {
        let source = query::source(&self.pool, source_id).await?;
        let mut done = Withdrawn {
            source_id,
            closed: 0,
            kept: 0,
        };
        if source.withdrawn_at.is_some() {
            return Ok(done);
        }
        let evidence: Vec<Uuid> = source.evidence.iter().map(|e| e.id).collect();
        // Proposals cite the evidence directly, or through the assertions they would write.
        let proposals: Vec<(Uuid, String)> = sqlx::query_as(
            "SELECT id, state FROM proposals WHERE evidence && $1 \
             OR EXISTS (SELECT 1 FROM unnest($1::uuid[]) e WHERE alternatives::text LIKE '%' || e::text || '%') \
             ORDER BY created_at",
        )
        .bind(&evidence)
        .fetch_all(&self.pool)
        .await?;
        let reject = || VerdictInput {
            decision: Decision::Reject,
            alternative: None,
            operations: None,
            reason: Some(REASON.to_owned()),
            deferred_until: None,
        };
        for (id, state) in proposals {
            match state.parse::<ProposalState>() {
                Ok(ProposalState::Accepted) => match self.undo(actor, id).await {
                    Ok(_) => {
                        self.submit_verdict(actor, id, reject()).await?;
                        done.closed += 1;
                    }
                    Err(LedgerError::Stale { .. }) => done.kept += 1,
                    Err(err) => return Err(err),
                },
                Ok(ProposalState::Open | ProposalState::Deferred) => {
                    self.submit_verdict(actor, id, reject()).await?;
                    done.closed += 1;
                }
                _ => {}
            }
        }
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "UPDATE sources SET withdrawn_at = clock_timestamp(), withdrawn_by = $2 WHERE id = $1",
        )
        .bind(source_id)
        .bind(actor.as_str())
        .execute(&mut *tx)
        .await?;
        journal::append(
            &mut tx,
            actor.as_str(),
            &[Event::new(
                "source.withdrawn",
                format!("source/{source_id}"),
                json!({ "source_id": source_id, "closed": done.closed, "kept": done.kept }),
            )],
        )
        .await?;
        tx.commit().await?;
        Ok(done)
    }

    /// The same note again after it was taken back: it counts as new, and is processed again.
    pub(crate) async fn revive(&self, actor: &Actor, source: Source) -> Result<CaptureResult> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE sources SET withdrawn_at = NULL, withdrawn_by = NULL WHERE id = $1")
            .bind(source.id)
            .execute(&mut *tx)
            .await?;
        let content_type = source.file.as_ref().map(|f| f.content_type.as_str());
        journal::append(
            &mut tx,
            actor.as_str(),
            &[Event::new(
                "source.captured",
                format!("source/{}", source.id),
                json!({
                    "source_id": source.id,
                    "kind": source.kind,
                    "origin": source.origin,
                    "captured_by": actor.as_str(),
                    "has_text": source.content.is_some(),
                    "has_media": content_type.is_some_and(is_media_type),
                    "content_type": content_type,
                }),
            )],
        )
        .await?;
        tx.commit().await?;
        Ok(CaptureResult {
            source: query::source(&self.pool, source.id).await?,
            created: true,
        })
    }
}
