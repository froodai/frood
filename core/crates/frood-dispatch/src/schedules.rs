//! Schedules (docs/13 §2): cron triggers of jobs become `schedule.tick` journal events, which
//! the dispatcher turns into runs like any other event. Times are in the trigger's timezone,
//! so "0 3 * * *" in Europe/Amsterdam stays at 03:00 across daylight saving changes.
//!
//! A schedule starts counting when the server first sees it. Ticks missed while the server was
//! down fire once, as the latest of them, after start.

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use croner::Cron;
use frood_ledger::Ledger;
use serde_json::json;

use crate::Dispatch;
use crate::jobs::JobSpec;

/// Journal event type of a tick.
pub(crate) const TICK: &str = "schedule.tick";
const ACTOR: &str = "system:scheduler";

/// One schedule trigger of a job.
pub(crate) struct Schedule {
    job: String,
    trigger: i32,
    expression: String,
    cron: Cron,
    timezone: Tz,
    for_each_source: bool,
}

/// Schedules of the jobs. Bad expressions and zones are already refused by the config check;
/// any that slip through are logged and skipped.
pub(crate) fn schedules(jobs: &[JobSpec]) -> Vec<Schedule> {
    let mut out = Vec::new();
    for job in jobs {
        for (i, trigger) in job.triggers.iter().enumerate() {
            let Some(expression) = &trigger.schedule else {
                continue;
            };
            let zone = trigger.timezone.as_deref().unwrap_or("UTC");
            match (expression.parse::<Cron>(), zone.parse::<Tz>()) {
                (Ok(cron), Ok(timezone)) => out.push(Schedule {
                    job: job.name.clone(),
                    trigger: i as i32,
                    expression: expression.clone(),
                    cron,
                    timezone,
                    for_each_source: trigger.for_each.as_deref() == Some("source"),
                }),
                _ => {
                    tracing::warn!(job = %job.name, expression, zone, "schedule skipped: it does not parse")
                }
            }
        }
    }
    out
}

/// Time between a job's next two scheduled runs, by its first schedule.
pub(crate) fn interval(
    jobs: &[JobSpec],
    job: &str,
    now: DateTime<Utc>,
) -> Option<chrono::Duration> {
    let schedule = schedules(jobs).into_iter().find(|s| s.job == job)?;
    let mut times = schedule
        .cron
        .iter_after(now.with_timezone(&schedule.timezone));
    let (a, b) = (times.next()?, times.next()?);
    Some(b.with_timezone(&Utc) - a.with_timezone(&Utc))
}

impl Schedule {
    /// The latest scheduled time at or before `now`.
    fn latest_due(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.cron
            .find_previous_occurrence(&now.with_timezone(&self.timezone), true)
            .ok()
            .map(|at| at.with_timezone(&Utc))
    }
}

impl Dispatch {
    /// Emits the ticks that are due at `now`. Returns how many. Safe to run on several servers
    /// at once: each tick is claimed in the same transaction that records it.
    pub async fn tick_schedules(&self, now: DateTime<Utc>) -> sqlx::Result<usize> {
        let mut fired = 0;
        for schedule in schedules(&self.jobs) {
            let items: Vec<Option<String>> = if schedule.for_each_source {
                self.ctx.sources.now().keys().cloned().map(Some).collect()
            } else {
                vec![None]
            };
            for item in items {
                if self.tick_one(&schedule, item.as_deref(), now).await? {
                    fired += 1;
                }
            }
        }
        Ok(fired)
    }

    async fn tick_one(
        &self,
        schedule: &Schedule,
        item: Option<&str>,
        now: DateTime<Utc>,
    ) -> sqlx::Result<bool> {
        let pool = &self.ctx.pool;
        let key = item.unwrap_or_default();
        let last: Option<DateTime<Utc>> = sqlx::query_scalar(
            "SELECT last_tick FROM schedule_state WHERE job = $1 AND trigger = $2 AND item = $3",
        )
        .bind(&schedule.job)
        .bind(schedule.trigger)
        .bind(key)
        .fetch_optional(pool)
        .await?;
        let Some(last) = last else {
            sqlx::query(
                "INSERT INTO schedule_state (job, trigger, item, last_tick) VALUES ($1, $2, $3, $4) \
                 ON CONFLICT DO NOTHING",
            )
            .bind(&schedule.job)
            .bind(schedule.trigger)
            .bind(key)
            .bind(now)
            .execute(pool)
            .await?;
            return Ok(false);
        };
        let Some(due) = schedule.latest_due(now).filter(|due| *due > last) else {
            return Ok(false);
        };
        let mut tx = pool.begin().await?;
        let claimed = sqlx::query(
            "UPDATE schedule_state SET last_tick = $5 \
             WHERE job = $1 AND trigger = $2 AND item = $3 AND last_tick = $4",
        )
        .bind(&schedule.job)
        .bind(schedule.trigger)
        .bind(key)
        .bind(last)
        .bind(due)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        if !claimed {
            return Ok(false);
        }
        let subject = match item {
            Some(item) => format!("schedule/{}/{item}", schedule.job),
            None => format!("schedule/{}", schedule.job),
        };
        let missed = schedule
            .cron
            .iter_after(last.with_timezone(&schedule.timezone))
            .take_while(|at| at.with_timezone(&Utc) < due)
            .take(1000)
            .count();
        let data = json!({
            "job": schedule.job,
            "trigger": schedule.trigger,
            "schedule": schedule.expression,
            "timezone": schedule.timezone.name(),
            "scheduled_for": due,
            "source": item,
            "missed": missed,
        });
        Ledger::append_event_in(&mut tx, ACTOR, TICK, subject, data)
            .await
            .map_err(|err| match err {
                frood_ledger::LedgerError::Database(db) => db,
                other => sqlx::Error::Protocol(other.to_string()),
            })?;
        tx.commit().await?;
        tracing::info!(job = %schedule.job, source = ?item, %due, missed, "schedule tick");
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn schedule(expression: &str, zone: &str) -> Schedule {
        Schedule {
            job: "j".into(),
            trigger: 0,
            expression: expression.into(),
            cron: expression.parse().unwrap(),
            timezone: zone.parse().unwrap(),
            for_each_source: false,
        }
    }

    #[test]
    fn ticks_follow_the_zone_across_daylight_saving() {
        let nightly = schedule("0 3 * * *", "Europe/Amsterdam");
        // Summer: 03:00 CEST is 01:00 UTC.
        let summer = Utc.with_ymd_and_hms(2026, 7, 1, 12, 0, 0).unwrap();
        assert_eq!(
            nightly.latest_due(summer),
            Some(Utc.with_ymd_and_hms(2026, 7, 1, 1, 0, 0).unwrap())
        );
        // Winter: 03:00 CET is 02:00 UTC.
        let winter = Utc.with_ymd_and_hms(2026, 12, 1, 12, 0, 0).unwrap();
        assert_eq!(
            nightly.latest_due(winter),
            Some(Utc.with_ymd_and_hms(2026, 12, 1, 2, 0, 0).unwrap())
        );
        // Before today's tick: yesterday's.
        let early = Utc.with_ymd_and_hms(2026, 12, 1, 1, 0, 0).unwrap();
        assert_eq!(
            nightly.latest_due(early),
            Some(Utc.with_ymd_and_hms(2026, 11, 30, 2, 0, 0).unwrap())
        );
    }
}
