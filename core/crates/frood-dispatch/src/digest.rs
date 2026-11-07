//! "What is new since last time" (docs/15 §7, B3; redone in docs/31): per person, since their
//! own checkpoint. Only what is new, and what is important or urgent whatever its age — a
//! conflict, an agent's question, a due date today, tomorrow or past, a failed run; what has
//! waited longer comes as one line. Findings about one issue or page are one line, with its
//! link. The `digest` job puts it into each person's Inbox when there is something to say;
//! an agent job, when there is one (`write-digest`), says it in plain words first.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
use frood_ledger::model::{AssertionFilter, Operation, ProposalState, SlotFilter, SlotOp, Status};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::inbox::{self, NewItem};
use crate::jobs::JobError;
use crate::runs::Run;
use crate::{Dispatch, runs};

const LIST: usize = 10;
/// Headlines are read on a phone.
const TITLE: usize = 80;
/// The agent job that writes the digest in plain words, when the installation has it.
pub(crate) const WRITER: &str = "write-digest";
/// How long a digest waits for its writer before it goes as it is.
const WRITER_WAIT_MINUTES: i64 = 10;

/// An issue key such as AIRC-1808: capital letters or digits, a dash, digits.
fn issue_key(text: &str) -> Option<&str> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .find(|word| {
            let Some((project, number)) = word.split_once('-') else {
                return false;
            };
            project.len() >= 2
                && project.starts_with(|c: char| c.is_ascii_uppercase())
                && project
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
                && !number.is_empty()
                && number.chars().all(|c| c.is_ascii_digit())
        })
}

/// A headline: at most [`TITLE`] characters, cut at a word, without the issue key it starts
/// with when the key is its label.
pub(crate) fn headline(text: &str, label: Option<&str>) -> String {
    let mut text = text.trim();
    if let Some(key) = label
        && let Some(rest) = text.strip_prefix(key)
    {
        let rest = rest.trim_start();
        // "AIRC-1808 (Bug, Chat): …" → "…"
        let rest = match rest.strip_prefix('(').and_then(|r| r.split_once(')')) {
            Some((_, after)) => after,
            None => rest,
        };
        text = rest.trim_start_matches([':', '—', '-', ',', ' ']).trim();
    }
    if text.chars().count() <= TITLE {
        return text.to_owned();
    }
    let cut: String = text.chars().take(TITLE).collect();
    let cut = match cut.rfind(' ') {
        Some(space) if space > TITLE / 2 => &cut[..space],
        _ => cut.as_str(),
    };
    format!("{}…", cut.trim_end_matches([',', ';', ':', ' ']))
}

/// One line of the digest: findings about the same issue or page together.
#[derive(Default)]
struct Line {
    ids: Vec<Value>,
    kind: String,
    title: String,
    label: Option<String>,
    link: Option<String>,
    sphere: Value,
}

fn grouped(lines: Vec<Line>) -> Vec<Value> {
    let mut order: Vec<String> = Vec::new();
    let mut groups: BTreeMap<String, Line> = BTreeMap::new();
    for (i, line) in lines.into_iter().enumerate() {
        let key = line
            .link
            .clone()
            .or_else(|| line.label.clone())
            .unwrap_or_else(|| format!("#{i}"));
        match groups.get_mut(&key) {
            Some(group) => group.ids.extend(line.ids),
            None => {
                order.push(key.clone());
                groups.insert(key, line);
            }
        }
    }
    order
        .into_iter()
        .filter_map(|key| groups.remove(&key))
        .take(LIST)
        .map(|g| {
            json!({
                "ids": g.ids, "count": g.ids.len(), "kind": g.kind, "title": g.title,
                "label": g.label, "link": g.link, "sphere": g.sphere,
            })
        })
        .collect()
}

/// Where a proposal's knowledge comes from, as a link and a short label.
fn origin_of(content: &Value, text: &str) -> (Option<String>, Option<String>) {
    let link = content["origin"]["uri"]
        .as_str()
        .filter(|u| u.starts_with("http://") || u.starts_with("https://"))
        .map(str::to_owned);
    let label = issue_key(text)
        .or_else(|| content["origin"]["title"].as_str().and_then(issue_key))
        .or_else(|| link.as_deref().and_then(issue_key))
        .map(str::to_owned);
    (link, label)
}

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

        // Accepted since: what is known now that was not.
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
            let content = self.proposal_content(proposal).await;
            for alternative in &proposal.alternatives {
                for op in &alternative.operations {
                    if let Operation::Create { assertion, .. }
                    | Operation::Supersede { assertion, .. } = op
                    {
                        let text = assertion.title.as_deref().unwrap_or(&assertion.text);
                        let (link, label) = origin_of(&content, text);
                        accepted.push(Line {
                            ids: vec![json!(proposal.id)],
                            kind: assertion.kind.clone(),
                            title: headline(text, label.as_deref()),
                            label,
                            link,
                            sphere: json!(assertion.sphere),
                        });
                    }
                }
            }
        }

        // What waits for a decision: the important whatever its age, the new, and the rest
        // as a count.
        let mut important = Vec::new();
        let mut fresh = Vec::new();
        let mut older_count = 0;
        let mut older_since: Option<DateTime<Utc>> = None;
        let mut accept_ids = Vec::new();
        let open = self.inbox(false, 200).await?;
        let open: Vec<_> = open
            .into_iter()
            .filter(|i| i.kind != "digest" && i.for_users.iter().all(|u| *u == user))
            .collect();
        for item in &open {
            let shown = self.inbox_shown(item.clone()).await?;
            let content = &shown.content;
            let change = &content["alternatives"][0]["changes"][0];
            let text = match item.kind.as_str() {
                "proposal" => change["title"]
                    .as_str()
                    .or_else(|| change["after"].as_str())
                    .or_else(|| change["before"].as_str())
                    .unwrap_or("?"),
                "question" => content["text"].as_str().unwrap_or("?"),
                "reminder" => item.data["text"].as_str().unwrap_or("?"),
                "alert" => item.data["job"]
                    .as_str()
                    .or_else(|| item.data["detector"].as_str())
                    .unwrap_or("?"),
                _ => continue,
            };
            let (link, label) = origin_of(content, text);
            let line = Line {
                ids: vec![json!(item.id)],
                kind: item.kind.clone(),
                title: headline(text, label.as_deref()),
                label,
                link,
                sphere: if item.kind == "proposal" {
                    change["sphere"].clone()
                } else {
                    Value::Null
                },
            };
            let why = match (item.kind.as_str(), content["risk"].as_str()) {
                ("proposal", Some("conflict")) => Some("conflict"),
                ("question", _) => Some("question"),
                ("reminder", _) => Some("due"),
                ("alert", _) => Some("failed"),
                _ => None,
            };
            if item.kind == "proposal" && content["risk"] == "new" {
                accept_ids.push(json!(item.id));
            }
            match why {
                Some(why) => important.push((why, line)),
                None if item.created_at > since => fresh.push(line),
                None => {
                    older_count += 1;
                    older_since =
                        Some(older_since.map_or(item.created_at, |s| s.min(item.created_at)));
                }
            }
        }

        // Due dates: today, tomorrow and past are important; the rest of the week is listed.
        let today = now.date_naive();
        let mut due_soon = Vec::new();
        let commitments = self
            .ctx
            .ledger
            .assertions(&AssertionFilter {
                kind: Some("commitment".into()),
                status: Some(Status::Active),
                slots: vec![SlotFilter {
                    key: "due".into(),
                    op: SlotOp::Lt,
                    value: (today + Duration::days(8)).to_string(),
                }],
                limit: Some(50),
                ..Default::default()
            })
            .await
            .unwrap_or_default();
        for commitment in commitments {
            let Some(due) = commitment.slots["due"]
                .as_str()
                .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
            else {
                continue;
            };
            let text = commitment.title.as_deref().unwrap_or(&commitment.text);
            let label = issue_key(text).map(str::to_owned);
            let title = headline(text, label.as_deref());
            let why = match (due - today).num_days() {
                d if d < 0 => Some("overdue"),
                0 => Some("due_today"),
                1 => Some("due_tomorrow"),
                _ => None,
            };
            let mut entry = json!({ "id": commitment.assertion_id, "kind": "commitment", "title": title, "label": label, "due": due, "sphere": commitment.sphere });
            if let Some(why) = why {
                entry["why"] = json!(why);
            }
            due_soon.push(entry);
        }

        // Runs that failed since: important.
        let failed: Vec<Run> = runs::list(pool, None, Some("failed"), 50)
            .await?
            .into_iter()
            .filter(|r| r.finished_at.is_some_and(|f| f > since))
            .collect();
        let mut important: Vec<Value> = important
            .into_iter()
            .take(LIST)
            .map(|(why, line)| {
                json!({ "id": line.ids[0], "kind": line.kind, "why": why, "title": line.title, "label": line.label, "link": line.link })
            })
            .collect();
        for run in failed.iter().take(LIST) {
            let already = important
                .iter()
                .any(|i| i["kind"] == "alert" && i["title"] == run.job.as_str());
            if !already {
                important.push(json!({ "id": run.id, "kind": "run", "why": "failed", "title": run.job, "label": null, "link": null, "error": run.error }));
            }
        }
        // Due today, tomorrow or past come first among the due dates and count as important.
        due_soon.sort_by(|a, b| a["due"].as_str().cmp(&b["due"].as_str()));
        let urgent_due = due_soon.iter().filter(|d| d.get("why").is_some()).count();

        Ok(json!({
            "since": since,
            "until": now,
            "important": important,
            "accepted": grouped(accepted),
            "waiting": {
                "count": open.len(),
                "new": grouped(fresh),
                "older": { "count": older_count, "since": older_since },
            },
            "due_soon": due_soon.into_iter().take(LIST).collect::<Vec<_>>(),
            "urgent_due": urgent_due,
            "accept_ids": accept_ids,
        }))
    }

    /// The `digest` job: puts each person's digest into their Inbox when it has something to
    /// say, as often as the person wants it, and moves their checkpoint.
    pub(crate) async fn run_digest(&self, run: &Run) -> Result<Value, JobError> {
        let pool = &self.ctx.pool;
        let people: Vec<Uuid> = sqlx::query_scalar("SELECT DISTINCT user_id FROM channel_links")
            .fetch_all(pool)
            .await?;
        let attention = self.ctx.attention.now();
        let writer = self.jobs().iter().any(|j| j.name == WRITER);
        let mut made = 0;
        for user in people {
            let every = match self.ctx.auth.user_email(user).await {
                Ok(email) => attention
                    .people
                    .get(&email)
                    .and_then(|p| p.digest.clone())
                    .unwrap_or_else(|| "daily".to_owned()),
                Err(_) => "daily".to_owned(),
            };
            let last: Option<DateTime<Utc>> = sqlx::query_scalar(
                "SELECT seen_until FROM attention_checkpoints WHERE user_id = $1",
            )
            .bind(user)
            .fetch_optional(pool)
            .await?;
            let due = match every.as_str() {
                "off" => false,
                // A week since the last one; what is new counts from then.
                "weekly" => last.is_none_or(|l| Utc::now() - l > Duration::hours(6 * 24 + 12)),
                _ => true,
            };
            if !due {
                continue;
            }
            let digest = self.digest_for(user, None).await?;
            let says_something = digest["important"]
                .as_array()
                .is_some_and(|a| !a.is_empty())
                || digest["accepted"].as_array().is_some_and(|a| !a.is_empty())
                || digest["waiting"]["new"]
                    .as_array()
                    .is_some_and(|a| !a.is_empty())
                || digest["urgent_due"].as_u64().unwrap_or(0) > 0;
            sqlx::query(
                "INSERT INTO attention_checkpoints (user_id, seen_until) VALUES ($1, $2) \
                 ON CONFLICT (user_id) DO UPDATE SET seen_until = excluded.seen_until",
            )
            .bind(user)
            .bind(Utc::now())
            .execute(pool)
            .await?;
            if !says_something {
                continue; // nothing new and nothing urgent: no message
            }
            self.close_digests(user).await?;
            // In plain words first, by the agent on the subscription, when the installation has it.
            let (notify_after, investigation) = if writer {
                let input = json!({ "digest": digest, "language": self.language() });
                match self.start_manual(WRITER, &input).await {
                    Ok(started) => (
                        Utc::now() + Duration::minutes(WRITER_WAIT_MINUTES),
                        Some(json!({ "run_id": started.id, "job": WRITER, "state": "running" })),
                    ),
                    Err(err) => {
                        tracing::warn!(error = %err, "cannot start the digest's writer; sending it as it is");
                        (Utc::now(), None)
                    }
                }
            } else {
                (Utc::now(), None)
            };
            inbox::insert(
                pool,
                NewItem {
                    kind: "digest",
                    subject: format!("digest/{user}/{}", run.id),
                    urgency: "now".to_owned(),
                    data: digest,
                    for_users: vec![user],
                    notify_after,
                    investigation,
                },
            )
            .await?;
            made += 1;
        }
        Ok(json!({ "digests": made }))
    }

    /// Earlier digests of a person leave the Inbox without touching their messages (docs/31 D5).
    async fn close_digests(&self, user: Uuid) -> sqlx::Result<()> {
        let pool = &self.ctx.pool;
        let subjects: Vec<String> = sqlx::query_scalar(
            "SELECT subject FROM inbox_items WHERE kind = 'digest' AND state <> 'resolved' AND $1 = ANY(for_users)",
        )
        .bind(user)
        .fetch_all(pool)
        .await?;
        for subject in subjects {
            inbox::close_quietly(pool, &subject).await?;
        }
        Ok(())
    }

    /// The language people read in: the first channel module's, else English.
    fn language(&self) -> String {
        self.job_base
            .as_ref()
            .and_then(|base| {
                base.config
                    .modules
                    .values()
                    .find_map(|m| m.config.get("language").and_then(Value::as_str))
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| "en".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headlines_are_short_and_lose_the_key_they_are_filed_under() {
        let text = "AIRC-1808 (Bug, Chat): a FINISHED session with a long history (1987 records) shows only the projected tail and keeps loading for about 22 minutes";
        let label = issue_key(text);
        assert_eq!(label, Some("AIRC-1808"));
        let title = headline(text, label);
        assert!(title.starts_with("a FINISHED session"), "{title}");
        assert!(title.chars().count() <= TITLE + 1, "{title}");
        assert!(title.ends_with('…'), "{title}");
        assert_eq!(headline("Short and clear", None), "Short and clear");
        assert_eq!(issue_key("Вызов PtyOptions::new в ExecApiService"), None);
        assert_eq!(issue_key("see JBAIP-1968, please"), Some("JBAIP-1968"));
        assert_eq!(issue_key("2026-10-20 and E-1"), None);
    }

    #[test]
    fn findings_about_one_issue_are_one_line() {
        let line = |id: &str, label: Option<&str>, link: Option<&str>| Line {
            ids: vec![json!(id)],
            kind: "proposal".into(),
            title: format!("about {id}"),
            label: label.map(str::to_owned),
            link: link.map(str::to_owned),
            sphere: Value::Null,
        };
        let lines = grouped(vec![
            line("a", Some("AIRC-1808"), None),
            line("b", None, None),
            line("c", Some("AIRC-1808"), None),
            line("d", Some("X-1"), Some("https://yt/issue/X-1")),
            line("e", None, Some("https://yt/issue/X-1")),
        ]);
        let counts: Vec<(Value, Value)> = lines
            .iter()
            .map(|l| (l["label"].clone(), l["count"].clone()))
            .collect();
        assert_eq!(
            counts,
            [
                (json!("AIRC-1808"), json!(2)),
                (Value::Null, json!(1)),
                (json!("X-1"), json!(2)),
            ]
        );
        assert_eq!(lines[0]["ids"], json!(["a", "c"]));
    }
}
