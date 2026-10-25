//! "Now" (docs/25 §3.1, H7): what holds, what is due and what changed, for all knowledge or
//! one project — one read for the console's first page and the bot's `/now`.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{Duration, NaiveDate};
use serde::Serialize;
use sqlx::{PgPool, Postgres, QueryBuilder};
use uuid::Uuid;

use crate::error::Result;
use crate::model::{AssertionVersion, Timestamp};
use crate::query::{entity_group, push_sphere};
use crate::rows::{self, version_columns};

/// How far ahead deadlines are shown.
const AHEAD_DAYS: i64 = 14;
/// How far back changes are shown.
const BACK_DAYS: i64 = 7;
const LIMIT: usize = 30;

#[derive(Debug, Serialize)]
pub struct Due {
    #[serde(flatten)]
    pub version: AssertionVersion,
    /// The commitment's `due` slot.
    pub due: String,
    pub overdue: bool,
}

#[derive(Debug, Serialize)]
pub struct Changed {
    pub assertion: AssertionVersion,
    /// The text of the version this one replaced; none for new knowledge.
    pub before: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ProjectRef {
    pub id: Uuid,
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct NowView {
    pub projects: Vec<ProjectRef>,
    /// Names of the entities the assertions below are about.
    pub names: BTreeMap<Uuid, String>,
    pub deadlines: Vec<Due>,
    pub decisions: Vec<AssertionVersion>,
    pub owners: Vec<AssertionVersion>,
    pub questions: Vec<AssertionVersion>,
    pub changes: Vec<Changed>,
}

/// Assertions of a kind that hold now, newest first.
async fn holding(
    pool: &PgPool,
    kind: &str,
    group: Option<&[Uuid]>,
    sphere: Option<&str>,
    at: Timestamp,
) -> Result<Vec<AssertionVersion>> {
    let mut builder = QueryBuilder::<Postgres>::new(concat!(
        "SELECT DISTINCT ON (assertion_id) ",
        version_columns!(),
        " FROM assertion_versions WHERE recorded_to IS NULL AND status = 'active' AND kind = "
    ));
    builder.push_bind(kind.to_owned());
    builder.push(" AND valid_from <= ").push_bind(at);
    builder
        .push(" AND (valid_to IS NULL OR valid_to > ")
        .push_bind(at)
        .push(")");
    if let Some(group) = group {
        builder.push(" AND subjects && ").push_bind(group.to_vec());
    }
    push_sphere(&mut builder, sphere);
    builder.push(" ORDER BY assertion_id, valid_from DESC");
    let mut versions = builder
        .build()
        .fetch_all(pool)
        .await?
        .iter()
        .map(rows::version)
        .collect::<Result<Vec<_>>>()?;
    versions.sort_by_key(|v| std::cmp::Reverse(v.valid_from));
    Ok(versions)
}

pub(crate) async fn now(
    pool: &PgPool,
    project: Option<Uuid>,
    sphere: Option<&str>,
    at: Timestamp,
) -> Result<NowView> {
    let group = match project {
        Some(id) => Some(entity_group(pool, id).await?),
        None => None,
    };
    let group = group.as_deref();

    let today = at.date_naive();
    let horizon = today + Duration::days(AHEAD_DAYS);
    let mut deadlines: Vec<Due> = holding(pool, "commitment", group, sphere, at)
        .await?
        .into_iter()
        .filter_map(|version| {
            let due = version.slots.get("due")?.as_str()?.to_owned();
            let day = NaiveDate::parse_from_str(due.get(..10)?, "%Y-%m-%d").ok()?;
            (day <= horizon).then(|| Due {
                overdue: day < today,
                due,
                version,
            })
        })
        .collect();
    deadlines.sort_by(|a, b| a.due.cmp(&b.due));
    deadlines.truncate(LIMIT);

    let mut decisions = holding(pool, "decision", group, sphere, at).await?;
    decisions.truncate(LIMIT);
    let mut owners = holding(pool, "responsibility", group, sphere, at).await?;
    owners.truncate(LIMIT);
    let mut questions = holding(pool, "open_question", group, sphere, at).await?;
    questions.truncate(LIMIT);

    let mut builder = QueryBuilder::<Postgres>::new(concat!(
        "SELECT DISTINCT ON (assertion_id) ",
        version_columns!(),
        " FROM assertion_versions WHERE recorded_to IS NULL AND recorded_from > "
    ));
    builder.push_bind(at - Duration::days(BACK_DAYS));
    if let Some(group) = group {
        builder.push(" AND subjects && ").push_bind(group.to_vec());
    }
    push_sphere(&mut builder, sphere);
    builder.push(" ORDER BY assertion_id, valid_from DESC");
    let mut recent = builder
        .build()
        .fetch_all(pool)
        .await?
        .iter()
        .map(rows::version)
        .collect::<Result<Vec<_>>>()?;
    recent.sort_by_key(|v| std::cmp::Reverse(v.recorded_from));
    recent.truncate(LIMIT);
    let since = at - Duration::days(BACK_DAYS);
    let mut changes = Vec::with_capacity(recent.len());
    for assertion in recent {
        let earlier: Vec<(String, Option<String>, String, Timestamp)> = sqlx::query_as(
            "SELECT text, sphere, status, recorded_from FROM assertion_versions \
             WHERE assertion_id = $1 AND version <= $2 ORDER BY version DESC, valid_from DESC LIMIT 20",
        )
        .bind(assertion.assertion_id)
        .bind(assertion.version)
        .fetch_all(pool)
        .await?;
        // Moving knowledge to another sphere is not news: what counts is the version the moves
        // started from.
        let mut content = 0;
        while let (Some(this), Some(previous)) = (earlier.get(content), earlier.get(content + 1)) {
            if previous.0 != this.0 || previous.2 != this.2 || previous.1 == this.1 {
                break;
            }
            content += 1;
        }
        if content > 0 && earlier[content].3 <= since {
            continue;
        }
        let before = earlier.get(content + 1).map(|row| row.0.clone());
        changes.push(Changed { assertion, before });
    }

    let projects: Vec<ProjectRef> = sqlx::query_as::<_, (Uuid, String)>(
        "SELECT id, name FROM entities WHERE type = 'project' AND merged_into IS NULL ORDER BY name LIMIT 50",
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|(id, name)| ProjectRef { id, name })
    .collect();

    let subjects: BTreeSet<Uuid> = deadlines
        .iter()
        .map(|d| &d.version)
        .chain(&decisions)
        .chain(&owners)
        .chain(&questions)
        .chain(changes.iter().map(|c| &c.assertion))
        .flat_map(|v| v.subjects.iter().copied())
        .collect();
    let names =
        sqlx::query_as::<_, (Uuid, String)>("SELECT id, name FROM entities WHERE id = ANY($1)")
            .bind(subjects.into_iter().collect::<Vec<_>>())
            .fetch_all(pool)
            .await?
            .into_iter()
            .collect();

    Ok(NowView {
        projects,
        names,
        deadlines,
        decisions,
        owners,
        questions,
        changes,
    })
}
