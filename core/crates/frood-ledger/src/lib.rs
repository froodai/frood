//! Knowledge Ledger (docs/11_knowledge_model.md v0.2): sources and evidence, observations,
//! entities, bitemporal assertion versions, proposals and verdicts, and the journal.
//!
//! Knowledge changes only through proposals. A verdict applies one alternative atomically, after
//! checking that every version the proposal was built on is still current.

mod apply;
mod attach;
mod error;
mod journal;
pub mod model;
mod now;
mod policy;
mod query;
mod rows;
mod search;
mod undo;
mod withdraw;

use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

pub use apply::MAX_TITLE_CHARS;
pub use error::{LedgerError, Result};
use error::{invalid, is_unique_violation};
use journal::Event;
use model::*;
pub use now::{Changed, Due, NowView, ProjectRef};
pub use policy::{PolicyConfig, ProposalContext};
pub use search::{SearchHit, SearchQuery};

/// Largest file a capture may carry.
pub const MAX_BLOB_BYTES: usize = 25 * 1024 * 1024;

#[derive(Clone)]
pub struct Ledger {
    pool: PgPool,
    policy: PolicyConfig,
    /// The installation's sphere ids; `None` takes any well-formed id.
    spheres: Option<std::sync::Arc<[String]>>,
    /// Where files go, and the largest one taken.
    objects: std::sync::Arc<frood_store::objects::Objects>,
    max_file_bytes: usize,
}

impl Ledger {
    pub fn new(pool: PgPool, policy: PolicyConfig) -> Self {
        Self {
            pool,
            policy,
            spheres: None,
            objects: std::sync::Arc::new(frood_store::objects::Objects::Database),
            max_file_bytes: MAX_BLOB_BYTES,
        }
    }

    /// Files go to this store (docs/26 §2), up to `max_mb` megabytes each.
    pub fn with_objects(mut self, objects: frood_store::objects::Objects, max_mb: u32) -> Self {
        self.objects = std::sync::Arc::new(objects);
        self.max_file_bytes = max_mb as usize * 1_048_576;
        self
    }

    /// The largest file a capture may carry.
    pub fn max_file_bytes(&self) -> usize {
        self.max_file_bytes
    }

    /// Moves up to `limit` files kept in the database to the files store; how many it moved.
    /// Files stay in the database when that is the store.
    pub async fn move_files_out(&self, limit: i64) -> Result<usize> {
        if self.objects.is_database() {
            return Ok(0);
        }
        let rows: Vec<(Uuid, String, Vec<u8>)> = sqlx::query_as(
            "SELECT id, sha256, data FROM blobs WHERE stored = 'database' ORDER BY created_at LIMIT $1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        for (id, sha, data) in &rows {
            self.objects
                .put(sha, data)
                .await
                .map_err(|e| LedgerError::Files(e.to_string()))?;
            sqlx::query("UPDATE blobs SET data = NULL, stored = 'objects' WHERE id = $1")
                .bind(id)
                .execute(&self.pool)
                .await?;
        }
        Ok(rows.len())
    }

    /// Only these spheres may be named (docs/26 §3); an empty list turns spheres off.
    pub fn with_spheres(mut self, ids: Vec<String>) -> Self {
        self.spheres = Some(ids.into());
        self
    }

    /// The installation's sphere ids, when the Ledger was given them.
    pub fn spheres(&self) -> Option<&[String]> {
        self.spheres.as_deref()
    }

    fn check_spheres(&self, operations: &[Operation]) -> Result<()> {
        let Some(allowed) = &self.spheres else {
            return Ok(());
        };
        for sphere in apply::spheres_in(operations) {
            if allowed.is_empty() {
                return Err(invalid(format!(
                    "sphere {sphere:?}: this installation has no spheres"
                )));
            }
            if !allowed.iter().any(|a| a == sphere) {
                return Err(invalid(format!(
                    "sphere {sphere:?} is not one of this installation's: {}",
                    allowed.join(", ")
                )));
            }
        }
        Ok(())
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Stores a source with one evidence row covering all of it. Capturing the same content
    /// from the same origin again returns the existing source.
    pub async fn capture_source(&self, actor: &Actor, input: NewSource) -> Result<CaptureResult> {
        self.capture(actor, input, None).await
    }

    /// Stores a source, with a file when one came with it (`source.captured` tells the
    /// dispatcher whether there is text to extract or media to transcribe). The same content,
    /// or the same file, from the same origin is captured once.
    pub async fn capture(
        &self,
        actor: &Actor,
        mut input: NewSource,
        blob: Option<NewBlob>,
    ) -> Result<CaptureResult> {
        // A person opens it from the console and the bot: only web addresses, never a script.
        input.uri = input
            .uri
            .map(|uri| uri.trim().to_owned())
            .filter(|uri| !uri.is_empty());
        if let Some(uri) = &input.uri {
            let scheme = uri.split_once("://").map(|(s, _)| s.to_ascii_lowercase());
            if !matches!(scheme.as_deref(), Some("http" | "https")) {
                return Err(invalid(format!("{uri:?} is not an http or https address")));
            }
        }
        if input.kind.trim().is_empty() {
            return Err(invalid("source kind must not be empty"));
        }
        if input.content.trim().is_empty() && blob.is_none() && input.uri.is_none() {
            return Err(invalid("source content must not be empty"));
        }
        if let Some(blob) = &blob {
            if blob.data.is_empty() {
                return Err(invalid("the file is empty"));
            }
            if blob.data.len() > self.max_file_bytes {
                return Err(invalid(format!(
                    "the file is larger than {} MB",
                    self.max_file_bytes / 1_048_576
                )));
            }
        }
        let sha = match (&blob, &input.uri) {
            (Some(blob), _) => hex::encode(Sha256::digest(&blob.data)),
            // A link with no words is the address itself.
            (None, Some(uri)) if input.content.trim().is_empty() => {
                hex::encode(Sha256::digest(uri.as_bytes()))
            }
            (None, _) => hex::encode(Sha256::digest(input.content.as_bytes())),
        };
        let existing: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM sources WHERE origin = $1 AND content_sha256 = $2")
                .bind(&input.origin)
                .bind(&sha)
                .fetch_optional(&self.pool)
                .await?;
        if let Some(id) = existing {
            let source = query::source(&self.pool, id).await?;
            if source.withdrawn_at.is_some() {
                return self.revive(actor, source).await;
            }
            return Ok(CaptureResult {
                source,
                created: false,
            });
        }

        let id = Uuid::now_v7();
        let metadata = if input.metadata.is_null() {
            json!({})
        } else {
            input.metadata
        };
        let content = (!input.content.trim().is_empty()).then_some(input.content.as_str());
        // The file first: a row never points at a file that is not there.
        let in_database = self.objects.is_database();
        if let Some(blob) = &blob
            && !in_database
        {
            self.objects
                .put(&sha, &blob.data)
                .await
                .map_err(|e| LedgerError::Files(e.to_string()))?;
        }
        let mut tx = self.pool.begin().await?;
        let blob_id = match &blob {
            Some(blob) => {
                let blob_id = Uuid::now_v7();
                sqlx::query(
                    "INSERT INTO blobs (id, content_type, filename, size_bytes, sha256, data, stored) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7)",
                )
                .bind(blob_id)
                .bind(&blob.content_type)
                .bind(&blob.filename)
                .bind(i32::try_from(blob.data.len()).unwrap_or(i32::MAX))
                .bind(&sha)
                .bind(in_database.then_some(&blob.data))
                .bind(if in_database { "database" } else { "objects" })
                .execute(&mut *tx)
                .await?;
                Some(blob_id)
            }
            None => None,
        };
        let inserted = sqlx::query(
            "INSERT INTO sources (id, kind, origin, uri, title, content, content_sha256, said_at, captured_by, metadata, blob_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(id)
        .bind(input.kind.trim())
        .bind(&input.origin)
        .bind(&input.uri)
        .bind(&input.title)
        .bind(content)
        .bind(&sha)
        .bind(input.said_at)
        .bind(actor.as_str())
        .bind(&metadata)
        .bind(blob_id)
        .execute(&mut *tx)
        .await;
        if let Err(err) = inserted {
            if is_unique_violation(&err) {
                // Captured concurrently by someone else.
                drop(tx);
                let id: Uuid = sqlx::query_scalar(
                    "SELECT id FROM sources WHERE origin = $1 AND content_sha256 = $2",
                )
                .bind(&input.origin)
                .bind(&sha)
                .fetch_one(&self.pool)
                .await?;
                return Ok(CaptureResult {
                    source: query::source(&self.pool, id).await?,
                    created: false,
                });
            }
            return Err(err.into());
        }
        let evidence_id = Uuid::now_v7();
        sqlx::query("INSERT INTO evidence (id, source_id, anchor) VALUES ($1, $2, $3)")
            .bind(evidence_id)
            .bind(id)
            .bind(json!({ "type": "whole" }))
            .execute(&mut *tx)
            .await?;
        journal::append(
            &mut tx,
            actor.as_str(),
            &[Event::new(
                "source.captured",
                format!("source/{id}"),
                json!({
                    "source_id": id,
                    "kind": input.kind.trim(),
                    "origin": input.origin,
                    "captured_by": actor.as_str(),
                    "has_text": content.is_some(),
                    "has_media": blob.as_ref().is_some_and(NewBlob::is_media),
                    "content_type": blob.as_ref().map(|b| b.content_type.as_str()),
                }),
            )],
        )
        .await?;
        tx.commit().await?;
        Ok(CaptureResult {
            source: query::source(&self.pool, id).await?,
            created: true,
        })
    }

    /// The newest sources first (the console's list of what was sent).
    pub async fn sources(
        &self,
        origin: Option<&str>,
        before: Option<Timestamp>,
        limit: i64,
    ) -> Result<Vec<SourceSummary>> {
        query::sources(&self.pool, origin, before, limit).await
    }

    /// The file of a source, with its bytes: from the database or from the files store.
    pub async fn source_file(&self, source_id: Uuid) -> Result<(BlobInfo, Vec<u8>)> {
        let row = sqlx::query(
            "SELECT b.id, b.content_type, b.filename, b.size_bytes, b.sha256, b.data FROM sources s \
             JOIN blobs b ON b.id = s.blob_id WHERE s.id = $1",
        )
        .bind(source_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| LedgerError::NotFound(format!("file of source {source_id}")))?;
        use sqlx::Row;
        let data = match row.try_get::<Option<Vec<u8>>, _>("data")? {
            Some(data) => data,
            None => self
                .objects
                .get(row.try_get::<&str, _>("sha256")?)
                .await
                .map_err(|e| LedgerError::Files(e.to_string()))?,
        };
        Ok((
            BlobInfo {
                id: row.try_get("id")?,
                content_type: row.try_get("content_type")?,
                filename: row.try_get("filename")?,
                size_bytes: row.try_get("size_bytes")?,
            },
            data,
        ))
    }

    /// Records the transcript of a source's media as its text and announces
    /// `source.transcribed`, which starts extraction.
    pub async fn set_transcript(
        &self,
        actor: &Actor,
        source_id: Uuid,
        text: &str,
        transcript: serde_json::Value,
    ) -> Result<()> {
        if text.trim().is_empty() {
            return Err(invalid("the transcript is empty"));
        }
        let mut tx = self.pool.begin().await?;
        let updated = sqlx::query(
            "UPDATE sources SET content = $2, metadata = metadata || jsonb_build_object('transcript', $3::jsonb) \
             WHERE id = $1",
        )
        .bind(source_id)
        .bind(text)
        .bind(&transcript)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() == 0 {
            return Err(LedgerError::NotFound(format!("source {source_id}")));
        }
        journal::append(
            &mut tx,
            actor.as_str(),
            &[Event::new(
                "source.transcribed",
                format!("source/{source_id}"),
                json!({ "source_id": source_id, "has_text": true }),
            )],
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn add_evidence(&self, input: NewEvidence) -> Result<Evidence> {
        if !input.anchor.is_object() {
            return Err(invalid("evidence anchor must be an object"));
        }
        let id = Uuid::now_v7();
        let row = sqlx::query(
            "INSERT INTO evidence (id, source_id, anchor, quote) \
             SELECT $1, id, $3, $4 FROM sources WHERE id = $2 \
             RETURNING id, source_id, anchor, quote, created_at",
        )
        .bind(id)
        .bind(input.source_id)
        .bind(&input.anchor)
        .bind(&input.quote)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| LedgerError::NotFound(format!("source {}", input.source_id)))?;
        rows::evidence(&row)
    }

    pub async fn add_observation(
        &self,
        actor: &Actor,
        input: NewObservation,
    ) -> Result<Observation> {
        if input.text.trim().is_empty() {
            return Err(invalid("observation text must not be empty"));
        }
        if input.confidence.is_some_and(|c| !(0.0..=1.0).contains(&c)) {
            return Err(invalid("confidence must be between 0 and 1"));
        }
        let mut tx = self.pool.begin().await?;
        let id = Uuid::now_v7();
        let row = sqlx::query(
            "INSERT INTO observations (id, source_id, evidence_id, text, modality, said_at, extracted_by, confidence, flags) \
             SELECT $1, s.id, $3, $4, $5, $6, $7, $8, $9 FROM sources s WHERE s.id = $2 \
             AND ($3::uuid IS NULL OR EXISTS (SELECT 1 FROM evidence e WHERE e.id = $3 AND e.source_id = s.id)) \
             RETURNING id, source_id, evidence_id, text, modality, said_at, extracted_by, confidence, flags, created_at",
        )
        .bind(id)
        .bind(input.source_id)
        .bind(input.evidence_id)
        .bind(input.text.trim())
        .bind(input.modality.as_str())
        .bind(input.said_at)
        .bind(actor.as_str())
        .bind(input.confidence)
        .bind(&input.flags)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| LedgerError::NotFound(format!("source {} (or its evidence)", input.source_id)))?;
        journal::append(
            &mut tx,
            actor.as_str(),
            &[Event::new(
                "observation.extracted",
                format!("observation/{id}"),
                json!({ "observation_id": id, "source_id": input.source_id }),
            )],
        )
        .await?;
        tx.commit().await?;
        rows::observation(&row)
    }

    /// Opens a proposal, then lets the policies decide whether it is accepted right away.
    pub async fn open_proposal(&self, author: &Actor, input: NewProposal) -> Result<Proposal> {
        self.open_proposal_in(author, input, &ProposalContext::default())
            .await
    }

    /// [`Ledger::open_proposal`] with what the policies may know about where it comes from.
    pub async fn open_proposal_in(
        &self,
        author: &Actor,
        input: NewProposal,
        context: &ProposalContext,
    ) -> Result<Proposal> {
        let alternatives = match (input.operations, input.alternatives) {
            (Some(operations), None) => vec![Alternative {
                key: "default".to_owned(),
                label: None,
                operations,
            }],
            (None, Some(alternatives)) => alternatives,
            _ => return Err(invalid("give either operations or alternatives")),
        };
        if alternatives.is_empty() {
            return Err(invalid("a proposal needs at least one alternative"));
        }
        let mut keys: Vec<&str> = alternatives.iter().map(|a| a.key.as_str()).collect();
        keys.sort_unstable();
        if keys.windows(2).any(|w| w[0] == w[1]) || keys.iter().any(|k| k.is_empty()) {
            return Err(invalid("alternative keys must be non-empty and unique"));
        }
        for alternative in &alternatives {
            apply::validate(&alternative.operations, &input.base)
                .and_then(|()| self.check_spheres(&alternative.operations))
                .map_err(|e| invalid(format!("alternative {:?}: {e}", alternative.key)))?;
        }
        if input.confidence.is_some_and(|c| !(0.0..=1.0).contains(&c)) {
            return Err(invalid("confidence must be between 0 and 1"));
        }

        let mut tx = self.pool.begin().await?;
        if let Some(key) = &input.idempotency_key
            && let Some(id) =
                sqlx::query_scalar::<_, Uuid>("SELECT id FROM proposals WHERE idempotency_key = $1")
                    .bind(key)
                    .fetch_optional(&mut *tx)
                    .await?
        {
            return query::proposal(&mut tx, id).await;
        }
        for base in &input.base {
            let current = apply::current_version(&mut tx, base.assertion_id)
                .await?
                .ok_or_else(|| LedgerError::NotFound(format!("assertion {}", base.assertion_id)))?;
            if current.version != base.version {
                return Err(LedgerError::Stale {
                    proposal: None,
                    assertion: base.assertion_id,
                    expected: base.version,
                    actual: current.version,
                });
            }
        }
        let id = Uuid::now_v7();
        let inserted = sqlx::query(
            "INSERT INTO proposals (id, idempotency_key, author, rationale, risk, confidence, base, alternatives, evidence, state) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'open')",
        )
        .bind(id)
        .bind(&input.idempotency_key)
        .bind(author.as_str())
        .bind(&input.rationale)
        .bind(input.risk.as_str())
        .bind(input.confidence)
        .bind(serde_json::to_value(&input.base).expect("base serializes"))
        .bind(serde_json::to_value(&alternatives).expect("alternatives serialize"))
        .bind(&input.evidence)
        .execute(&mut *tx)
        .await;
        if let Err(err) = inserted {
            if is_unique_violation(&err)
                && let Some(key) = &input.idempotency_key
            {
                drop(tx);
                let id: Uuid =
                    sqlx::query_scalar("SELECT id FROM proposals WHERE idempotency_key = $1")
                        .bind(key)
                        .fetch_one(&self.pool)
                        .await?;
                return self.proposal(id).await;
            }
            return Err(err.into());
        }

        let mut events = vec![Event::new(
            "proposal.opened",
            format!("proposal/{id}"),
            json!({ "proposal_id": id, "risk": input.risk, "author": author.as_str() }),
        )];
        if input.risk == Risk::Conflict {
            for base in &input.base {
                sqlx::query(
                    "INSERT INTO assertion_flags (assertion_id, flag, reason) VALUES ($1, 'contested', $2) \
                     ON CONFLICT (assertion_id, flag) DO NOTHING",
                )
                .bind(base.assertion_id)
                .bind(format!("proposal {id}"))
                .execute(&mut *tx)
                .await?;
                events.push(Event::new(
                    "assertion.flagged",
                    format!("assertion/{}", base.assertion_id),
                    json!({ "assertion_id": base.assertion_id, "flag": "contested", "proposal_id": id }),
                ));
            }
        }
        journal::append(&mut tx, author.as_str(), &events).await?;
        tx.commit().await?;

        let proposal = self.proposal(id).await?;
        let context = self.with_own_assertions(&proposal, context).await?;
        if let Some((policy, alternative)) = policy::evaluate(&proposal, &self.policy, &context) {
            let reason = match (&context.authoritative_source, policy) {
                (Some(source), "authoritative-source") => {
                    format!("auto-accepted by policy {policy}: source {source}")
                }
                _ => format!("auto-accepted by policy {policy}"),
            };
            let verdict = VerdictInput {
                decision: Decision::Accept,
                alternative: Some(alternative),
                operations: None,
                reason: Some(reason),
                deferred_until: None,
            };
            match self
                .submit_verdict(&Actor::policy(policy), id, verdict)
                .await
            {
                Ok(decided) if policy == "agent-finding" => {
                    self.mark_unverified(&decided).await?;
                    return Ok(decided);
                }
                Ok(decided) => return Ok(decided),
                Err(LedgerError::Stale { .. }) => return self.proposal(id).await,
                Err(err) => return Err(err),
            }
        }
        Ok(proposal)
    }

    /// The policies' context with the assertions the proposal changes that only its author ever
    /// wrote (their notes, imports, edits) and that nothing contests (docs/27 §4).
    async fn with_own_assertions(
        &self,
        proposal: &Proposal,
        context: &ProposalContext,
    ) -> Result<ProposalContext> {
        let mut context = context.clone();
        let person = proposal.author.split('/').next().unwrap_or_default();
        let [only] = proposal.alternatives.as_slice() else {
            return Ok(context);
        };
        let touched: Vec<Uuid> = only
            .operations
            .iter()
            .filter_map(|op| match op {
                Operation::Supersede { assertion_id, .. }
                | Operation::Close { assertion_id, .. }
                | Operation::Reverify { assertion_id, .. }
                | Operation::SetSphere { assertion_id, .. } => Some(*assertion_id),
                _ => None,
            })
            .collect();
        if !person.starts_with("user:") || touched.is_empty() {
            return Ok(context);
        }
        context.own_assertions = sqlx::query_scalar(
            "SELECT a FROM unnest($1::uuid[]) AS a \
             WHERE NOT EXISTS (SELECT 1 FROM assertion_versions v JOIN proposals p ON p.id = v.proposal_id \
                               WHERE v.assertion_id = a AND split_part(p.author, '/', 1) <> $2) \
               AND NOT EXISTS (SELECT 1 FROM assertion_versions v CROSS JOIN LATERAL unnest(v.evidence) AS e(id) \
                               JOIN evidence ev ON ev.id = e.id JOIN sources s ON s.id = ev.source_id \
                               WHERE v.assertion_id = a AND split_part(s.captured_by, '/', 1) <> $2) \
               AND NOT EXISTS (SELECT 1 FROM assertion_flags f WHERE f.assertion_id = a AND f.flag = 'contested')",
        )
        .bind(&touched)
        .bind(person)
        .fetch_all(&self.pool)
        .await?;
        Ok(context)
    }

    /// What an agent found in an ordinary source and a policy took is marked not verified
    /// (QF3): answers say so, and a person's check takes the mark off.
    async fn mark_unverified(&self, proposal: &Proposal) -> Result<()> {
        let created: Vec<Uuid> = sqlx::query_scalar(
            "SELECT assertion_id FROM assertion_versions WHERE proposal_id = $1 AND version = 1",
        )
        .bind(proposal.id)
        .fetch_all(&self.pool)
        .await?;
        let author = proposal
            .author
            .split("/for/")
            .next()
            .unwrap_or(&proposal.author);
        for assertion_id in created {
            self.flag_for_verification(
                &Actor::policy("agent-finding"),
                assertion_id,
                &format!("found by {author}; not verified"),
            )
            .await?;
        }
        Ok(())
    }

    /// A person confirms knowledge marked not verified is right; the mark comes off.
    pub async fn verify(&self, actor: &Actor, assertion_id: Uuid) -> Result<Proposal> {
        let current = {
            let mut conn = self.pool.acquire().await?;
            apply::current_version(&mut conn, assertion_id)
                .await?
                .ok_or_else(|| LedgerError::NotFound(format!("assertion {assertion_id}")))?
        };
        self.open_proposal(
            actor,
            NewProposal {
                idempotency_key: None,
                rationale: None,
                risk: Risk::Change,
                confidence: None,
                base: vec![BaseRef {
                    assertion_id,
                    version: current.version,
                }],
                operations: Some(vec![Operation::Reverify {
                    assertion_id,
                    review_after: None,
                }]),
                alternatives: None,
                evidence: Vec::new(),
            },
        )
        .await
    }

    /// Records a verdict. Accepting applies one alternative atomically; if a base version is no
    /// longer current, the proposal becomes `stale` and nothing is applied.
    pub async fn submit_verdict(
        &self,
        actor: &Actor,
        proposal_id: Uuid,
        input: VerdictInput,
    ) -> Result<Proposal> {
        if input.decision == Decision::Undo {
            return self.undo(actor, proposal_id).await;
        }
        let mut tx = self.pool.begin().await?;
        let locked: Option<String> =
            sqlx::query_scalar("SELECT state FROM proposals WHERE id = $1 FOR UPDATE")
                .bind(proposal_id)
                .fetch_optional(&mut *tx)
                .await?;
        let state: ProposalState = match locked {
            Some(state) => error::parse_text(&state)?,
            None => return Err(LedgerError::NotFound(format!("proposal {proposal_id}"))),
        };
        if !matches!(state, ProposalState::Open | ProposalState::Deferred) {
            return Err(LedgerError::AlreadyDecided(proposal_id, state));
        }
        let proposal = query::proposal(&mut tx, proposal_id).await?;
        let verdict_id = Uuid::now_v7();
        let subject = format!("proposal/{proposal_id}");

        let (new_state, chosen, edits): (
            Option<ProposalState>,
            Option<String>,
            Option<Vec<Operation>>,
        ) = match input.decision {
            Decision::Reject => (Some(ProposalState::Rejected), None, None),
            Decision::Defer => (Some(ProposalState::Deferred), None, None),
            Decision::Investigate | Decision::Undo => (None, None, None),
            Decision::Accept => (
                Some(ProposalState::Accepted),
                Some(self.choose(&proposal, input.alternative.as_deref())?),
                None,
            ),
            Decision::AcceptWithEdits => {
                let operations = input
                    .operations
                    .clone()
                    .ok_or_else(|| invalid("accept_with_edits needs operations"))?;
                apply::validate(&operations, &proposal.base)?;
                self.check_spheres(&operations)?;
                let key = input
                    .alternative
                    .clone()
                    .unwrap_or_else(|| "edited".to_owned());
                (Some(ProposalState::Accepted), Some(key), Some(operations))
            }
        };

        let mut events = Vec::new();
        let at: chrono::DateTime<chrono::Utc>;
        if new_state == Some(ProposalState::Accepted) {
            let operations = match &edits {
                Some(operations) => operations.clone(),
                None => {
                    let key = chosen.as_deref().unwrap_or_default();
                    proposal
                        .alternatives
                        .iter()
                        .find(|a| a.key == key)
                        .map(|a| a.operations.clone())
                        .unwrap_or_default()
                }
            };
            let touched = apply::touched_assertions(&operations, &proposal.base);
            sqlx::query("SELECT id FROM assertions WHERE id = ANY($1) ORDER BY id FOR UPDATE")
                .bind(&touched)
                .execute(&mut *tx)
                .await?;
            for base in &proposal.base {
                let actual = apply::current_version(&mut tx, base.assertion_id)
                    .await?
                    .map_or(0, |v| v.version);
                if actual != base.version {
                    sqlx::query("UPDATE proposals SET state = 'stale', decided_at = clock_timestamp() WHERE id = $1")
                        .bind(proposal_id)
                        .execute(&mut *tx)
                        .await?;
                    self.release_contested(&mut tx, &proposal).await?;
                    journal::append(
                        &mut tx,
                        actor.as_str(),
                        &[Event::new("proposal.stale", subject, json!({ "proposal_id": proposal_id, "assertion_id": base.assertion_id }))],
                    )
                    .await?;
                    tx.commit().await?;
                    return Err(LedgerError::Stale {
                        proposal: Some(proposal_id),
                        assertion: base.assertion_id,
                        expected: base.version,
                        actual,
                    });
                }
            }
            at = sqlx::query_scalar("SELECT clock_timestamp()")
                .fetch_one(&mut *tx)
                .await?;
            self.insert_verdict(
                &mut tx,
                verdict_id,
                proposal_id,
                &input,
                chosen.as_deref(),
                edits.as_ref(),
                actor,
                at,
            )
            .await?;
            let mut applier =
                apply::Applier::new(&mut tx, at, proposal_id, verdict_id, actor.as_str());
            applier.spheres = self.spheres.as_deref();
            applier.apply(&operations).await?;
            events.append(&mut applier.events);
            events.extend(attach::carry(&mut tx, proposal_id, at).await?);
        } else {
            at = sqlx::query_scalar("SELECT clock_timestamp()")
                .fetch_one(&mut *tx)
                .await?;
            self.insert_verdict(
                &mut tx,
                verdict_id,
                proposal_id,
                &input,
                None,
                None,
                actor,
                at,
            )
            .await?;
        }

        if let Some(new_state) = new_state {
            sqlx::query("UPDATE proposals SET state = $2, deferred_until = $3, decided_at = $4 WHERE id = $1")
                .bind(proposal_id)
                .bind(new_state.as_str())
                .bind(if new_state == ProposalState::Deferred { input.deferred_until } else { None })
                .bind(if new_state == ProposalState::Deferred { None } else { Some(at) })
                .execute(&mut *tx)
                .await?;
            if matches!(new_state, ProposalState::Accepted | ProposalState::Rejected) {
                self.release_contested(&mut tx, &proposal).await?;
            }
        }
        events.insert(
            0,
            Event::new(
                "proposal.verdict",
                subject,
                json!({ "proposal_id": proposal_id, "verdict_id": verdict_id, "decision": input.decision, "alternative": chosen }),
            ),
        );
        journal::append(&mut tx, actor.as_str(), &events).await?;
        tx.commit().await?;
        self.proposal(proposal_id).await
    }

    fn choose(&self, proposal: &Proposal, requested: Option<&str>) -> Result<String> {
        match (requested, proposal.alternatives.as_slice()) {
            (Some(key), alternatives) => alternatives
                .iter()
                .find(|a| a.key == key)
                .map(|a| a.key.clone())
                .ok_or_else(|| invalid(format!("proposal has no alternative {key:?}"))),
            (None, [only]) => Ok(only.key.clone()),
            (None, _) => Err(invalid("the proposal has several alternatives; choose one")),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn insert_verdict(
        &self,
        conn: &mut sqlx::PgConnection,
        id: Uuid,
        proposal_id: Uuid,
        input: &VerdictInput,
        alternative: Option<&str>,
        edits: Option<&Vec<Operation>>,
        actor: &Actor,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO verdicts (id, proposal_id, decision, alternative, edits, reason, decided_by, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(id)
        .bind(proposal_id)
        .bind(input.decision.as_str())
        .bind(alternative)
        .bind(edits.map(|ops| serde_json::to_value(ops).expect("operations serialize")))
        .bind(&input.reason)
        .bind(actor.as_str())
        .bind(at)
        .execute(&mut *conn)
        .await?;
        Ok(())
    }

    /// Clears `contested` from the proposal's base unless another open conflict still holds it.
    async fn release_contested(
        &self,
        conn: &mut sqlx::PgConnection,
        proposal: &Proposal,
    ) -> Result<()> {
        if proposal.risk != Risk::Conflict {
            return Ok(());
        }
        for base in &proposal.base {
            sqlx::query(
                "DELETE FROM assertion_flags f WHERE f.assertion_id = $1 AND f.flag = 'contested' AND NOT EXISTS ( \
                 SELECT 1 FROM proposals p WHERE p.id <> $2 AND p.risk = 'conflict' AND p.state IN ('open', 'deferred') \
                 AND p.base @> jsonb_build_array(jsonb_build_object('assertion_id', $1::text)))",
            )
            .bind(base.assertion_id)
            .bind(proposal.id)
            .execute(&mut *conn)
            .await?;
        }
        Ok(())
    }

    /// Marks an assertion for re-verification. Flags are not knowledge changes, so no proposal.
    pub async fn flag_for_verification(
        &self,
        actor: &Actor,
        assertion_id: Uuid,
        reason: &str,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let done = sqlx::query(
            "INSERT INTO assertion_flags (assertion_id, flag, reason) SELECT id, 'needs_verification', $2 FROM assertions \
             WHERE id = $1 ON CONFLICT (assertion_id, flag) DO NOTHING",
        )
        .bind(assertion_id)
        .bind(reason)
        .execute(&mut *tx)
        .await?;
        if done.rows_affected() > 0 {
            journal::append(
                &mut tx,
                actor.as_str(),
                &[Event::new(
                    "assertion.flagged",
                    format!("assertion/{assertion_id}"),
                    json!({ "assertion_id": assertion_id, "flag": "needs_verification", "reason": reason }),
                )],
            )
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn source(&self, id: Uuid) -> Result<Source> {
        query::source(&self.pool, id).await
    }

    pub async fn assertion(
        &self,
        id: Uuid,
        as_of: Option<Timestamp>,
        valid_at: Option<Timestamp>,
    ) -> Result<AssertionView> {
        query::assertion(&self.pool, id, as_of, valid_at).await
    }

    /// Evidence by id, with its quotes; unknown ids are left out.
    pub async fn evidence(&self, ids: &[Uuid]) -> Result<Vec<Evidence>> {
        let rows = sqlx::query(
            "SELECT id, source_id, anchor, quote, created_at FROM evidence WHERE id = ANY($1) ORDER BY created_at",
        )
        .bind(ids)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(rows::evidence).collect()
    }

    pub async fn history(&self, id: Uuid) -> Result<Vec<AssertionVersion>> {
        query::history(&self.pool, id).await
    }

    pub async fn assertions(&self, filter: &AssertionFilter) -> Result<Vec<AssertionVersion>> {
        query::assertions(&self.pool, filter).await
    }

    /// What holds, what is due and what changed (docs/25 §3.1), for one project or all, in
    /// one sphere or all.
    pub async fn now(&self, project: Option<Uuid>, sphere: Option<&str>) -> Result<NowView> {
        now::now(&self.pool, project, sphere, chrono::Utc::now()).await
    }

    /// Moves an assertion to another sphere. A person's own move is accepted at once.
    pub async fn set_sphere(
        &self,
        actor: &Actor,
        assertion_id: Uuid,
        sphere: Option<String>,
    ) -> Result<Proposal> {
        let current = {
            let mut conn = self.pool.acquire().await?;
            apply::current_version(&mut conn, assertion_id)
                .await?
                .ok_or_else(|| LedgerError::NotFound(format!("assertion {assertion_id}")))?
        };
        self.open_proposal(
            actor,
            NewProposal {
                idempotency_key: None,
                rationale: None,
                risk: Risk::Change,
                confidence: None,
                base: vec![BaseRef {
                    assertion_id,
                    version: current.version,
                }],
                operations: Some(vec![Operation::SetSphere {
                    assertion_id,
                    sphere,
                }]),
                alternatives: None,
                evidence: Vec::new(),
            },
        )
        .await
    }

    /// The sphere most current assertions about each entity have: a hint for new ones.
    pub async fn usual_spheres(
        &self,
        entities: &[Uuid],
    ) -> Result<std::collections::HashMap<Uuid, String>> {
        Ok(sqlx::query_as::<_, (Uuid, String)>(
            "SELECT DISTINCT ON (entity) entity, sphere FROM ( \
               SELECT unnest(subjects) AS entity, sphere, count(*) AS n FROM assertion_versions \
               WHERE recorded_to IS NULL AND status = 'active' AND sphere IS NOT NULL AND subjects && $1 \
                 AND ($2::text[] IS NULL OR sphere = ANY($2)) \
               GROUP BY 1, 2) counted \
             WHERE entity = ANY($1) ORDER BY entity, n DESC, sphere",
        )
        .bind(entities)
        .bind(self.spheres.as_deref())
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .collect())
    }

    pub async fn entity(&self, id: Uuid) -> Result<EntityView> {
        query::entity(&self.pool, id).await
    }

    pub async fn entities(
        &self,
        name: Option<&str>,
        entity_type: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Entity>> {
        query::entities(&self.pool, name, entity_type, limit).await
    }

    pub async fn proposal(&self, id: Uuid) -> Result<Proposal> {
        let mut conn = self.pool.acquire().await?;
        query::proposal(&mut conn, id).await
    }

    pub async fn proposals(
        &self,
        state: Option<ProposalState>,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<Vec<Proposal>> {
        query::proposals(&self.pool, state, after, limit).await
    }

    /// Appends an event from another part of the core (runs, questions, schedules) to the
    /// journal, in the journal's single order.
    pub async fn append_event(
        &self,
        actor: &str,
        event_type: &'static str,
        subject: String,
        data: serde_json::Value,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        journal::append(&mut tx, actor, &[Event::new(event_type, subject, data)]).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Appends an event inside the caller's transaction: it is recorded only together with the
    /// caller's own changes.
    pub async fn append_event_in(
        conn: &mut sqlx::PgConnection,
        actor: &str,
        event_type: &'static str,
        subject: String,
        data: serde_json::Value,
    ) -> Result<()> {
        journal::append(conn, actor, &[Event::new(event_type, subject, data)]).await
    }

    /// Proposals that cite evidence from a source, oldest first.
    pub async fn proposals_citing_source(&self, source_id: Uuid) -> Result<Vec<Proposal>> {
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT p.id FROM proposals p WHERE p.evidence && \
             (SELECT coalesce(array_agg(e.id), '{}') FROM evidence e WHERE e.source_id = $1) \
             ORDER BY p.created_at",
        )
        .bind(source_id)
        .fetch_all(&self.pool)
        .await?;
        let mut conn = self.pool.acquire().await?;
        let mut proposals = Vec::with_capacity(ids.len());
        for id in ids {
            proposals.push(query::proposal(&mut conn, id).await?);
        }
        Ok(proposals)
    }

    /// Hybrid search over current knowledge (words, and meaning when an embedding is given).
    pub async fn search(&self, query: &SearchQuery) -> Result<Vec<SearchHit>> {
        search::search(&self.pool, query).await
    }

    /// Proposals still waiting for a verdict whose texts share words with `text`.
    pub async fn pending_proposals(&self, text: &str, limit: i64) -> Result<Vec<Proposal>> {
        let mut out = Vec::new();
        for id in search::pending_proposals(&self.pool, text, limit).await? {
            out.push(self.proposal(id).await?);
        }
        Ok(out)
    }

    /// Current versions without an embedding from `model`: (assertion, version, text).
    pub async fn missing_embeddings(
        &self,
        model: &str,
        limit: i64,
    ) -> Result<Vec<(Uuid, i32, String)>> {
        search::missing_embeddings(&self.pool, model, limit).await
    }

    pub async fn store_embeddings(
        &self,
        model: &str,
        items: &[((Uuid, i32), Vec<f32>)],
    ) -> Result<()> {
        search::store_embeddings(&self.pool, model, items).await
    }

    /// Entities a text names, by name, alias or a similar name.
    pub async fn entities_in(&self, text: &str, limit: i64) -> Result<Vec<Entity>> {
        search::entities_in(&self.pool, text, limit).await
    }

    pub async fn journal(&self, after: i64, limit: i64) -> Result<Vec<JournalEvent>> {
        journal::read(&self.pool, after, limit).await
    }

    /// Position of the newest journal event; 0 for an empty journal.
    pub async fn journal_head(&self) -> Result<i64> {
        Ok(
            sqlx::query_scalar::<_, Option<i64>>("SELECT max(position) FROM journal")
                .fetch_one(&self.pool)
                .await?
                .unwrap_or(0),
        )
    }
}
