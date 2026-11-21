//! "Frood stopped learning" (docs/33 S1): sources are connected, but nothing new has entered the
//! Ledger for three working days. The digest and the «Сейчас» page say why, in one line.
//!
//! Unlike the detectors (docs/16 §9), which watch that runs go, this watches what Frood
//! learns: a `daily-sweep` that was never scheduled looks fine to every run check.

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, TimeZone, Utc, Weekday};
use serde_json::{Value, json};

use crate::Dispatch;
use crate::jobs::JobSpec;
use crate::runs::{self, Run};

/// How many working days (Monday to Friday) without news count as stalled.
const WORKING_DAYS: usize = 3;
/// The job that reads sources, whether or not the config declares it.
const SWEEP: &str = "daily-sweep";

fn is_working(day: NaiveDate) -> bool {
    !matches!(day.weekday(), Weekday::Sat | Weekday::Sun)
}

/// The start of the window: local midnight of the third working day counting back from today
/// (today counts when it is one). On Tuesday that is the Friday before.
pub(crate) fn window_start(now: DateTime<Utc>, zone: Option<chrono_tz::Tz>) -> DateTime<Utc> {
    let mut day = match zone {
        Some(zone) => now.with_timezone(&zone).date_naive(),
        None => now.date_naive(),
    };
    let mut counted = usize::from(is_working(day));
    while counted < WORKING_DAYS {
        day -= Duration::days(1);
        if is_working(day) {
            counted += 1;
        }
    }
    let midnight = day.and_time(NaiveTime::MIN);
    match zone {
        Some(zone) => zone
            .from_local_datetime(&midnight)
            .earliest()
            .map_or_else(|| midnight.and_utc(), |t| t.with_timezone(&Utc)),
        None => midnight.and_utc(),
    }
}

/// Whether an agent job reads sources: it gets a `source:<name>` tool.
fn reads_sources(job: &JobSpec) -> bool {
    job.name == SWEEP
        || job
            .agent
            .as_ref()
            .is_some_and(|a| a.tools.iter().any(|t| t.starts_with("source:")))
}

fn is_scheduled(job: &JobSpec) -> bool {
    job.triggers.iter().any(|t| t.schedule.is_some())
}

/// Whether the error reads like a source refusing access: an HTTP 401 or 403, an expired
/// token, a refusal.
fn refused(error: &str) -> bool {
    let lower = error.to_lowercase();
    let status = |code: &str| {
        lower.match_indices(code).any(|(at, _)| {
            let before = lower[..at].chars().next_back();
            let after = lower[at + code.len()..].chars().next();
            !before.is_some_and(|c| c.is_ascii_digit())
                && !after.is_some_and(|c| c.is_ascii_digit())
        })
    };
    status("401")
        || status("403")
        || [
            "unauthorized",
            "unauthorised",
            "forbidden",
            "access denied",
            "permission denied",
            "invalid_token",
            "token expired",
            "expired token",
            "authentication failed",
        ]
        .iter()
        .any(|needle| lower.contains(needle))
}

impl Dispatch {
    /// Why Frood has learned nothing for three working days, when it has not: `None` when no
    /// source is connected or something new came in. The answer names the `reason`
    /// (`not_scheduled`, `not_run`, `failed`, `access_denied`, `found_nothing`) and the `job`,
    /// with the `source` and the `run_id` where there are ones.
    pub async fn stalled(&self, now: DateTime<Utc>) -> sqlx::Result<Option<Value>> {
        let sources = self.ctx.sources.now();
        let Some(first_source) = sources.keys().next().cloned() else {
            return Ok(None);
        };
        let pool = &self.ctx.pool;
        let since = window_start(now, self.ctx.timezone);
        let learned: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM proposals WHERE created_at >= $1 AND created_at <= $2) \
                 OR EXISTS (SELECT 1 FROM assertion_versions WHERE recorded_from >= $1 AND recorded_from <= $2)",
        )
        .bind(since)
        .bind(now)
        .fetch_one(pool)
        .await?;
        if learned {
            return Ok(None);
        }

        let jobs = self.jobs();
        let readers: Vec<&JobSpec> = jobs.iter().filter(|j| reads_sources(j)).collect();
        let scheduled: Vec<&JobSpec> = readers
            .iter()
            .copied()
            .filter(|j| is_scheduled(j))
            .collect();
        let result = |reason: &str, job: &str, source: Option<&str>, run: Option<&Run>| {
            Some(json!({
                "reason": reason, "job": job, "source": source,
                "run_id": run.map(|r| r.id), "since": since,
            }))
        };
        if scheduled.is_empty() {
            let job = readers.first().map_or(SWEEP, |j| j.name.as_str());
            return Ok(result("not_scheduled", job, Some(&first_source), None));
        }
        // A young installation has not had three working days yet.
        let first_event: Option<DateTime<Utc>> =
            sqlx::query_scalar("SELECT min(occurred_at) FROM journal")
                .fetch_one(pool)
                .await?;
        if first_event.is_some_and(|first| first > since) {
            return Ok(None);
        }

        // Runs of the reading jobs in the window, newest first.
        let mut in_window: Vec<Run> = Vec::new();
        for job in &scheduled {
            in_window.extend(
                runs::list(pool, Some(&job.name), None, 100)
                    .await?
                    .into_iter()
                    .filter(|r| r.created_at >= since && r.created_at <= now),
            );
        }
        in_window.sort_by_key(|r| std::cmp::Reverse(r.created_at));
        let name = scheduled[0].name.as_str();
        if in_window.is_empty() {
            return Ok(result("not_run", name, None, None));
        }
        // The latest finished run of each job and subject.
        let mut latest: Vec<&Run> = Vec::new();
        for run in in_window
            .iter()
            .filter(|r| matches!(r.status.as_str(), "succeeded" | "failed"))
        {
            if !latest
                .iter()
                .any(|l| l.job == run.job && l.subject == run.subject)
            {
                latest.push(run);
            }
        }
        if let Some(failed) = latest.iter().find(|r| r.status == "failed") {
            let error = failed.error.as_deref().unwrap_or_default();
            let named = failed
                .subject
                .as_deref()
                .filter(|s| sources.contains_key(*s))
                .map(str::to_owned)
                .or_else(|| {
                    let lower = error.to_lowercase();
                    sources
                        .keys()
                        .find(|k| lower.contains(&k.to_lowercase()))
                        .cloned()
                })
                .or_else(|| (sources.len() == 1).then(|| first_source.clone()));
            return Ok(if refused(error) && named.is_some() {
                result("access_denied", &failed.job, named.as_deref(), Some(failed))
            } else {
                result("failed", &failed.job, named.as_deref(), Some(failed))
            });
        }
        if latest.is_empty() {
            return Ok(None); // runs are still going
        }
        Ok(result("found_nothing", name, None, latest.first().copied()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn the_window_is_three_working_days() {
        let none = |t| window_start(at(t), None);
        // 2026-11-24 is a Tuesday: Tue, Mon, Fri before.
        assert_eq!(none("2026-11-24T10:00:00Z"), at("2026-11-20T00:00:00Z"));
        // Monday: Mon, Fri, Thu.
        assert_eq!(none("2026-11-23T10:00:00Z"), at("2026-11-19T00:00:00Z"));
        // Saturday and Sunday: Fri, Thu, Wed.
        assert_eq!(none("2026-11-21T10:00:00Z"), at("2026-11-18T00:00:00Z"));
        assert_eq!(none("2026-11-22T23:00:00Z"), at("2026-11-18T00:00:00Z"));
        // The installation's zone decides what day it is: 23:00 UTC on Sunday is Monday 02:00
        // in Moscow, so the window starts on Thursday, midnight Moscow time.
        assert_eq!(
            window_start(at("2026-11-22T23:00:00Z"), Some(chrono_tz::Europe::Moscow)),
            at("2026-11-18T21:00:00Z")
        );
    }

    #[test]
    fn a_refusal_is_told_from_a_plain_failure() {
        assert!(refused("youtrack: HTTP 403 Forbidden"));
        assert!(refused("request failed with status 401"));
        assert!(refused("Invalid_Token: the token has expired"));
        assert!(!refused("timed out after 3000 ms"));
        assert!(!refused("exit code 14031"));
    }
}
