//! "What is new since last time" (docs/15 §7, B3): per person, since their own checkpoint, with
//! no model call — what was accepted, what waits for a decision, what is due soon, which
//! background runs failed. The `digest` job puts it into each person's Inbox; anyone can ask for
//! it at any time.

use chrono::{DateTime, Duration, Utc};
use frood_ledger::model::{AssertionFilter, Operation, ProposalState, SlotFilter, SlotOp, Status};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::inbox::{self, NewItem};
use crate::jobs::JobError;
use crate::runs::Run;
use crate::{Dispatch, runs};

const LIST: usize = 10;

impl Dispatch {
    /// The digest of one person since a moment (their checkpoint when `since` is None).
    pub async fn digest_for(
        &self,
        user: Uuid,
        since: Option<DateTime<Utc>>,
    ) -> sqlx::Result<Value> {
        let pool = &self.ctx.pool;
        let now = Utc::now();
        let since = match since {
            Some(since) => since,
            None => sqlx::query_scalar::<_, DateTime<Utc>>(
                "SELECT seen_until FROM attention_checkpoints WHERE user_id = $1",
            )
            .bind(user)
            .fetch_optional(pool)
            .await?
            .unwrap_or(now - Duration::days(1)),
        };

        let mut accepted = Vec::new();
        let proposals = self
            .ctx
            .ledger
            .proposals(Some(ProposalState::Accepted), None, 200)
            .await
            .unwrap_or_default();
        for proposal in proposals
            .iter()
            .filter(|p| p.decided_at.is_some_and(|d| d > since))
        {
            for alternative in &proposal.alternatives {
                for op in &alternative.operations {
                    if let Operation::Create { assertion, .. }
                    | Operation::Supersede { assertion, .. } = op
                    {
                        accepted.push(assertion.text.clone());
                    }
                }
            }
        }

        let mut waiting = Vec::new();
        let open = self.inbox(false, 100).await?;
        let open: Vec<_> = open.into_iter().filter(|i| i.kind != "digest").collect();
        for item in open.iter().take(LIST) {
            let shown = self.inbox_shown(item.clone()).await?;
            let what = match item.kind.as_str() {
                "proposal" => shown.content["alternatives"][0]["changes"][0]["after"]
                    .as_str()
                    .or_else(|| shown.content["alternatives"][0]["changes"][0]["before"].as_str())
                    .unwrap_or("?")
                    .to_owned(),
                "question" => shown.content["text"].as_str().unwrap_or("?").to_owned(),
                "reminder" => item.data["text"].as_str().unwrap_or("?").to_owned(),
                "alert" => item.data["job"].as_str().unwrap_or("?").to_owned(),
                _ => continue,
            };
            waiting.push(json!({ "id": item.id, "kind": item.kind, "what": what }));
        }

        let today = now.date_naive();
        let due_soon = self
            .ctx
            .ledger
            .assertions(&AssertionFilter {
                kind: Some("commitment".into()),
                status: Some(Status::Active),
                slots: vec![
                    SlotFilter {
                        key: "due".into(),
                        op: SlotOp::Gte,
                        value: today.to_string(),
                    },
                    SlotFilter {
                        key: "due".into(),
                        op: SlotOp::Lt,
                        value: (today + Duration::days(8)).to_string(),
                    },
                ],
                limit: Some(LIST as i64),
                ..Default::default()
            })
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|a| json!({ "text": a.text, "due": a.slots["due"] }))
            .collect::<Vec<_>>();

        let failed: Vec<Run> = runs::list(pool, None, Some("failed"), 50)
            .await?
            .into_iter()
            .filter(|r| r.finished_at.is_some_and(|f| f > since))
            .collect();
        let failed: Vec<Value> = failed
            .iter()
            .take(LIST)
            .map(|r| json!({ "job": r.job, "error": r.error }))
            .collect();

        Ok(json!({
            "since": since,
            "until": now,
            "accepted": accepted.into_iter().take(LIST).collect::<Vec<_>>(),
            "waiting": { "count": open.len(), "items": waiting },
            "due_soon": due_soon,
            "failed_runs": failed,
        }))
    }

    /// The `digest` job: puts each person's digest into their Inbox and moves their checkpoint.
    pub(crate) async fn run_digest(&self, run: &Run) -> Result<Value, JobError> {
        let pool = &self.ctx.pool;
        let people: Vec<Uuid> = sqlx::query_scalar("SELECT DISTINCT user_id FROM channel_links")
            .fetch_all(pool)
            .await?;
        let mut made = 0;
        for user in people {
            let digest = self.digest_for(user, None).await?;
            let empty = digest["accepted"].as_array().is_none_or(Vec::is_empty)
                && digest["waiting"]["count"] == 0
                && digest["due_soon"].as_array().is_none_or(Vec::is_empty)
                && digest["failed_runs"].as_array().is_none_or(Vec::is_empty);
            sqlx::query(
                "INSERT INTO attention_checkpoints (user_id, seen_until) VALUES ($1, $2) \
                 ON CONFLICT (user_id) DO UPDATE SET seen_until = excluded.seen_until",
            )
            .bind(user)
            .bind(Utc::now())
            .execute(pool)
            .await?;
            if empty {
                continue; // nothing new: no message
            }
            inbox::insert(
                pool,
                NewItem {
                    kind: "digest",
                    subject: format!("digest/{user}/{}", run.id),
                    urgency: "now".to_owned(),
                    data: digest,
                    for_users: vec![user],
                    notify_after: Utc::now(),
                    investigation: None,
                },
            )
            .await?;
            made += 1;
        }
        Ok(json!({ "digests": made }))
    }
}
