//! Validating and applying proposal operations (docs/11 §5–6).

use std::collections::{HashMap, HashSet};

use serde_json::json;
use sqlx::{PgConnection, Row};
use uuid::Uuid;

use crate::error::{Result, invalid};
use crate::journal::Event;
use crate::model::{
    AssertionContent, AssertionVersion, BaseRef, KNOWN_KINDS, Operation, Status, Timestamp,
};
use crate::rows::{self, version_columns};

/// Checks that need no database: shapes, kinds, references, and that every change to an
/// existing assertion is covered by `base` (so the stale check protects it).
pub(crate) fn validate(operations: &[Operation], base: &[BaseRef]) -> Result<()> {
    let base_ids: HashSet<Uuid> = base.iter().map(|b| b.assertion_id).collect();
    if base_ids.len() != base.len() {
        return Err(invalid("base lists an assertion twice"));
    }
    let mut entity_refs = HashSet::new();
    let mut assertion_refs = HashSet::new();
    for (index, op) in operations.iter().enumerate() {
        let at = |message: String| invalid(format!("operation {index}: {message}"));
        match op {
            Operation::Create { key, assertion } => {
                validate_content(assertion, &entity_refs).map_err(at)?;
                if let Some(key) = key
                    && !assertion_refs.insert(key.clone())
                {
                    return Err(at(format!("ref {key:?} is used twice")));
                }
            }
            Operation::Supersede {
                assertion_id,
                assertion,
            } => {
                validate_content(assertion, &entity_refs).map_err(at)?;
                require_base(&base_ids, *assertion_id).map_err(at)?;
            }
            Operation::Close {
                assertion_id,
                status,
                ..
            } => {
                require_base(&base_ids, *assertion_id).map_err(at)?;
                if matches!(status, Status::Active | Status::Superseded) {
                    return Err(at(format!(
                        "close takes completed, historical or retired, not {status}"
                    )));
                }
            }
            Operation::Reverify { assertion_id, .. } => {
                require_base(&base_ids, *assertion_id).map_err(at)?
            }
            Operation::Link { from, to, .. } => {
                check_reference(from, &assertion_refs).map_err(at)?;
                check_reference(to, &assertion_refs).map_err(at)?;
                if from == to {
                    return Err(at("an assertion cannot link to itself".to_owned()));
                }
            }
            Operation::Unlink { from, to, .. } => {
                if from == to {
                    return Err(at("an assertion cannot link to itself".to_owned()));
                }
            }
            Operation::CreateEntity {
                key,
                entity_type,
                name,
                ..
            } => {
                if entity_type.trim().is_empty() || name.trim().is_empty() {
                    return Err(at("entity type and name must not be empty".to_owned()));
                }
                if let Some(key) = key
                    && !entity_refs.insert(key.clone())
                {
                    return Err(at(format!("ref {key:?} is used twice")));
                }
            }
            Operation::MergeEntities { from, into } => {
                if from == into {
                    return Err(at("cannot merge an entity into itself".to_owned()));
                }
            }
            Operation::SplitEntity { .. } => {}
        }
    }
    Ok(())
}

fn validate_content(
    content: &AssertionContent,
    entity_refs: &HashSet<String>,
) -> std::result::Result<(), String> {
    if !KNOWN_KINDS.contains(&content.kind.as_str()) {
        return Err(format!(
            "kind {:?} is not one of {}",
            content.kind,
            KNOWN_KINDS.join(", ")
        ));
    }
    if content.text.trim().is_empty() {
        return Err("text must not be empty".to_owned());
    }
    if title(content.title.as_deref()).is_some_and(|t| t.chars().count() > MAX_TITLE_CHARS) {
        return Err(format!(
            "title must be at most {MAX_TITLE_CHARS} characters; the rest belongs in text"
        ));
    }
    if !content.slots.is_object() {
        return Err("slots must be an object".to_owned());
    }
    if !content.authority.is_array() {
        return Err("authority must be an array".to_owned());
    }
    if content.evidence.is_empty() {
        return Err("an assertion needs at least one evidence id".to_owned());
    }
    for subject in &content.subjects {
        check_reference(subject, entity_refs)?;
    }
    Ok(())
}

/// Longest title a person still reads at a glance.
pub const MAX_TITLE_CHARS: usize = 120;

/// A title as stored: trimmed, and none when blank.
fn title(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|t| !t.is_empty())
}

fn require_base(base_ids: &HashSet<Uuid>, id: Uuid) -> std::result::Result<(), String> {
    if base_ids.contains(&id) {
        Ok(())
    } else {
        Err(format!("assertion {id} is changed but not listed in base"))
    }
}

/// A reference is an id or `ref:<key>` defined by an earlier operation.
fn check_reference(value: &str, refs: &HashSet<String>) -> std::result::Result<(), String> {
    match value.strip_prefix("ref:") {
        Some(key) if refs.contains(key) => Ok(()),
        Some(key) => Err(format!(
            "ref {key:?} is not defined by an earlier operation"
        )),
        None => Uuid::parse_str(value)
            .map(|_| ())
            .map_err(|_| format!("{value:?} is neither an id nor ref:<key>")),
    }
}

/// Assertion ids an operation list touches directly; they are locked before applying.
pub(crate) fn touched_assertions(operations: &[Operation], base: &[BaseRef]) -> Vec<Uuid> {
    let mut ids: Vec<Uuid> = base.iter().map(|b| b.assertion_id).collect();
    for op in operations {
        match op {
            Operation::Link { from, to, .. } => {
                ids.extend(
                    [from, to]
                        .into_iter()
                        .filter_map(|v| Uuid::parse_str(v).ok()),
                );
            }
            Operation::Unlink { from, to, .. } => ids.extend([*from, *to]),
            _ => {}
        }
    }
    ids.sort();
    ids.dedup();
    ids
}

pub(crate) struct Applier<'a> {
    pub conn: &'a mut PgConnection,
    pub at: Timestamp,
    pub proposal_id: Uuid,
    pub verdict_id: Uuid,
    pub actor: &'a str,
    pub events: Vec<Event>,
    entity_refs: HashMap<String, Uuid>,
    assertion_refs: HashMap<String, Uuid>,
}

impl<'a> Applier<'a> {
    pub fn new(
        conn: &'a mut PgConnection,
        at: Timestamp,
        proposal_id: Uuid,
        verdict_id: Uuid,
        actor: &'a str,
    ) -> Self {
        Self {
            conn,
            at,
            proposal_id,
            verdict_id,
            actor,
            events: Vec::new(),
            entity_refs: HashMap::new(),
            assertion_refs: HashMap::new(),
        }
    }

    pub async fn apply(&mut self, operations: &[Operation]) -> Result<()> {
        for (index, op) in operations.iter().enumerate() {
            self.apply_one(op).await.map_err(|err| match err {
                crate::LedgerError::Invalid(message) => {
                    invalid(format!("operation {index}: {message}"))
                }
                other => other,
            })?;
        }
        Ok(())
    }

    async fn apply_one(&mut self, op: &Operation) -> Result<()> {
        match op {
            Operation::Create { key, assertion } => {
                let id = Uuid::now_v7();
                sqlx::query("INSERT INTO assertions (id) VALUES ($1)")
                    .bind(id)
                    .execute(&mut *self.conn)
                    .await?;
                let valid_from = assertion
                    .valid_from
                    .or(assertion.said_at)
                    .unwrap_or(self.at);
                let subjects = self.resolve_subjects(&assertion.subjects).await?;
                self.check_evidence(&assertion.evidence).await?;
                self.insert_version(
                    id,
                    1,
                    assertion,
                    &subjects,
                    valid_from,
                    None,
                    Status::Active,
                )
                .await?;
                if let Some(key) = key {
                    self.assertion_refs.insert(key.clone(), id);
                }
                self.event(
                    "assertion.created",
                    id,
                    json!({ "assertion_id": id, "version": 1 }),
                );
            }
            Operation::Supersede {
                assertion_id,
                assertion,
            } => {
                let current = self.current(*assertion_id).await?;
                let subjects = self.resolve_subjects(&assertion.subjects).await?;
                self.check_evidence(&assertion.evidence).await?;
                let new_from = assertion
                    .valid_from
                    .or(assertion.said_at)
                    .unwrap_or(self.at);
                self.close_row(&current).await?;
                let mut next = self.max_version(*assertion_id).await? + 1;
                let to_version = if new_from > current.valid_from {
                    // Keep the old content for the time before the change.
                    let replaced = current.valid_to.is_none_or(|end| end > new_from);
                    let (end, status) = if replaced {
                        (Some(new_from), Status::Superseded)
                    } else {
                        (current.valid_to, current.status)
                    };
                    self.copy_row(&current, next, end, status, current.review_after)
                        .await?;
                    next += 1;
                    self.insert_version(
                        *assertion_id,
                        next,
                        assertion,
                        &subjects,
                        new_from,
                        None,
                        Status::Active,
                    )
                    .await?;
                    next
                } else {
                    // A correction: the new content covers the same period as the old one.
                    self.insert_version(
                        *assertion_id,
                        next,
                        assertion,
                        &subjects,
                        current.valid_from,
                        current.valid_to,
                        Status::Active,
                    )
                    .await?;
                    next
                };
                self.event(
                    "assertion.superseded",
                    *assertion_id,
                    json!({ "assertion_id": assertion_id, "from_version": current.version, "to_version": to_version }),
                );
            }
            Operation::Close {
                assertion_id,
                status,
                valid_to,
            } => {
                let current = self.current(*assertion_id).await?;
                let requested = valid_to.unwrap_or(self.at);
                if requested <= current.valid_from {
                    return Err(invalid(format!(
                        "valid_to must be after the assertion's valid_from {}",
                        current.valid_from
                    )));
                }
                let end = current
                    .valid_to
                    .map_or(requested, |existing| existing.min(requested));
                self.close_row(&current).await?;
                let next = self.max_version(*assertion_id).await? + 1;
                self.copy_row(&current, next, Some(end), *status, current.review_after)
                    .await?;
                self.event(
                    "assertion.closed",
                    *assertion_id,
                    json!({ "assertion_id": assertion_id, "version": next, "status": status }),
                );
            }
            Operation::Reverify {
                assertion_id,
                review_after,
            } => {
                let current = self.current(*assertion_id).await?;
                self.close_row(&current).await?;
                let next = self.max_version(*assertion_id).await? + 1;
                self.copy_row(
                    &current,
                    next,
                    current.valid_to,
                    current.status,
                    *review_after,
                )
                .await?;
                sqlx::query("DELETE FROM assertion_flags WHERE assertion_id = $1 AND flag = 'needs_verification'")
                    .bind(assertion_id)
                    .execute(&mut *self.conn)
                    .await?;
                self.event(
                    "assertion.reverified",
                    *assertion_id,
                    json!({ "assertion_id": assertion_id, "version": next }),
                );
            }
            Operation::Link { from, to, kind } => {
                let from = self.resolve_assertion(from).await?;
                let to = self.resolve_assertion(to).await?;
                sqlx::query(
                    "INSERT INTO assertion_links (id, from_assertion, to_assertion, kind, recorded_from, created_by_verdict) \
                     VALUES ($1, $2, $3, $4, $5, $6) \
                     ON CONFLICT (from_assertion, to_assertion, kind) WHERE recorded_to IS NULL DO NOTHING",
                )
                .bind(Uuid::now_v7())
                .bind(from)
                .bind(to)
                .bind(kind.as_str())
                .bind(self.at)
                .bind(self.verdict_id)
                .execute(&mut *self.conn)
                .await?;
                self.event(
                    "assertion.linked",
                    from,
                    json!({ "from": from, "to": to, "kind": kind }),
                );
            }
            Operation::Unlink { from, to, kind } => {
                let done = sqlx::query(
                    "UPDATE assertion_links SET recorded_to = $4 \
                     WHERE from_assertion = $1 AND to_assertion = $2 AND kind = $3 AND recorded_to IS NULL",
                )
                .bind(from)
                .bind(to)
                .bind(kind.as_str())
                .bind(self.at)
                .execute(&mut *self.conn)
                .await?;
                if done.rows_affected() == 0 {
                    return Err(invalid(format!(
                        "there is no current {kind} link from {from} to {to}"
                    )));
                }
                self.event(
                    "assertion.unlinked",
                    *from,
                    json!({ "from": from, "to": to, "kind": kind }),
                );
            }
            Operation::CreateEntity {
                key,
                entity_type,
                name,
                aliases,
            } => {
                let id = Uuid::now_v7();
                sqlx::query("INSERT INTO entities (id, type, name, aliases, created_by, created_at) VALUES ($1, $2, $3, $4, $5, $6)")
                    .bind(id)
                    .bind(entity_type.trim())
                    .bind(name.trim())
                    .bind(aliases)
                    .bind(self.actor)
                    .bind(self.at)
                    .execute(&mut *self.conn)
                    .await?;
                if let Some(key) = key {
                    self.entity_refs.insert(key.clone(), id);
                }
                self.event_on(
                    "entity.created",
                    format!("entity/{id}"),
                    json!({ "entity_id": id, "type": entity_type, "name": name }),
                );
            }
            Operation::MergeEntities { from, into } => {
                let into_row = self.entity_row(*into).await?;
                if into_row.1.is_some() {
                    return Err(invalid(format!(
                        "entity {into} is itself merged; merge into its target instead"
                    )));
                }
                let from_row = self.entity_row(*from).await?;
                if from_row.1.is_some() {
                    return Err(invalid(format!("entity {from} is already merged")));
                }
                // Entities merged into `from` follow it, so every chain stays one level deep.
                sqlx::query(
                    "UPDATE entities SET merged_into = $2 WHERE id = $1 OR merged_into = $1",
                )
                .bind(from)
                .bind(into)
                .execute(&mut *self.conn)
                .await?;
                self.event_on(
                    "entity.merged",
                    format!("entity/{from}"),
                    json!({ "from": from, "into": into }),
                );
            }
            Operation::SplitEntity { entity } => {
                let done = sqlx::query("UPDATE entities SET merged_into = NULL WHERE id = $1 AND merged_into IS NOT NULL")
                    .bind(entity)
                    .execute(&mut *self.conn)
                    .await?;
                if done.rows_affected() == 0 {
                    return Err(invalid(format!("entity {entity} is not merged")));
                }
                self.event_on(
                    "entity.split",
                    format!("entity/{entity}"),
                    json!({ "entity_id": entity }),
                );
            }
        }
        Ok(())
    }

    fn event(&mut self, event_type: &'static str, assertion_id: Uuid, data: serde_json::Value) {
        self.event_on(event_type, format!("assertion/{assertion_id}"), data);
    }

    fn event_on(&mut self, event_type: &'static str, subject: String, data: serde_json::Value) {
        self.events.push(Event::new(event_type, subject, data));
    }

    /// The latest validity segment of an assertion in current knowledge.
    async fn current(&mut self, id: Uuid) -> Result<AssertionVersion> {
        current_version(&mut *self.conn, id)
            .await?
            .ok_or_else(|| crate::LedgerError::NotFound(format!("assertion {id}")))
    }

    async fn max_version(&mut self, id: Uuid) -> Result<i32> {
        let max: Option<i32> = sqlx::query_scalar(
            "SELECT max(version) FROM assertion_versions WHERE assertion_id = $1",
        )
        .bind(id)
        .fetch_one(&mut *self.conn)
        .await?;
        Ok(max.unwrap_or(0))
    }

    async fn close_row(&mut self, row: &AssertionVersion) -> Result<()> {
        sqlx::query("UPDATE assertion_versions SET recorded_to = $3 WHERE assertion_id = $1 AND version = $2")
            .bind(row.assertion_id)
            .bind(row.version)
            .bind(self.at)
            .execute(&mut *self.conn)
            .await?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn insert_version(
        &mut self,
        id: Uuid,
        version: i32,
        content: &AssertionContent,
        subjects: &[Uuid],
        valid_from: Timestamp,
        valid_to: Option<Timestamp>,
        status: Status,
    ) -> Result<()> {
        let slots = self.resolve_slot_refs(&content.slots)?;
        sqlx::query(
            "INSERT INTO assertion_versions (assertion_id, version, kind, title, text, subjects, slots, modality, \
             authority, said_at, valid_from, valid_to, recorded_from, status, review_after, evidence, proposal_id, \
             verdict_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18)",
        )
        .bind(id)
        .bind(version)
        .bind(&content.kind)
        .bind(title(content.title.as_deref()))
        .bind(content.text.trim())
        .bind(subjects)
        .bind(&slots)
        .bind(content.modality.as_str())
        .bind(&content.authority)
        .bind(content.said_at)
        .bind(valid_from)
        .bind(valid_to)
        .bind(self.at)
        .bind(status.as_str())
        .bind(content.review_after)
        .bind(&content.evidence)
        .bind(self.proposal_id)
        .bind(self.verdict_id)
        .execute(&mut *self.conn)
        .await?;
        Ok(())
    }

    /// Re-records an existing row's content under a new version with a new validity end,
    /// status and review date.
    async fn copy_row(
        &mut self,
        row: &AssertionVersion,
        version: i32,
        valid_to: Option<Timestamp>,
        status: Status,
        review_after: Option<Timestamp>,
    ) -> Result<()> {
        let content = AssertionContent {
            kind: row.kind.clone(),
            title: row.title.clone(),
            text: row.text.clone(),
            subjects: Vec::new(),
            slots: row.slots.clone(),
            modality: row.modality,
            authority: row.authority.clone(),
            said_at: row.said_at,
            valid_from: Some(row.valid_from),
            review_after,
            evidence: row.evidence.clone(),
        };
        self.insert_version(
            row.assertion_id,
            version,
            &content,
            &row.subjects,
            row.valid_from,
            valid_to,
            status,
        )
        .await
    }

    /// Slot values `ref:<key>` (an owner created in the same proposal) become entity ids.
    fn resolve_slot_refs(&self, slots: &serde_json::Value) -> Result<serde_json::Value> {
        let mut slots = slots.clone();
        if let Some(map) = slots.as_object_mut() {
            for value in map.values_mut() {
                if let Some(key) = value.as_str().and_then(|v| v.strip_prefix("ref:")) {
                    let id = self
                        .entity_refs
                        .get(key)
                        .ok_or_else(|| invalid(format!("entity ref {key:?} is not defined")))?;
                    *value = json!(id.to_string());
                }
            }
        }
        Ok(slots)
    }

    async fn resolve_subjects(&mut self, subjects: &[String]) -> Result<Vec<Uuid>> {
        let mut ids = Vec::with_capacity(subjects.len());
        for subject in subjects {
            let id = match subject.strip_prefix("ref:") {
                Some(key) => *self
                    .entity_refs
                    .get(key)
                    .ok_or_else(|| invalid(format!("entity ref {key:?} is not defined")))?,
                None => Uuid::parse_str(subject)
                    .map_err(|_| invalid(format!("{subject:?} is not an entity id")))?,
            };
            ids.push(id);
        }
        ids.sort();
        ids.dedup();
        let found: i64 = sqlx::query_scalar("SELECT count(*) FROM entities WHERE id = ANY($1)")
            .bind(&ids)
            .fetch_one(&mut *self.conn)
            .await?;
        if found != ids.len() as i64 {
            return Err(invalid("some subjects are not existing entities"));
        }
        Ok(ids)
    }

    async fn resolve_assertion(&mut self, value: &str) -> Result<Uuid> {
        let id = match value.strip_prefix("ref:") {
            Some(key) => *self
                .assertion_refs
                .get(key)
                .ok_or_else(|| invalid(format!("assertion ref {key:?} is not defined")))?,
            None => Uuid::parse_str(value)
                .map_err(|_| invalid(format!("{value:?} is not an assertion id")))?,
        };
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM assertions WHERE id = $1)")
                .bind(id)
                .fetch_one(&mut *self.conn)
                .await?;
        if exists {
            Ok(id)
        } else {
            Err(crate::LedgerError::NotFound(format!("assertion {id}")))
        }
    }

    async fn check_evidence(&mut self, evidence: &[Uuid]) -> Result<()> {
        let mut ids = evidence.to_vec();
        ids.sort();
        ids.dedup();
        let found: i64 = sqlx::query_scalar("SELECT count(*) FROM evidence WHERE id = ANY($1)")
            .bind(&ids)
            .fetch_one(&mut *self.conn)
            .await?;
        if found == ids.len() as i64 {
            Ok(())
        } else {
            Err(invalid("some evidence ids do not exist"))
        }
    }

    async fn entity_row(&mut self, id: Uuid) -> Result<(Uuid, Option<Uuid>)> {
        let row = sqlx::query("SELECT id, merged_into FROM entities WHERE id = $1")
            .bind(id)
            .fetch_optional(&mut *self.conn)
            .await?
            .ok_or_else(|| crate::LedgerError::NotFound(format!("entity {id}")))?;
        Ok((row.try_get("id")?, row.try_get("merged_into")?))
    }
}

/// The latest validity segment of an assertion in current knowledge, if it exists.
pub(crate) async fn current_version(
    conn: &mut PgConnection,
    id: Uuid,
) -> Result<Option<AssertionVersion>> {
    let row = sqlx::query(concat!(
        "SELECT ",
        version_columns!(),
        " FROM assertion_versions WHERE assertion_id = $1 AND recorded_to IS NULL ORDER BY valid_from DESC LIMIT 1"
    ))
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    row.as_ref().map(rows::version).transpose()
}
