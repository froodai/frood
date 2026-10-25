//! "Something stalled" detectors (docs/16 §9, C3): checks every few minutes that become alerts in
//! the Inbox, one per problem, and close themselves when the problem is gone.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};

use crate::inbox::{self, NewItem};
use crate::{Dispatch, runs};

/// A problem a detector found: a stable key and what to tell people.
struct Problem {
    key: String,
    data: Value,
}

fn duration(value: Option<&str>, default_minutes: i64) -> Duration {
    value
        .and_then(|v| humantime::parse_duration(v).ok())
        .and_then(|d| Duration::from_std(d).ok())
        .unwrap_or_else(|| Duration::minutes(default_minutes))
}

impl Dispatch {
    /// Runs the detectors at `now`: opens alerts for new problems and closes the ones that are
    /// gone. Returns how many problems there are.
    pub async fn detect_once(&self, now: DateTime<Utc>) -> sqlx::Result<usize> {
        let mut problems = Vec::new();
        problems.extend(self.dispatcher_lag(now).await?);
        problems.extend(self.stuck_runs(now).await?);
        problems.extend(self.failing_jobs().await?);
        problems.extend(self.stale_sources(now).await?);
        problems.extend(self.budgets_running_out().await?);
        problems.extend(self.modules_down(now).await);
        problems.extend(self.backups(now).await?);
        let pool = &self.ctx.pool;
        let urgency = inbox::urgency(&self.ctx.attention, "alert", None);
        for problem in &problems {
            // One alert per occurrence: while the problem lasts — shown or hidden by a person —
            // no new one; after it cleared and came back, a new one.
            let ongoing: Option<bool> = sqlx::query_scalar(
                "SELECT NOT coalesce((data->>'cleared')::boolean, false) FROM inbox_items \
                 WHERE kind = 'alert' AND data->>'key' = $1 ORDER BY created_at DESC LIMIT 1",
            )
            .bind(&problem.key)
            .fetch_optional(pool)
            .await?;
            if ongoing == Some(true) {
                continue;
            }
            let mut data = problem.data.clone();
            data["key"] = json!(problem.key);
            inbox::insert(
                pool,
                NewItem {
                    kind: "alert",
                    subject: format!("detector/{}/{}", problem.key, now.timestamp()),
                    urgency: urgency.clone(),
                    data,
                    for_users: Vec::new(),
                    notify_after: now,
                    investigation: None,
                },
            )
            .await?;
        }
        // Problems that are gone: open alerts close, every alert of them is marked cleared.
        let current: Vec<&str> = problems.iter().map(|p| p.key.as_str()).collect();
        sqlx::query(
            "UPDATE inbox_items SET state = 'resolved', resolution = $2, resolved_at = now(), updated_at = now() \
             WHERE kind = 'alert' AND data ? 'key' AND state <> 'resolved' AND NOT (data->>'key' = ANY($1))",
        )
        .bind(&current)
        .bind(json!({ "action": "recovered", "by": "system:detectors", "at": now }))
        .execute(pool)
        .await?;
        sqlx::query(
            "UPDATE inbox_items SET data = data || '{\"cleared\": true}'::jsonb, updated_at = now() \
             WHERE kind = 'alert' AND data ? 'key' AND NOT coalesce((data->>'cleared')::boolean, false) \
               AND NOT (data->>'key' = ANY($1))",
        )
        .bind(&current)
        .execute(pool)
        .await?;
        Ok(problems.len())
    }

    async fn dispatcher_lag(&self, now: DateTime<Utc>) -> sqlx::Result<Vec<Problem>> {
        let limit = duration(self.ctx.attention.detectors.dispatcher_lag.as_deref(), 10);
        let oldest: Option<DateTime<Utc>> = sqlx::query_scalar(
            "SELECT min(j.occurred_at) FROM journal j \
             WHERE j.position > coalesce((SELECT position FROM journal_checkpoints WHERE reader = 'dispatcher'), 0)",
        )
        .fetch_one(&self.ctx.pool)
        .await?;
        Ok(oldest
            .filter(|at| now - *at > limit)
            .map(|at| Problem {
                key: "dispatcher".into(),
                data: json!({
                    "detector": "dispatcher_lag",
                    "message": format!("The dispatcher is behind: events wait since {}", at.to_rfc3339()),
                    "since": at,
                }),
            })
            .into_iter()
            .collect())
    }

    async fn stuck_runs(&self, now: DateTime<Utc>) -> sqlx::Result<Vec<Problem>> {
        let detectors = &self.ctx.attention.detectors;
        let queued = duration(detectors.queued.as_deref(), 30);
        let overrun = duration(detectors.overrun.as_deref(), 10);
        let mut out = Vec::new();
        for run in runs::list(&self.ctx.pool, None, Some("queued"), 200).await? {
            if now - run.created_at > queued {
                out.push(Problem {
                    key: format!("queued/{}", run.id),
                    data: json!({
                        "detector": "queued", "run_id": run.id, "job": run.job,
                        "message": format!("Run of {} waits in the queue since {}", run.job, run.created_at.to_rfc3339()),
                    }),
                });
            }
        }
        for run in runs::list(&self.ctx.pool, None, Some("running"), 200).await? {
            let Some(started) = run.started_at else {
                continue;
            };
            let budget = self
                .jobs
                .iter()
                .find(|j| j.name == run.job)
                .and_then(|j| j.agent.as_ref())
                .and_then(|a| a.budget.max_minutes)
                .map_or(Duration::hours(1), |m| Duration::minutes(i64::from(m)));
            if now - started > budget + overrun {
                out.push(Problem {
                    key: format!("running/{}", run.id),
                    data: json!({
                        "detector": "overrun", "run_id": run.id, "job": run.job,
                        "message": format!("Run of {} works since {}, longer than its time", run.job, started.to_rfc3339()),
                    }),
                });
            }
        }
        Ok(out)
    }

    async fn failing_jobs(&self) -> sqlx::Result<Vec<Problem>> {
        let in_a_row = i64::from(
            self.ctx
                .attention
                .detectors
                .failures_in_a_row
                .unwrap_or(3)
                .max(1),
        );
        let mut out = Vec::new();
        for job in self.jobs.iter() {
            let last: Vec<(String, Option<String>)> = sqlx::query_as(
                "SELECT status, error FROM runs WHERE job = $1 AND status IN ('succeeded', 'failed') \
                 ORDER BY finished_at DESC NULLS LAST LIMIT $2",
            )
            .bind(&job.name)
            .bind(in_a_row)
            .fetch_all(&self.ctx.pool)
            .await?;
            if last.len() as i64 == in_a_row && last.iter().all(|(s, _)| s == "failed") {
                out.push(Problem {
                    key: format!("failing/{}", job.name),
                    data: json!({
                        "detector": "failures_in_a_row", "job": job.name,
                        "error": last[0].1,
                        "message": format!("Job {} failed {in_a_row} times in a row", job.name),
                    }),
                });
            }
        }
        Ok(out)
    }

    async fn stale_sources(&self, now: DateTime<Utc>) -> sqlx::Result<Vec<Problem>> {
        let limit = duration(
            self.ctx.attention.detectors.source_stale.as_deref(),
            48 * 60,
        );
        // Only jobs that read each source on a schedule are expected to keep up.
        let mut out = Vec::new();
        for job in self.jobs.iter().filter(|j| {
            j.triggers
                .iter()
                .any(|t| t.schedule.is_some() && t.for_each.as_deref() == Some("source"))
        }) {
            for source in self.ctx.sources.now().keys() {
                let read: Option<Value> = sqlx::query_scalar(
                    "SELECT checkpoint FROM source_checkpoints WHERE source = $1 AND reader = $2",
                )
                .bind(source)
                .bind(&job.name)
                .fetch_optional(&self.ctx.pool)
                .await?;
                let until = read
                    .as_ref()
                    .and_then(|c| c.get("read_until"))
                    .and_then(|v| serde_json::from_value::<DateTime<Utc>>(v.clone()).ok());
                // Never read yet: measured from the job's first run.
                let since = match until {
                    Some(until) => Some(until),
                    None => {
                        sqlx::query_scalar::<_, Option<DateTime<Utc>>>(
                            "SELECT min(created_at) FROM runs WHERE job = $1",
                        )
                        .bind(&job.name)
                        .fetch_one(&self.ctx.pool)
                        .await?
                    }
                };
                if let Some(since) = since
                    && now - since > limit
                {
                    out.push(Problem {
                        key: format!("source/{}/{source}", job.name),
                        data: json!({
                            "detector": "source_stale", "job": job.name, "source": source,
                            "message": format!("{} has not read {source} successfully since {}", job.name, since.to_rfc3339()),
                        }),
                    });
                }
            }
        }
        Ok(out)
    }

    async fn budgets_running_out(&self) -> sqlx::Result<Vec<Problem>> {
        let share = self.ctx.attention.detectors.budget_share.unwrap_or(0.8);
        let mut out = Vec::new();
        let accounts: Vec<String> =
            sqlx::query_scalar("SELECT DISTINCT account FROM runs WHERE account IS NOT NULL")
                .fetch_all(&self.ctx.pool)
                .await?;
        for account in accounts {
            let Some(max) = self
                .ctx
                .gateway
                .credential(&account)
                .and_then(|c| c.monthly_usd)
            else {
                continue;
            };
            let spent = runs::spent_this_month(&self.ctx.pool, &account).await?;
            if max > 0.0 && spent >= max * share {
                out.push(Problem {
                    key: format!("budget/{account}"),
                    data: json!({
                        "detector": "budget", "account": account, "spent_usd": spent, "monthly_usd": max,
                        "message": format!("Account {account} has spent ${spent:.2} of ${max:.2} this month"),
                    }),
                });
            }
        }
        Ok(out)
    }

    /// No successful backup for several intervals of its schedule, or the latest restore check
    /// failed (C3).
    async fn backups(&self, now: DateTime<Utc>) -> sqlx::Result<Vec<Problem>> {
        if self.ctx.backup.is_none() {
            return Ok(Vec::new());
        }
        let pool = &self.ctx.pool;
        let mut out = Vec::new();
        let missed = i32::try_from(
            self.ctx
                .attention
                .detectors
                .backups_missed
                .unwrap_or(2)
                .max(1),
        )
        .unwrap_or(2);
        if let Some(interval) = crate::schedules::interval(&self.jobs, "backup", now) {
            let (last_ok, first): (Option<DateTime<Utc>>, Option<DateTime<Utc>>) = sqlx::query_as(
                "SELECT max(finished_at) FILTER (WHERE status = 'succeeded'), min(created_at) \
                 FROM runs WHERE job = 'backup'",
            )
            .fetch_one(pool)
            .await?;
            // Never succeeded: measured from the first attempt.
            if let Some(since) = last_ok.or(first)
                && now - since > interval * missed
            {
                out.push(Problem {
                    key: "backup/missed".into(),
                    data: json!({
                        "detector": "backups_missed",
                        "message": match last_ok {
                            Some(at) => format!("No backup has succeeded since {}", at.to_rfc3339()),
                            None => "No backup has succeeded yet".to_owned(),
                        },
                        "since": since,
                    }),
                });
            }
        }
        let latest: Option<(String, Option<String>, Option<DateTime<Utc>>)> = sqlx::query_as(
            "SELECT status, error, finished_at FROM runs WHERE job = 'backup-verify' \
             AND status IN ('succeeded', 'failed') ORDER BY finished_at DESC NULLS LAST LIMIT 1",
        )
        .fetch_optional(pool)
        .await?;
        if let Some((status, error, at)) = latest
            && status == "failed"
        {
            out.push(Problem {
                key: "backup/verify".into(),
                data: json!({
                    "detector": "backup_verify",
                    "error": error,
                    "message": format!(
                        "Restoring from a backup did not work{}",
                        at.map(|at| format!(" ({})", at.to_rfc3339())).unwrap_or_default()
                    ),
                }),
            });
        }
        Ok(out)
    }

    async fn modules_down(&self, now: DateTime<Utc>) -> Vec<Problem> {
        let limit = duration(self.ctx.attention.detectors.module_down.as_deref(), 5);
        let names: Vec<String> = self.ctx.modules.names().map(str::to_owned).collect();
        let mut health = BTreeMap::new();
        for name in &names {
            health.insert(name.clone(), self.ctx.modules.healthy(name).await);
        }
        let mut down = self.down_since.lock().expect("not poisoned");
        let mut out = Vec::new();
        for (name, healthy) in health {
            if healthy {
                down.remove(&name);
                continue;
            }
            let since = *down.entry(name.clone()).or_insert(now);
            if now - since > limit {
                out.push(Problem {
                    key: format!("module/{name}"),
                    data: json!({
                        "detector": "module_down", "module": name,
                        "message": format!("Module {name} does not answer since {}", since.to_rfc3339()),
                    }),
                });
            }
        }
        out
    }
}
