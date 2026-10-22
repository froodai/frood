//! Inbox (docs/15, docs/22): one item per situation that needs a person — a proposal waiting
//! for a verdict, an agent's question, a failed run, a due date. Items are made and closed from
//! journal events, routed (who, how urgently, where), shown in channel modules
//! (`notify.send@1`), and their messages edited when the situation is resolved anywhere
//! (`notify.update@1`). Buttons in a channel act through [`Dispatch::inbox_act`] as the person.

use std::collections::BTreeSet;

use chrono::{DateTime, Duration, Utc};
use frood_config::{Attention, BeforeNotify};
use frood_ledger::model::{
    Actor, Decision, Operation, Proposal, ProposalState, Risk, VerdictInput,
};
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::{Dispatch, questions, runs};

const READER: &str = "inbox";
const EVENTS_PER_PASS: i64 = 200;
/// A fuse against floods: most push messages a person gets in 24 hours (B2).
const DEFAULT_MAX_PUSH_PER_DAY: u32 = 30;
const MAX_ATTEMPTS: i32 = 5;

#[derive(Debug, Clone, Serialize)]
pub struct InboxItem {
    pub id: Uuid,
    pub kind: String,
    pub subject: String,
    pub state: String,
    pub urgency: String,
    pub data: Value,
    pub for_users: Vec<Uuid>,
    pub snoozed_until: Option<DateTime<Utc>>,
    pub notify_after: DateTime<Utc>,
    pub investigation: Option<Value>,
    pub resolution: Option<Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
}

macro_rules! columns {
    () => {
        "id, kind, subject, state, urgency, data, for_users, snoozed_until, notify_after, \
         investigation, resolution, created_at, updated_at, resolved_at"
    };
}

fn item(row: &sqlx::postgres::PgRow) -> sqlx::Result<InboxItem> {
    Ok(InboxItem {
        id: row.try_get("id")?,
        kind: row.try_get("kind")?,
        subject: row.try_get("subject")?,
        state: row.try_get("state")?,
        urgency: row.try_get("urgency")?,
        data: row.try_get("data")?,
        for_users: row.try_get("for_users")?,
        snoozed_until: row.try_get("snoozed_until")?,
        notify_after: row.try_get("notify_after")?,
        investigation: row.try_get("investigation")?,
        resolution: row.try_get("resolution")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
        resolved_at: row.try_get("resolved_at")?,
    })
}

/// A new item; nothing happens if the situation already has one.
pub(crate) struct NewItem<'a> {
    pub kind: &'a str,
    pub subject: String,
    pub urgency: String,
    pub data: Value,
    pub for_users: Vec<Uuid>,
    pub notify_after: DateTime<Utc>,
    pub investigation: Option<Value>,
}

pub(crate) async fn insert(pool: &PgPool, new: NewItem<'_>) -> sqlx::Result<Option<InboxItem>> {
    // Deliveries compare with the database's clock: an item due by this host's clock is due by
    // the database's too, even when the two differ by a few milliseconds.
    let row = sqlx::query(concat!(
        "INSERT INTO inbox_items (id, kind, subject, urgency, data, for_users, notify_after, investigation) \
         VALUES ($1, $2, $3, $4, $5, $6, CASE WHEN $7 <= $9 THEN least($7, now()) ELSE $7 END, $8) \
         ON CONFLICT (subject) DO NOTHING RETURNING ",
        columns!()
    ))
    .bind(Uuid::now_v7())
    .bind(new.kind)
    .bind(&new.subject)
    .bind(&new.urgency)
    .bind(&new.data)
    .bind(&new.for_users)
    .bind(new.notify_after)
    .bind(&new.investigation)
    .bind(Utc::now())
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(item).transpose()
}

pub async fn get(pool: &PgPool, id: Uuid) -> sqlx::Result<Option<InboxItem>> {
    let row = sqlx::query(concat!(
        "SELECT ",
        columns!(),
        " FROM inbox_items WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(item).transpose()
}

#[allow(dead_code)]
async fn by_subject(pool: &PgPool, subject: &str) -> sqlx::Result<Option<InboxItem>> {
    let row = sqlx::query(concat!(
        "SELECT ",
        columns!(),
        " FROM inbox_items WHERE subject = $1"
    ))
    .bind(subject)
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(item).transpose()
}

/// Closes the item of a situation. Returns whether it was open.
pub(crate) async fn resolve(
    pool: &PgPool,
    subject: &str,
    resolution: &Value,
) -> sqlx::Result<bool> {
    Ok(sqlx::query(
        "UPDATE inbox_items SET state = 'resolved', resolution = $2, resolved_at = now(), updated_at = now() \
         WHERE subject = $1 AND state <> 'resolved'",
    )
    .bind(subject)
    .bind(resolution)
    .execute(pool)
    .await?
    .rows_affected()
        > 0)
}

/// An undone decision puts the item back: it needs a person again.
async fn reopen(pool: &PgPool, subject: &str) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE inbox_items SET state = 'open', resolution = NULL, resolved_at = NULL, snoozed_until = NULL, \
         updated_at = now() WHERE subject = $1",
    )
    .bind(subject)
    .execute(pool)
    .await?;
    Ok(())
}

async fn snooze(pool: &PgPool, subject: &str, until: DateTime<Utc>) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE inbox_items SET state = 'snoozed', snoozed_until = $2, updated_at = now() \
         WHERE subject = $1 AND state <> 'resolved'",
    )
    .bind(subject)
    .bind(until)
    .execute(pool)
    .await?;
    Ok(())
}

/// Items, open ones first, newest first.
pub async fn list(
    pool: &PgPool,
    include_resolved: bool,
    limit: i64,
) -> sqlx::Result<Vec<InboxItem>> {
    let rows = sqlx::query(concat!(
        "SELECT ",
        columns!(),
        " FROM inbox_items WHERE ($1 OR state <> 'resolved') \
         ORDER BY (state = 'resolved'), (urgency = 'now') DESC, created_at DESC LIMIT $2"
    ))
    .bind(include_resolved)
    .bind(limit.clamp(1, 200))
    .fetch_all(pool)
    .await?;
    rows.iter().map(item).collect()
}

pub async fn seen_by(pool: &PgPool, user: Uuid) -> sqlx::Result<BTreeSet<Uuid>> {
    let ids: Vec<Uuid> = sqlx::query_scalar("SELECT item_id FROM inbox_seen WHERE user_id = $1")
        .bind(user)
        .fetch_all(pool)
        .await?;
    Ok(ids.into_iter().collect())
}

pub async fn mark_seen(pool: &PgPool, item: Uuid, user: Uuid) -> sqlx::Result<()> {
    sqlx::query("INSERT INTO inbox_seen (item_id, user_id) VALUES ($1, $2) ON CONFLICT DO NOTHING")
        .bind(item)
        .bind(user)
        .execute(pool)
        .await?;
    Ok(())
}

/// The people an actor stands for: `user:<id>…` and `agent:<job>/for/<id>`.
fn people_of(actor: &str) -> Vec<Uuid> {
    let id = actor
        .strip_prefix("user:")
        .map(|rest| rest.split('/').next().unwrap_or_default())
        .or_else(|| actor.split("/for/").nth(1));
    id.and_then(|id| id.parse().ok()).into_iter().collect()
}

fn risk_name(risk: Risk) -> String {
    serde_json::to_value(risk)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// How urgent a kind of situation is: the configured routes, then the built-in table (B2).
pub(crate) fn urgency(attention: &Attention, kind: &str, risk: Option<&str>) -> String {
    let configured = attention.routes.iter().find(|route| {
        route.when.kind == kind && route.when.risk.as_deref().is_none_or(|r| Some(r) == risk)
    });
    if let Some(route) = configured {
        return route.urgency.clone();
    }
    match (kind, risk) {
        ("proposal", Some("new" | "entity_merge")) => "digest",
        _ => "now",
    }
    .to_owned()
}

fn before_notify<'a>(
    attention: &'a Attention,
    kind: &str,
    risk: Option<&str>,
) -> Option<&'a BeforeNotify> {
    attention.before_notify.iter().find(|rule| {
        rule.when.kind == kind && rule.when.risk.as_deref().is_none_or(|r| Some(r) == risk)
    })
}

/// What a person may do with an item, for the buttons of a channel.
fn actions(kind: &str, data: &Value, proposal: Option<&Proposal>) -> Vec<Value> {
    match kind {
        "proposal" => {
            let mut out: Vec<Value> = proposal
                .map(|p| {
                    p.alternatives
                        .iter()
                        .map(|a| json!({ "action": "accept", "alternative": a.key, "label": a.label }))
                        .collect()
                })
                .unwrap_or_default();
            out.push(json!({ "action": "reject" }));
            out.push(json!({ "action": "defer" }));
            out
        }
        "question" => {
            let mut out: Vec<Value> = data["options"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(|o| json!({ "action": "answer", "answer": o }))
                .collect();
            out.push(json!({ "action": "snooze" }));
            out
        }
        "reminder" => vec![json!({ "action": "done" }), json!({ "action": "postpone" })],
        // A failed run can be retried; a detector's alert can only be hidden until it clears.
        "alert" if data.get("run_id").is_some() && data.get("detector").is_none() => {
            vec![json!({ "action": "retry" }), json!({ "action": "dismiss" })]
        }
        "alert" => vec![json!({ "action": "dismiss" })],
        _ => vec![json!({ "action": "seen" })],
    }
}

/// What happened, as a channel shows it.
#[derive(Debug, Clone, Serialize)]
pub struct Shown {
    pub item: InboxItem,
    /// Kind-specific content: the proposal's changes, the question, the run, the due date.
    pub content: Value,
    pub actions: Vec<Value>,
}

/// An action a person takes on an item.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct Act {
    pub action: String,
    #[serde(default)]
    pub alternative: Option<String>,
    #[serde(default)]
    pub answer: Option<String>,
    /// For snooze and defer: how long, default a day.
    #[serde(default)]
    pub hours: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct ActOutcome {
    /// done, or already_resolved when someone settled it before.
    pub outcome: &'static str,
    pub item: InboxItem,
}

impl Dispatch {
    /// Items, open first; read the journal first so the list is current.
    pub async fn inbox(&self, include_resolved: bool, limit: i64) -> sqlx::Result<Vec<InboxItem>> {
        self.inbox_pending().await?;
        list(&self.ctx.pool, include_resolved, limit).await
    }

    pub async fn inbox_item(&self, id: Uuid) -> sqlx::Result<Option<InboxItem>> {
        self.inbox_pending().await?;
        get(&self.ctx.pool, id).await
    }

    pub async fn inbox_seen_by(&self, user: Uuid) -> sqlx::Result<BTreeSet<Uuid>> {
        seen_by(&self.ctx.pool, user).await
    }

    /// Turns new journal events into Inbox items and closes the ones whose situation is settled.
    pub async fn inbox_pending(&self) -> sqlx::Result<usize> {
        let pool = &self.ctx.pool;
        sqlx::query(
            "INSERT INTO journal_checkpoints (reader, position) VALUES ($1, 0) ON CONFLICT DO NOTHING",
        )
        .bind(READER)
        .execute(pool)
        .await?;
        let checkpoint: i64 =
            sqlx::query_scalar("SELECT position FROM journal_checkpoints WHERE reader = $1")
                .bind(READER)
                .fetch_one(pool)
                .await?;
        let events = sqlx::query(
            "SELECT position, type, actor, data FROM journal WHERE position > $1 ORDER BY position LIMIT $2",
        )
        .bind(checkpoint)
        .bind(EVENTS_PER_PASS)
        .fetch_all(pool)
        .await?;
        let mut last = checkpoint;
        for event in &events {
            let event_type: String = event.try_get("type")?;
            let actor: String = event.try_get("actor")?;
            let data: Value = event.try_get("data")?;
            if let Err(err) = self.on_event(&event_type, &actor, &data).await {
                tracing::error!(event = %event_type, error = %err, "inbox could not process an event");
                break;
            }
            last = event.try_get("position")?;
        }
        if last > checkpoint {
            sqlx::query(
                "UPDATE journal_checkpoints SET position = $2, updated_at = now() WHERE reader = $1",
            )
            .bind(READER)
            .bind(last)
            .execute(pool)
            .await?;
        }
        Ok(events.len())
    }

    async fn on_event(&self, event_type: &str, actor: &str, data: &Value) -> sqlx::Result<()> {
        let pool = &self.ctx.pool;
        let id = |key: &str| data[key].as_str().and_then(|s| s.parse::<Uuid>().ok());
        let by = json!({ "by": actor, "at": Utc::now() });
        match event_type {
            "proposal.opened" => {
                let Some(pid) = id("proposal_id") else {
                    return Ok(());
                };
                let Ok(proposal) = self.ctx.ledger.proposal(pid).await else {
                    return Ok(());
                };
                if proposal.state != ProposalState::Open {
                    return Ok(()); // a policy accepted it already: the digest tells
                }
                let risk = risk_name(proposal.risk);
                let urgency = urgency(&self.ctx.attention, "proposal", Some(&risk));
                let (notify_after, investigation) = self
                    .investigate_first("proposal", Some(&risk), json!({ "proposal_id": pid }))
                    .await;
                insert(
                    pool,
                    NewItem {
                        kind: "proposal",
                        subject: format!("proposal/{pid}"),
                        urgency,
                        data: json!({ "proposal_id": pid, "risk": risk, "author": proposal.author }),
                        for_users: people_of(&proposal.author),
                        notify_after,
                        investigation,
                    },
                )
                .await?;
            }
            "proposal.verdict" => {
                let Some(pid) = id("proposal_id") else {
                    return Ok(());
                };
                let subject = format!("proposal/{pid}");
                match data["decision"].as_str() {
                    Some("defer") => {
                        let until = self
                            .ctx
                            .ledger
                            .proposal(pid)
                            .await
                            .ok()
                            .and_then(|p| p.deferred_until)
                            .unwrap_or_else(|| Utc::now() + Duration::days(1));
                        snooze(pool, &subject, until).await?;
                    }
                    Some("investigate") => {}
                    Some(decision) => {
                        let mut resolution = by.clone();
                        resolution["action"] = json!(decision);
                        resolution["alternative"] = data["alternative"].clone();
                        resolve(pool, &subject, &resolution).await?;
                    }
                    None => {}
                }
            }
            "proposal.reopened" => {
                if let Some(pid) = id("proposal_id") {
                    reopen(pool, &format!("proposal/{pid}")).await?;
                }
            }
            "proposal.stale" => {
                if let Some(pid) = id("proposal_id") {
                    let mut resolution = by.clone();
                    resolution["action"] = json!("stale");
                    resolve(pool, &format!("proposal/{pid}"), &resolution).await?;
                }
            }
            "question.asked" => {
                let Some(qid) = id("question_id") else {
                    return Ok(());
                };
                let Some(question) = questions::get(pool, qid).await? else {
                    return Ok(());
                };
                insert(
                    pool,
                    NewItem {
                        kind: "question",
                        subject: format!("question/{qid}"),
                        urgency: urgency(&self.ctx.attention, "question", None),
                        data: json!({ "question_id": qid, "run_id": question.run_id, "job": question.job, "options": question.options }),
                        for_users: question.asked_for.into_iter().collect(),
                        notify_after: Utc::now(),
                        investigation: None,
                    },
                )
                .await?;
            }
            "question.answered" | "question.expired" | "question.cancelled" => {
                if let Some(qid) = id("question_id") {
                    let mut resolution = by.clone();
                    resolution["action"] = json!(event_type.trim_start_matches("question."));
                    resolve(pool, &format!("question/{qid}"), &resolution).await?;
                }
            }
            "run.failed" => {
                let Some(rid) = id("run_id") else {
                    return Ok(());
                };
                let Some(run) = runs::get(pool, rid).await? else {
                    return Ok(());
                };
                if self.investigation_ended(rid, None).await? {
                    return Ok(());
                }
                insert(
                    pool,
                    NewItem {
                        kind: "alert",
                        subject: format!("run/{rid}"),
                        urgency: urgency(&self.ctx.attention, "alert", None),
                        data: json!({ "run_id": rid, "job": run.job, "error": run.error }),
                        for_users: run.on_behalf_of.into_iter().collect(),
                        notify_after: Utc::now(),
                        investigation: None,
                    },
                )
                .await?;
            }
            "run.succeeded" => {
                let Some(rid) = id("run_id") else {
                    return Ok(());
                };
                let report = runs::get(pool, rid)
                    .await?
                    .and_then(|r| r.output)
                    .and_then(|o| o["result"].as_str().map(str::to_owned));
                if self.investigation_ended(rid, report.as_deref()).await? {
                    return Ok(());
                }
                // A job that works again settles its earlier failures.
                if let Some(job) = data["job"].as_str() {
                    sqlx::query(
                        "UPDATE inbox_items SET state = 'resolved', resolution = $2, resolved_at = now(), updated_at = now() \
                         WHERE kind = 'alert' AND data->>'job' = $1 AND state <> 'resolved'",
                    )
                    .bind(job)
                    .bind(json!({ "action": "recovered", "by": actor, "at": Utc::now() }))
                    .execute(pool)
                    .await?;
                }
            }
            "run.cancelled" => {
                if let Some(rid) = id("run_id") {
                    self.investigation_ended(rid, None).await?;
                }
            }
            // A commitment that changed or closed: its reminders are settled.
            "assertion.superseded" | "assertion.closed" => {
                if let Some(aid) = id("assertion_id") {
                    sqlx::query(
                        "UPDATE inbox_items SET state = 'resolved', resolution = $2, resolved_at = now(), updated_at = now() \
                         WHERE kind = 'reminder' AND data->>'assertion_id' = $1 AND state <> 'resolved'",
                    )
                    .bind(aid.to_string())
                    .bind(json!({ "action": event_type.trim_start_matches("assertion."), "by": actor, "at": Utc::now() }))
                    .execute(pool)
                    .await?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Starts the investigation an `attention.before_notify` rule asks for. Returns when to
    /// notify and the investigation's record.
    async fn investigate_first(
        &self,
        kind: &str,
        risk: Option<&str>,
        input: Value,
    ) -> (DateTime<Utc>, Option<Value>) {
        let now = Utc::now();
        let Some(rule) = before_notify(&self.ctx.attention, kind, risk) else {
            return (now, None);
        };
        let wait = humantime::parse_duration(&rule.max_wait)
            .ok()
            .and_then(|d| Duration::from_std(d).ok())
            .unwrap_or_else(|| Duration::minutes(30));
        match self
            .start_manual(&rule.run, &json!({ "investigate": input }))
            .await
        {
            Ok(run) => (
                now + wait,
                Some(json!({ "run_id": run.id, "job": rule.run, "state": "running" })),
            ),
            Err(err) => {
                tracing::warn!(job = %rule.run, error = %err, "cannot start the investigation; notifying without it");
                (now, None)
            }
        }
    }

    /// An investigation run ended: its report goes into the item, which may be shown now.
    /// Returns whether the run was an investigation.
    async fn investigation_ended(&self, run_id: Uuid, report: Option<&str>) -> sqlx::Result<bool> {
        Ok(sqlx::query(
            "UPDATE inbox_items SET \
               investigation = investigation || jsonb_build_object('state', $2::text, 'report', $3::text), \
               notify_after = least(notify_after, now()), updated_at = now() \
             WHERE investigation->>'run_id' = $1::text",
        )
        .bind(run_id.to_string())
        .bind(if report.is_some() { "done" } else { "failed" })
        .bind(report)
        .execute(&self.ctx.pool)
        .await?
        .rows_affected()
            > 0)
    }

    /// Reminders of due dates (B4′): each person's list of `{days, at}` — days from the due
    /// date, a time of day in their zone — or the shared `attention.reminders`. An item is made
    /// when its time comes (the scan runs every few minutes), and closes the earlier reminders
    /// of the same due date. Returns how many it made.
    pub async fn remind_once(&self, now: DateTime<Utc>) -> sqlx::Result<usize> {
        use frood_ledger::model::{AssertionFilter, SlotFilter, SlotOp, Status};
        let attention = &self.ctx.attention;
        let mut days: Vec<i32> = attention.reminders.iter().map(|r| r.days).collect();
        for person in attention.people.values() {
            for rule in person.reminders.iter().flatten() {
                days.push(rule.days);
            }
        }
        let (Some(&earliest), Some(&latest)) = (days.iter().min(), days.iter().max()) else {
            return Ok(0); // nobody wants reminders
        };
        let today = now.date_naive();
        let filter = AssertionFilter {
            kind: Some("commitment".into()),
            status: Some(Status::Active),
            slots: vec![
                SlotFilter {
                    key: "due".into(),
                    op: SlotOp::Gte,
                    value: (today - Duration::days(i64::from(latest) + 1)).to_string(),
                },
                SlotFilter {
                    key: "due".into(),
                    op: SlotOp::Lt,
                    value: (today + Duration::days(2 - i64::from(earliest))).to_string(),
                },
            ],
            limit: Some(1000),
            ..Default::default()
        };
        let commitments = match self.ctx.ledger.assertions(&filter).await {
            Ok(found) => found,
            Err(frood_ledger::LedgerError::Database(err)) => return Err(err),
            Err(err) => {
                tracing::warn!(error = %err, "cannot list commitments for reminders");
                return Ok(0);
            }
        };
        let mut made = 0;
        for commitment in commitments {
            let Some(due) = commitment.slots["due"]
                .as_str()
                .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
            else {
                continue;
            };
            // For whom: the person who recorded it, or the one an agent recorded it for.
            let Ok(proposal) = self.ctx.ledger.proposal(commitment.proposal_id).await else {
                continue;
            };
            for user in people_of(&proposal.author) {
                let Some((zone, rules)) = self.reminders_of(user).await else {
                    continue;
                };
                let local_today = now.with_timezone(&zone).date_naive();
                for rule in &rules {
                    if due + Duration::days(i64::from(rule.days)) != local_today {
                        continue;
                    }
                    let Ok(at) = chrono::NaiveTime::parse_from_str(&rule.at, "%H:%M") else {
                        continue;
                    };
                    let Some(at) = local_today
                        .and_time(at)
                        .and_local_timezone(zone)
                        .earliest()
                        .map(|t| t.with_timezone(&Utc))
                    else {
                        continue;
                    };
                    // Made when its time comes, so a later reminder does not close an earlier one
                    // before it is shown.
                    if now + Duration::minutes(10) < at {
                        continue;
                    }
                    let subject = format!(
                        "due/{}/{due}/{}@{}/{user}",
                        commitment.assertion_id, rule.days, rule.at
                    );
                    let new = insert(
                        &self.ctx.pool,
                        NewItem {
                            kind: "reminder",
                            subject: subject.clone(),
                            urgency: urgency(attention, "reminder", None),
                            data: json!({
                                "assertion_id": commitment.assertion_id, "text": commitment.text,
                                "due": due.to_string(), "days": rule.days,
                            }),
                            for_users: vec![user],
                            notify_after: at,
                            investigation: None,
                        },
                    )
                    .await?;
                    if new.is_some() {
                        made += 1;
                        // Earlier reminders of the same due date are over.
                        sqlx::query(
                            "UPDATE inbox_items SET state = 'resolved', resolution = $5, resolved_at = now(), updated_at = now() \
                             WHERE kind = 'reminder' AND state <> 'resolved' AND subject <> $1 \
                               AND data->>'assertion_id' = $2 AND data->>'due' = $3 AND $4 = ANY(for_users)",
                        )
                        .bind(&subject)
                        .bind(commitment.assertion_id.to_string())
                        .bind(due.to_string())
                        .bind(user)
                        .bind(json!({ "action": "next_reminder", "at": now }))
                        .execute(&self.ctx.pool)
                        .await?;
                    }
                }
            }
        }
        Ok(made)
    }

    /// A person's zone and reminders, if they have both.
    async fn reminders_of(
        &self,
        user: Uuid,
    ) -> Option<(chrono_tz::Tz, Vec<frood_config::Reminder>)> {
        let email = self.ctx.auth.user_email(user).await.ok()?;
        let attention = &self.ctx.attention;
        let person = attention.people.get(&email)?;
        let zone = person.timezone.as_deref()?.parse().ok()?;
        let rules = person
            .reminders
            .clone()
            .unwrap_or_else(|| attention.reminders.clone());
        (!rules.is_empty()).then_some((zone, rules))
    }

    /// The item with what a channel needs to show it.
    pub async fn inbox_shown(&self, item: InboxItem) -> sqlx::Result<Shown> {
        let pool = &self.ctx.pool;
        let id = |key: &str| item.data[key].as_str().and_then(|s| s.parse::<Uuid>().ok());
        let mut proposal = None;
        let content = match item.kind.as_str() {
            "proposal" => match id("proposal_id") {
                Some(pid) => match self.ctx.ledger.proposal(pid).await {
                    Ok(p) => {
                        let content = self.proposal_content(&p).await;
                        proposal = Some(p);
                        content
                    }
                    Err(_) => json!({}),
                },
                None => json!({}),
            },
            "question" => match id("question_id") {
                Some(qid) => questions::get(pool, qid)
                    .await?
                    .map(|q| json!({ "text": q.text, "options": q.options, "context": q.context, "job": q.job, "status": q.status }))
                    .unwrap_or_default(),
                None => json!({}),
            },
            _ => item.data.clone(),
        };
        let actions = if item.state == "resolved" {
            Vec::new()
        } else {
            actions(&item.kind, &content, proposal.as_ref())
        };
        Ok(Shown {
            item,
            content,
            actions,
        })
    }

    /// The changes of a proposal as before → after texts, with the quote it rests on.
    async fn proposal_content(&self, p: &Proposal) -> Value {
        let mut alternatives = Vec::new();
        for alternative in &p.alternatives {
            let mut changes = Vec::new();
            for op in &alternative.operations {
                let (kind, before, after) = match op {
                    Operation::Create { assertion, .. } => {
                        (assertion.kind.clone(), None, Some(assertion.text.clone()))
                    }
                    Operation::Supersede {
                        assertion_id,
                        assertion,
                    } => {
                        let before = self
                            .ctx
                            .ledger
                            .assertion(*assertion_id, None, None)
                            .await
                            .ok()
                            .map(|a| a.version.text);
                        (assertion.kind.clone(), before, Some(assertion.text.clone()))
                    }
                    Operation::Close { assertion_id, .. } => {
                        let current = self
                            .ctx
                            .ledger
                            .assertion(*assertion_id, None, None)
                            .await
                            .ok();
                        (
                            current
                                .as_ref()
                                .map(|a| a.version.kind.clone())
                                .unwrap_or_default(),
                            current.map(|a| a.version.text),
                            None,
                        )
                    }
                    _ => continue,
                };
                changes.push(json!({ "kind": kind, "before": before, "after": after }));
            }
            alternatives.push(
                json!({ "key": alternative.key, "label": alternative.label, "changes": changes }),
            );
        }
        // The words it rests on: the quoted part, or the whole source when it was taken whole.
        let evidence = self
            .ctx
            .ledger
            .evidence(&p.evidence)
            .await
            .unwrap_or_default();
        let mut quote = evidence.iter().find_map(|e| e.quote.clone());
        if quote.is_none()
            && let Some(first) = evidence.first()
            && let Ok(source) = self.ctx.ledger.source(first.source_id).await
        {
            quote = source
                .content
                .map(|text| text.chars().take(500).collect::<String>());
        }
        json!({
            "proposal_id": p.id,
            "risk": risk_name(p.risk),
            "author": p.author,
            "rationale": p.rationale,
            "state": p.state,
            "alternatives": alternatives,
            "quote": quote,
        })
    }

    /// Takes back a person's decision on a proposal item (docs/25 §3.2); the item opens again.
    pub async fn inbox_undo(&self, item_id: Uuid, actor: &str) -> Result<InboxItem, String> {
        let pool = &self.ctx.pool;
        let item = get(pool, item_id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("no such Inbox item")?;
        let proposal_id = match item.kind.as_str() {
            "proposal" => item.data["proposal_id"]
                .as_str()
                .and_then(|s| s.parse::<Uuid>().ok())
                .ok_or("the item has no proposal_id")?,
            kind => return Err(format!("a {kind} cannot be undone")),
        };
        let actor = Actor::parse(actor).map_err(|e| e.to_string())?;
        self.ctx
            .ledger
            .undo(&actor, proposal_id)
            .await
            .map_err(|e| e.to_string())?;
        // The journal reopens it too; here so that the answer already shows it open.
        reopen(pool, &item.subject)
            .await
            .map_err(|e| e.to_string())?;
        get(pool, item_id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "no such Inbox item".to_owned())
    }

    /// A person acts on an item, from the API or a channel. `actor` is how the ledger records
    /// it, e.g. `user:<id>/via/telegram`.
    pub async fn inbox_act(
        &self,
        item_id: Uuid,
        user: Uuid,
        actor: &str,
        act: &Act,
    ) -> Result<ActOutcome, String> {
        let pool = &self.ctx.pool;
        let db = |e: sqlx::Error| e.to_string();
        let item = get(pool, item_id)
            .await
            .map_err(db)?
            .ok_or("no such Inbox item")?;
        mark_seen(pool, item_id, user).await.map_err(db)?;
        if item.state == "resolved" {
            return Ok(ActOutcome {
                outcome: "already_resolved",
                item,
            });
        }
        let ledger_actor = Actor::parse(actor).map_err(|e| e.to_string())?;
        let id = |key: &str| {
            item.data[key]
                .as_str()
                .and_then(|s| s.parse::<Uuid>().ok())
                .ok_or_else(|| format!("the item has no {key}"))
        };
        let hours = act.hours.unwrap_or(24).clamp(1, 24 * 30);
        let resolution = json!({ "action": act.action, "by": actor, "at": Utc::now(), "alternative": act.alternative });
        match (item.kind.as_str(), act.action.as_str()) {
            ("proposal", "accept" | "reject" | "defer") => {
                let decision = match act.action.as_str() {
                    "accept" => Decision::Accept,
                    "reject" => Decision::Reject,
                    _ => Decision::Defer,
                };
                let verdict = VerdictInput {
                    decision,
                    alternative: act.alternative.clone(),
                    operations: None,
                    reason: Some(format!("from the Inbox ({actor})")),
                    deferred_until: (decision == Decision::Defer)
                        .then(|| Utc::now() + Duration::hours(hours)),
                };
                match self
                    .ctx
                    .ledger
                    .submit_verdict(&ledger_actor, id("proposal_id")?, verdict)
                    .await
                {
                    Ok(proposal) if proposal.state == ProposalState::Deferred => {
                        snooze(pool, &item.subject, Utc::now() + Duration::hours(hours))
                            .await
                            .map_err(db)?;
                    }
                    Ok(proposal) => {
                        let mut resolution = resolution.clone();
                        resolution["state"] = json!(proposal.state);
                        resolve(pool, &item.subject, &resolution)
                            .await
                            .map_err(db)?;
                    }
                    Err(err) => return Err(err.to_string()),
                }
            }
            ("question", "answer") => {
                let answer = act.answer.as_deref().ok_or("give the answer")?;
                self.answer_question(id("question_id")?, answer, actor)
                    .await?;
                resolve(pool, &item.subject, &resolution)
                    .await
                    .map_err(db)?;
            }
            ("reminder", "done") => {
                self.close_commitment(&item, &ledger_actor).await?;
                resolve(pool, &item.subject, &resolution)
                    .await
                    .map_err(db)?;
            }
            ("reminder", "postpone") => {
                self.postpone_commitment(&item, &ledger_actor, 7).await?;
                resolve(pool, &item.subject, &resolution)
                    .await
                    .map_err(db)?;
            }
            ("alert", "retry") => {
                let run = runs::get(pool, id("run_id")?)
                    .await
                    .map_err(db)?
                    .ok_or("the run is gone")?;
                self.start_manual(&run.job, &run.input).await?;
                resolve(pool, &item.subject, &resolution)
                    .await
                    .map_err(db)?;
            }
            (_, "dismiss") | ("digest", "seen") => {
                resolve(pool, &item.subject, &resolution)
                    .await
                    .map_err(db)?;
            }
            (_, "snooze") => {
                snooze(pool, &item.subject, Utc::now() + Duration::hours(hours))
                    .await
                    .map_err(db)?;
            }
            (_, "seen") => {}
            (kind, action) => return Err(format!("a {kind} item has no action {action}")),
        }
        let item = get(pool, item_id)
            .await
            .map_err(db)?
            .ok_or("no such Inbox item")?;
        Ok(ActOutcome {
            outcome: "done",
            item,
        })
    }

    /// Closes a commitment as done, on the person's word.
    async fn close_commitment(&self, item: &InboxItem, actor: &Actor) -> Result<(), String> {
        let aid: Uuid = item.data["assertion_id"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .ok_or("the reminder names no assertion")?;
        let current = self
            .ctx
            .ledger
            .assertion(aid, None, None)
            .await
            .map_err(|e| e.to_string())?;
        let operations: Vec<Operation> = serde_json::from_value(json!([
            { "op": "close", "assertion_id": aid, "status": "completed", "valid_to": Utc::now() }
        ]))
        .map_err(|e| e.to_string())?;
        self.own_change(
            actor,
            aid,
            current.version.version,
            operations,
            "Done, from a reminder",
        )
        .await
    }

    /// Moves a commitment's due date by some days, on the person's word.
    async fn postpone_commitment(
        &self,
        item: &InboxItem,
        actor: &Actor,
        days: i64,
    ) -> Result<(), String> {
        let aid: Uuid = item.data["assertion_id"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .ok_or("the reminder names no assertion")?;
        let current = self
            .ctx
            .ledger
            .assertion(aid, None, None)
            .await
            .map_err(|e| e.to_string())?
            .version;
        let due = current.slots["due"]
            .as_str()
            .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
            .ok_or("the commitment has no due date")?;
        let mut slots = current.slots.clone();
        slots["due"] = json!((due + Duration::days(days)).format("%Y-%m-%d").to_string());
        let operations: Vec<Operation> = serde_json::from_value(json!([{
            "op": "supersede",
            "assertion_id": aid,
            "assertion": {
                "kind": current.kind, "text": current.text, "subjects": current.subjects,
                "slots": slots, "modality": current.modality, "evidence": current.evidence,
            }
        }]))
        .map_err(|e| e.to_string())?;
        self.own_change(
            actor,
            aid,
            current.version,
            operations,
            &format!("Postponed by {days} days, from a reminder"),
        )
        .await
    }

    /// A change the person makes themselves: proposed and accepted in their name.
    async fn own_change(
        &self,
        actor: &Actor,
        aid: Uuid,
        version: i32,
        operations: Vec<Operation>,
        rationale: &str,
    ) -> Result<(), String> {
        let input = frood_ledger::model::NewProposal {
            idempotency_key: None,
            rationale: Some(rationale.to_owned()),
            risk: Risk::Change,
            confidence: None,
            base: vec![frood_ledger::model::BaseRef {
                assertion_id: aid,
                version,
            }],
            operations: Some(operations),
            alternatives: None,
            evidence: Vec::new(),
        };
        let proposal = self
            .ctx
            .ledger
            .open_proposal(actor, input)
            .await
            .map_err(|e| e.to_string())?;
        if proposal.state == ProposalState::Open {
            let verdict = VerdictInput {
                decision: Decision::Accept,
                alternative: None,
                operations: None,
                reason: Some(rationale.to_owned()),
                deferred_until: None,
            };
            self.ctx
                .ledger
                .submit_verdict(actor, proposal.id, verdict)
                .await
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Items the person should get now in each channel they linked: shows new items and edits
    /// the messages of resolved ones. Returns how many messages were sent or edited.
    pub async fn deliver_once(&self) -> sqlx::Result<usize> {
        let pool = &self.ctx.pool;
        let channels = self.ctx.modules.providers("notify.send");
        let mut done = 0;
        // Snoozed items come back when their time is up.
        sqlx::query(
            "UPDATE inbox_items SET state = 'open', snoozed_until = NULL, notify_after = now(), updated_at = now() \
             WHERE state = 'snoozed' AND snoozed_until <= now()",
        )
        .execute(pool)
        .await?;
        if channels.is_empty() {
            return Ok(0);
        }
        let due = sqlx::query(concat!(
            "SELECT ",
            columns!(),
            " FROM inbox_items WHERE state = 'open' AND urgency = 'now' AND notify_after <= now() \
             ORDER BY created_at LIMIT 50"
        ))
        .fetch_all(pool)
        .await?;
        for row in &due {
            let item = item(row)?;
            let people = if item.for_users.is_empty() {
                // Nobody in particular: everyone with a linked channel (B1).
                sqlx::query_scalar::<_, Uuid>("SELECT DISTINCT user_id FROM channel_links")
                    .fetch_all(pool)
                    .await?
            } else {
                item.for_users.clone()
            };
            let mut shown = None;
            for user in people {
                for channel in &channels {
                    let link: Option<String> = sqlx::query_scalar(
                        "SELECT external_id FROM channel_links WHERE channel = $1 AND user_id = $2 ORDER BY created_at LIMIT 1",
                    )
                    .bind(channel)
                    .bind(user)
                    .fetch_optional(pool)
                    .await?;
                    let Some(external_id) = link else { continue };
                    let previous: Option<(String, i32, DateTime<Utc>)> = sqlx::query_as(
                        "SELECT state, attempts, updated_at FROM inbox_deliveries WHERE item_id = $1 AND user_id = $2 AND channel = $3",
                    )
                    .bind(item.id)
                    .bind(user)
                    .bind(channel)
                    .fetch_optional(pool)
                    .await?;
                    match &previous {
                        Some((state, _, _)) if state != "failed" => continue,
                        Some((_, attempts, at))
                            if *attempts >= MAX_ATTEMPTS
                                || Utc::now()
                                    < *at + Duration::minutes(1 << attempts.clamp(&0, &10)) =>
                        {
                            continue;
                        }
                        _ => {}
                    }
                    if !self.may_push(user).await? {
                        continue;
                    }
                    if shown.is_none() {
                        shown = Some(self.inbox_shown(item.clone()).await?);
                    }
                    let body = json!({
                        "recipient": { "user_id": user, "external_id": external_id },
                        "item": shown,
                    });
                    match self.ctx.modules.call(channel, "notify.send", &body).await {
                        Ok(out) => {
                            sqlx::query(
                                "INSERT INTO inbox_deliveries (item_id, user_id, channel, state, message_ref, attempts) \
                                 VALUES ($1, $2, $3, 'sent', $4, 1) ON CONFLICT (item_id, user_id, channel) DO UPDATE \
                                 SET state = 'sent', message_ref = excluded.message_ref, attempts = inbox_deliveries.attempts + 1, \
                                     last_error = NULL, sent_at = now(), updated_at = now()",
                            )
                            .bind(item.id)
                            .bind(user)
                            .bind(channel)
                            .bind(out["message_ref"].as_str())
                            .execute(pool)
                            .await?;
                            done += 1;
                        }
                        Err(err) => {
                            tracing::warn!(item = %item.id, channel, error = %err, "could not show an Inbox item");
                            sqlx::query(
                                "INSERT INTO inbox_deliveries (item_id, user_id, channel, state, attempts, last_error) \
                                 VALUES ($1, $2, $3, 'failed', 1, $4) ON CONFLICT (item_id, user_id, channel) DO UPDATE \
                                 SET state = 'failed', attempts = inbox_deliveries.attempts + 1, last_error = $4, updated_at = now()",
                            )
                            .bind(item.id)
                            .bind(user)
                            .bind(channel)
                            .bind(err.to_string())
                            .execute(pool)
                            .await?;
                        }
                    }
                }
            }
        }
        done += self.update_resolved().await?;
        Ok(done)
    }

    /// Edits the messages of items resolved since they were shown.
    async fn update_resolved(&self) -> sqlx::Result<usize> {
        let pool = &self.ctx.pool;
        let rows = sqlx::query(
            "SELECT d.item_id, d.user_id, d.channel, d.message_ref, l.external_id FROM inbox_deliveries d \
             JOIN inbox_items i ON i.id = d.item_id \
             LEFT JOIN LATERAL (SELECT external_id FROM channel_links c WHERE c.channel = d.channel AND c.user_id = d.user_id \
                                ORDER BY created_at LIMIT 1) l ON true \
             WHERE i.state = 'resolved' AND d.state = 'sent' LIMIT 50",
        )
        .fetch_all(pool)
        .await?;
        let mut done = 0;
        for row in &rows {
            let item_id: Uuid = row.try_get("item_id")?;
            let user: Uuid = row.try_get("user_id")?;
            let channel: String = row.try_get("channel")?;
            let Some(item) = get(pool, item_id).await? else {
                continue;
            };
            let shown = self.inbox_shown(item).await?;
            let body = json!({
                "recipient": { "user_id": user, "external_id": row.try_get::<Option<String>, _>("external_id")? },
                "message_ref": row.try_get::<Option<String>, _>("message_ref")?,
                "item": shown,
            });
            let state = match self
                .ctx
                .modules
                .call(&channel, "notify.update", &body)
                .await
            {
                Ok(_) => {
                    done += 1;
                    "closed"
                }
                Err(err) => {
                    tracing::warn!(item = %item_id, channel, error = %err, "could not update a shown Inbox item");
                    "closed"
                }
            };
            sqlx::query(
                "UPDATE inbox_deliveries SET state = $4, updated_at = now() WHERE item_id = $1 AND user_id = $2 AND channel = $3",
            )
            .bind(item_id)
            .bind(user)
            .bind(&channel)
            .bind(state)
            .execute(pool)
            .await?;
        }
        Ok(done)
    }

    /// Whether a person may get a push now: quiet hours and the daily fuse (B2).
    async fn may_push(&self, user: Uuid) -> sqlx::Result<bool> {
        let attention = &self.ctx.attention;
        let email = self.ctx.auth.user_email(user).await.ok();
        let person = email.as_deref().and_then(|e| attention.people.get(e));
        if let Some(person) = person
            && let (Some(zone), Some(quiet)) = (&person.timezone, &person.quiet_hours)
            && let (Ok(zone), Some((from, to))) = (
                zone.parse::<chrono_tz::Tz>(),
                frood_config::parse_quiet_hours(quiet),
            )
        {
            let now = Utc::now().with_timezone(&zone).time();
            let quiet_now = if from <= to {
                now >= from && now < to
            } else {
                now >= from || now < to
            };
            if quiet_now {
                return Ok(false);
            }
        }
        let limit = person
            .and_then(|p| p.max_push_per_day)
            .or(attention.max_push_per_day)
            .unwrap_or(DEFAULT_MAX_PUSH_PER_DAY);
        let sent: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM inbox_deliveries WHERE user_id = $1 AND state <> 'failed' AND sent_at > now() - interval '1 day'",
        )
        .bind(user)
        .fetch_one(&self.ctx.pool)
        .await?;
        Ok(sent < i64::from(limit))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn people_come_from_actors() {
        let id = Uuid::now_v7();
        assert_eq!(people_of(&format!("user:{id}")), [id]);
        assert_eq!(people_of(&format!("user:{id}/via/telegram")), [id]);
        assert_eq!(people_of(&format!("agent:daily-sweep/for/{id}")), [id]);
        assert!(people_of("agent:daily-sweep").is_empty());
        assert!(people_of("policy:own-facts").is_empty());
    }

    #[test]
    fn urgency_follows_the_table_and_the_config() {
        let mut attention = Attention::default();
        assert_eq!(urgency(&attention, "proposal", Some("conflict")), "now");
        assert_eq!(urgency(&attention, "proposal", Some("new")), "digest");
        assert_eq!(urgency(&attention, "question", None), "now");
        attention.routes.push(frood_config::AttentionRoute {
            when: frood_config::SituationMatch {
                kind: "proposal".into(),
                risk: Some("new".into()),
            },
            urgency: "now".into(),
        });
        assert_eq!(urgency(&attention, "proposal", Some("new")), "now");
    }
}
