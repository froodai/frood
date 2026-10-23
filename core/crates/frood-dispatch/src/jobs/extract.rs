//! `extract` (docs/14 §8): source text → observations → entities → similar assertions →
//! proposals. The model gets the source as quoted data with the related knowledge and answers
//! in a fixed JSON shape; the platform turns the answer into proposals.
//!
//! The model's answer is saved in the run before proposals are made, so a retry after a
//! failure makes the same proposals (their idempotency keys come from the run) and does not
//! pay for a second call.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, NaiveDate, Utc};
use frood_ledger::SearchQuery;
use frood_ledger::model::{
    Actor, Alternative, AssertionContent, BaseRef, Entity, KNOWN_KINDS, Modality, NewEvidence,
    NewObservation, NewProposal, Operation, Risk, Source, Status,
};
use frood_llm::CompleteRequest;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{JobError, JobSpec};
use crate::Ctx;
use crate::runs::{self, Run};

const INSTRUCTIONS: &str = include_str!("../../prompts/extract.md");
const SIMILAR: i64 = 8;
const ENTITIES: i64 = 20;

#[derive(Debug, Deserialize)]
struct Extraction {
    entities: Vec<EntityOut>,
    changes: Vec<ChangeOut>,
}

#[derive(Debug, Deserialize)]
struct EntityOut {
    #[serde(rename = "ref")]
    key: String,
    name: String,
    #[serde(rename = "type")]
    entity_type: String,
    existing_id: Option<String>,
    aliases: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ChangeOut {
    action: String,
    kind: String,
    text: String,
    subjects: Vec<String>,
    slots: Vec<SlotOut>,
    modality: Modality,
    valid_from: Option<String>,
    target_assertion_id: Option<String>,
    close_status: Option<String>,
    conflict: bool,
    confidence: f32,
    quote: String,
    reason: String,
}

#[derive(Debug, Deserialize)]
struct SlotOut {
    key: String,
    value: String,
}

/// JSON Schema of the answer: closed objects, every property required (strict modes).
pub(crate) fn schema() -> Value {
    let nullable = |t: Value| json!({ "anyOf": [t, { "type": "null" }] });
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["entities", "changes"],
        "properties": {
            "entities": { "type": "array", "items": {
                "type": "object", "additionalProperties": false,
                "required": ["ref", "name", "type", "existing_id", "aliases"],
                "properties": {
                    "ref": { "type": "string" },
                    "name": { "type": "string" },
                    "type": { "type": "string", "enum": ["person", "team", "project", "product", "organization", "system", "document", "place", "other"] },
                    "existing_id": nullable(json!({ "type": "string" })),
                    "aliases": { "type": "array", "items": { "type": "string" } }
                }
            } },
            "changes": { "type": "array", "items": {
                "type": "object", "additionalProperties": false,
                "required": ["action", "kind", "text", "subjects", "slots", "modality", "valid_from", "target_assertion_id", "close_status", "conflict", "confidence", "quote", "reason"],
                "properties": {
                    "action": { "type": "string", "enum": ["create", "supersede", "close", "skip"] },
                    "kind": { "type": "string", "enum": KNOWN_KINDS },
                    "text": { "type": "string" },
                    "subjects": { "type": "array", "items": { "type": "string" } },
                    "slots": { "type": "array", "items": {
                        "type": "object", "additionalProperties": false,
                        "required": ["key", "value"],
                        "properties": { "key": { "type": "string" }, "value": { "type": "string" } }
                    } },
                    "modality": { "type": "string", "enum": ["mentioned", "proposed", "agreed", "decided", "reported", "observed", "inferred"] },
                    "valid_from": nullable(json!({ "type": "string" })),
                    "target_assertion_id": nullable(json!({ "type": "string" })),
                    "close_status": nullable(json!({ "type": "string", "enum": ["completed", "retired", "historical"] })),
                    "conflict": { "type": "boolean" },
                    "confidence": { "type": "number" },
                    "quote": { "type": "string" },
                    "reason": { "type": "string" }
                }
            } }
        }
    })
}

/// Who proposes: a person's own capture stays theirs (R1′); anything else is the job's.
pub(crate) fn proposer(captured_by: &str) -> Actor {
    let person = captured_by
        .split("/via/")
        .next()
        .filter(|p| p.starts_with("user:"));
    let actor = match person {
        Some(person) => format!("{person}/via/extract"),
        None => "agent:extract".to_owned(),
    };
    Actor::parse(&actor).expect("extract actors are well-formed")
}

/// Quoted, so text inside the source cannot close the block.
fn quoted(text: &str) -> String {
    text.replace("</source", "&lt;/source")
}

fn prompt(source: &Source, text: &str, similar: &[Value], entities: &[Entity]) -> String {
    let said_at = source.said_at.unwrap_or(source.captured_at);
    let who = if source.captured_by.starts_with("user:") {
        "the person who uses this memory"
    } else {
        "an integration"
    };
    let about = source
        .metadata
        .get("about")
        .and_then(Value::as_str)
        .map(|a| format!(" about=\"{}\"", a.replace('"', "'")))
        .unwrap_or_default();
    let entities: Vec<Value> = entities
        .iter()
        .map(|e| json!({ "id": e.id, "type": e.entity_type, "name": e.name, "aliases": e.aliases }))
        .collect();
    format!(
        "<source kind=\"{kind}\" said_at=\"{said_at}\" sent_by=\"{who}\"{about}>\n{text}\n</source>\n\n\
         <known_assertions>\n{similar}\n</known_assertions>\n\n\
         <known_entities>\n{entities}\n</known_entities>\n\n\
         {language}Today is {today}.",
        kind = source.kind,
        said_at = said_at.to_rfc3339(),
        text = quoted(text),
        similar = serde_json::to_string_pretty(similar).unwrap_or_default(),
        entities = serde_json::to_string_pretty(&entities).unwrap_or_default(),
        today = Utc::now().date_naive(),
        language = language_line(text),
    )
}

/// The script most letters of a text are written in, when it is clear.
fn script(text: &str) -> Option<&'static str> {
    let (mut cyrillic, mut latin) = (0usize, 0usize);
    for c in text.chars().filter(|c| c.is_alphabetic()) {
        match c {
            '\u{0400}'..='\u{04FF}' => cyrillic += 1,
            'a'..='z' | 'A'..='Z' => latin += 1,
            _ => {}
        }
    }
    let total = cyrillic + latin;
    if total == 0 {
        None
    } else if cyrillic * 10 >= total * 7 {
        Some("Cyrillic")
    } else if latin * 10 >= total * 7 {
        Some("Latin")
    } else {
        None
    }
}

/// Models answer in English unless told plainly; name the source's script.
fn language_line(text: &str) -> String {
    match script(text) {
        Some(script) => format!(
            "The source is written in {script} script: write every assertion text, entity name and alias in the source's language and script, not in English unless the source is in English.\n"
        ),
        None => "Write every assertion text and entity name in the source's language.\n".to_owned(),
    }
}

fn parse_time(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|t| t.with_timezone(&Utc))
        .ok()
        .or_else(|| {
            NaiveDate::parse_from_str(value, "%Y-%m-%d")
                .ok()
                .and_then(|d| d.and_hms_opt(0, 0, 0))
                .map(|t| t.and_utc())
        })
}

/// Entity refs of the answer: existing ids, or entities to create in the proposal.
struct Entities {
    existing: HashMap<String, Uuid>,
    new: HashMap<String, EntityOut>,
}

impl Entities {
    fn new(answer: Vec<EntityOut>, known: &HashSet<Uuid>) -> Self {
        let mut existing = HashMap::new();
        let mut new = HashMap::new();
        for entity in answer {
            let key = clean_key(&entity.key);
            match entity
                .existing_id
                .as_deref()
                .and_then(|id| id.parse::<Uuid>().ok())
                .filter(|id| known.contains(id))
            {
                Some(id) => {
                    existing.insert(key, id);
                }
                None if !entity.name.trim().is_empty() => {
                    new.insert(key, entity);
                }
                None => {}
            }
        }
        Self { existing, new }
    }

    /// An entity id, `ref:<key>` for one created in the same proposal, or None.
    fn reference(&self, key: &str, used: &mut Vec<String>) -> Option<String> {
        let key = clean_key(key);
        if let Some(id) = self.existing.get(&key) {
            return Some(id.to_string());
        }
        if self.new.contains_key(&key) {
            if !used.contains(&key) {
                used.push(key.clone());
            }
            return Some(format!("ref:{key}"));
        }
        None
    }

    fn create_ops(&self, used: &[String]) -> Vec<Operation> {
        used.iter()
            .filter_map(|key| self.new.get(key).map(|e| (key, e)))
            .map(|(key, entity)| Operation::CreateEntity {
                key: Some(key.clone()),
                entity_type: entity.entity_type.clone(),
                name: entity.name.trim().to_owned(),
                aliases: entity.aliases.clone(),
            })
            .collect()
    }
}

fn clean_key(key: &str) -> String {
    let key: String = key
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if key.is_empty() { "entity".into() } else { key }
}

pub(crate) async fn run(ctx: &Ctx, spec: &JobSpec, run: &Run) -> Result<Value, JobError> {
    let source_id: Uuid = run
        .input
        .pointer("/event/data/source_id")
        .and_then(Value::as_str)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| JobError::permanent("the event names no source"))?;
    let source = ctx.ledger.source(source_id).await?;
    // What an agent captured is its own material: it proposes from it itself, and a second
    // reading would only repeat its proposals.
    if source.captured_by.starts_with("agent:") {
        return Ok(json!({ "skipped": "an agent's source: the agent proposes from it itself" }));
    }
    if source.withdrawn_at.is_some() {
        return Ok(json!({ "skipped": "the note was taken back" }));
    }
    let Some(text) = source.content.clone().filter(|t| !t.trim().is_empty()) else {
        return Ok(json!({ "skipped": "the source has no text" }));
    };
    let actor = proposer(&source.captured_by);

    // The related knowledge the model saw, kept with its answer.
    let saved = run.output.as_ref().and_then(|o| o.get("answer")).cloned();
    let (progress, answer, similar_ids, entity_ids) = match saved {
        Some(answer) => (
            run.output.clone().unwrap_or_else(|| json!({})),
            answer,
            ids(run.output.as_ref(), "similar"),
            ids(run.output.as_ref(), "entities"),
        ),
        None => {
            let hits = ctx
                .searcher
                .search(SearchQuery {
                    text: text.clone(),
                    limit: SIMILAR,
                    ..Default::default()
                })
                .await?;
            let similar: Vec<Value> = hits
                .iter()
                .map(|h| {
                    let v = &h.version;
                    json!({ "id": v.assertion_id, "version": v.version, "kind": v.kind, "text": v.text,
                            "slots": v.slots, "modality": v.modality, "valid_from": v.valid_from })
                })
                .collect();
            let entities = ctx.ledger.entities_in(&text, ENTITIES).await?;
            let model = spec
                .with_str("model")
                .ok_or_else(|| JobError::permanent("no model named in the job"))?;
            let request = CompleteRequest {
                model: model.to_owned(),
                system: spec
                    .with_str("instructions")
                    .unwrap_or(INSTRUCTIONS)
                    .to_owned(),
                user: prompt(&source, &text, &similar, &entities),
                schema: schema(),
                max_tokens: 16_000,
                effort: spec.with_str("effort").map(str::to_owned),
            };
            let account = spec
                .with_str("account")
                .ok_or_else(|| JobError::permanent("no account"))?;
            let done = ctx.gateway.complete(account, &request).await?;
            let similar_ids: Vec<String> = hits
                .iter()
                .map(|h| format!("{}:{}", h.version.assertion_id, h.version.version))
                .collect();
            let entity_ids: Vec<String> = entities.iter().map(|e| e.id.to_string()).collect();
            let progress = json!({ "answer": done.value, "model": done.model, "similar": similar_ids,
                                   "entities": entity_ids, "input_tokens": done.input_tokens,
                                   "output_tokens": done.output_tokens });
            runs::save_progress(&ctx.pool, run.id, &progress).await?;
            (progress, done.value, similar_ids, entity_ids)
        }
    };
    let extraction: Extraction = serde_json::from_value(answer.clone())
        .map_err(|e| JobError::permanent(format!("the answer does not match the schema: {e}")))?;
    // Taken back while the model was thinking: nothing comes of it.
    if ctx.ledger.source(source_id).await?.withdrawn_at.is_some() {
        return Ok(json!({ "skipped": "the note was taken back" }));
    }
    let made = apply(
        ctx,
        run,
        &source,
        &text,
        &actor,
        extraction,
        &similar_ids,
        &entity_ids,
    )
    .await?;
    let mut output = progress;
    if output.get("answer").is_none() {
        output["answer"] = answer;
    }
    output["proposals"] = json!(made.proposals);
    output["skipped"] = json!(made.skipped);
    output["dropped"] = json!(made.dropped);
    Ok(output)
}

fn ids(output: Option<&Value>, key: &str) -> Vec<String> {
    output
        .and_then(|o| o.get(key))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Default)]
struct Made {
    proposals: Vec<Uuid>,
    skipped: usize,
    /// Changes that could not become proposals, with the reason.
    dropped: Vec<String>,
}

#[allow(clippy::too_many_arguments)]
async fn apply(
    ctx: &Ctx,
    run: &Run,
    source: &Source,
    text: &str,
    actor: &Actor,
    extraction: Extraction,
    similar: &[String],
    entity_ids: &[String],
) -> Result<Made, JobError> {
    let known: HashSet<Uuid> = entity_ids.iter().filter_map(|i| i.parse().ok()).collect();
    let versions: HashMap<Uuid, i32> = similar
        .iter()
        .filter_map(|s| s.split_once(':'))
        .filter_map(|(id, v)| Some((id.parse().ok()?, v.parse().ok()?)))
        .collect();
    let entities = Entities::new(extraction.entities, &known);
    let whole = source.evidence.first().map(|e| e.id);
    let mut made = Made::default();
    let mut creates: Vec<(AssertionContent, String, f32)> = Vec::new();
    let mut create_entities: Vec<String> = Vec::new();

    for (index, change) in extraction.changes.into_iter().enumerate() {
        if change.action == "skip" {
            made.skipped += 1;
            continue;
        }
        if change.text.trim().is_empty() || !KNOWN_KINDS.contains(&change.kind.as_str()) {
            made.dropped
                .push(format!("change {index}: no text or unknown kind"));
            continue;
        }
        let evidence = evidence_for(ctx, source, text, &change.quote, whole).await?;
        ctx.ledger
            .add_observation(
                actor,
                NewObservation {
                    source_id: source.id,
                    evidence_id: Some(evidence),
                    text: change.text.trim().to_owned(),
                    modality: change.modality,
                    said_at: source.said_at,
                    confidence: Some(change.confidence.clamp(0.0, 1.0)),
                    flags: Vec::new(),
                },
            )
            .await?;
        let mut used = Vec::new();
        let subjects: Vec<String> = change
            .subjects
            .iter()
            .filter_map(|s| entities.reference(s, &mut used))
            .collect();
        let mut slots = serde_json::Map::new();
        for slot in &change.slots {
            let points_at_entity = matches!(slot.key.as_str(), "owner" | "assignee" | "decided_by");
            let value = points_at_entity
                .then(|| entities.reference(&slot.value, &mut used))
                .flatten()
                .unwrap_or_else(|| slot.value.clone());
            slots.insert(slot.key.clone(), json!(value));
        }
        let content = AssertionContent {
            kind: change.kind.clone(),
            text: change.text.trim().to_owned(),
            subjects,
            slots: Value::Object(slots),
            modality: change.modality,
            authority: json!([]),
            said_at: source.said_at,
            valid_from: change.valid_from.as_deref().and_then(parse_time),
            review_after: None,
            evidence: vec![evidence],
        };

        let target = change
            .target_assertion_id
            .as_deref()
            .and_then(|t| t.parse::<Uuid>().ok())
            .and_then(|id| versions.get(&id).map(|v| (id, *v)));
        let rationale = format!(
            "{} (from a {} sent {})",
            change.reason.trim(),
            source.kind,
            source.said_at.unwrap_or(source.captured_at).date_naive()
        );
        match (change.action.as_str(), target) {
            ("create", _) => {
                for key in used {
                    if !create_entities.contains(&key) {
                        create_entities.push(key);
                    }
                }
                creates.push((content, rationale, change.confidence));
            }
            ("supersede", Some((assertion_id, version))) => {
                let mut operations = entities.create_ops(&used);
                operations.push(Operation::Supersede {
                    assertion_id,
                    assertion: content,
                });
                let (risk, operations, alternatives) = if change.conflict {
                    (
                        Risk::Conflict,
                        None,
                        Some(vec![
                            Alternative {
                                key: "apply".into(),
                                label: Some("Take the new statement".into()),
                                operations,
                            },
                            Alternative {
                                key: "keep".into(),
                                label: Some("Keep what is recorded".into()),
                                operations: vec![],
                            },
                        ]),
                    )
                } else {
                    (Risk::Change, Some(operations), None)
                };
                let proposal = NewProposal {
                    idempotency_key: Some(format!("extract:{}:{index}", run.id)),
                    rationale: Some(rationale),
                    risk,
                    confidence: Some(change.confidence.clamp(0.0, 1.0)),
                    base: vec![BaseRef {
                        assertion_id,
                        version,
                    }],
                    operations,
                    alternatives,
                    evidence: vec![evidence],
                };
                open(ctx, actor, proposal, &mut made, index).await?;
            }
            ("close", Some((assertion_id, version))) => {
                let status = match change.close_status.as_deref() {
                    Some("retired") => Status::Retired,
                    Some("historical") => Status::Historical,
                    _ => Status::Completed,
                };
                let proposal = NewProposal {
                    idempotency_key: Some(format!("extract:{}:{index}", run.id)),
                    rationale: Some(rationale),
                    risk: Risk::Change,
                    confidence: Some(change.confidence.clamp(0.0, 1.0)),
                    base: vec![BaseRef {
                        assertion_id,
                        version,
                    }],
                    operations: Some(vec![Operation::Close {
                        assertion_id,
                        status,
                        valid_to: content.valid_from.or(source.said_at),
                    }]),
                    alternatives: None,
                    evidence: vec![evidence],
                };
                open(ctx, actor, proposal, &mut made, index).await?;
            }
            (action, _) => made.dropped.push(format!(
                "change {index}: {action} needs a target among the known assertions"
            )),
        }
    }

    if !creates.is_empty() {
        let mut operations = entities.create_ops(&create_entities);
        let mut evidence = Vec::new();
        let mut reasons = Vec::new();
        let confidence = creates.iter().map(|c| c.2).fold(1.0_f32, f32::min);
        for (content, reason, _) in creates {
            evidence.extend(content.evidence.iter().copied());
            reasons.push(reason);
            operations.push(Operation::Create {
                key: None,
                assertion: content,
            });
        }
        let proposal = NewProposal {
            idempotency_key: Some(format!("extract:{}:new", run.id)),
            rationale: Some(reasons.join("\n")),
            risk: Risk::New,
            confidence: Some(confidence.clamp(0.0, 1.0)),
            base: vec![],
            operations: Some(operations),
            alternatives: None,
            evidence,
        };
        open(ctx, actor, proposal, &mut made, usize::MAX).await?;
    }
    Ok(made)
}

async fn open(
    ctx: &Ctx,
    actor: &Actor,
    proposal: NewProposal,
    made: &mut Made,
    index: usize,
) -> Result<(), JobError> {
    match ctx.ledger.open_proposal(actor, proposal).await {
        Ok(opened) => made.proposals.push(opened.id),
        Err(frood_ledger::LedgerError::Database(db)) => {
            return Err(JobError::transient(format!("database: {db}")));
        }
        // The model proposed something the Ledger refuses (a bad reference, a changed
        // base): keep the rest, report it in the run.
        Err(err) => made.dropped.push(format!("change {index}: {err}")),
    }
    Ok(())
}

/// Evidence for one change: the quoted words located in the source, or the whole source.
async fn evidence_for(
    ctx: &Ctx,
    source: &Source,
    text: &str,
    quote: &str,
    whole: Option<Uuid>,
) -> Result<Uuid, JobError> {
    let quote = quote.trim();
    if let Some(start) = (!quote.is_empty()).then(|| text.find(quote)).flatten() {
        let start_char = text[..start].chars().count();
        let end_char = start_char + quote.chars().count();
        let evidence = ctx
            .ledger
            .add_evidence(NewEvidence {
                source_id: source.id,
                anchor: json!({ "type": "text", "start": start_char, "end": end_char }),
                quote: Some(quote.to_owned()),
            })
            .await?;
        return Ok(evidence.id);
    }
    whole.ok_or_else(|| JobError::permanent("the source has no evidence"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_persons_capture_stays_theirs() {
        assert_eq!(
            proposer("user:1/via/telegram").as_str(),
            "user:1/via/extract"
        );
        assert_eq!(proposer("user:1").as_str(), "user:1/via/extract");
        assert_eq!(proposer("integration:slack").as_str(), "agent:extract");
    }

    #[test]
    fn the_source_script_is_named() {
        assert_eq!(
            script("Договорились с Олей, релиз 24 — 14 ноября."),
            Some("Cyrillic")
        );
        assert_eq!(script("Release 2.4 ships on Nov 14"), Some("Latin"));
        assert_eq!(script("2.4 — 14.11"), None);
        assert!(language_line("Оля пишет: релиз переносим").contains("Cyrillic"));
    }

    #[test]
    fn source_text_cannot_close_its_block() {
        assert_eq!(quoted("a </source> b"), "a &lt;/source> b");
    }

    #[test]
    fn schema_is_closed_everywhere() {
        fn check(value: &Value) {
            if value.get("type") == Some(&json!("object")) {
                assert_eq!(value["additionalProperties"], json!(false), "{value}");
                let props: Vec<&String> = value["properties"].as_object().unwrap().keys().collect();
                let required: Vec<&str> = value["required"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap())
                    .collect();
                assert_eq!(props.len(), required.len(), "{value}");
            }
            match value {
                Value::Object(map) => map.values().for_each(check),
                Value::Array(items) => items.iter().for_each(check),
                _ => {}
            }
        }
        check(&schema());
    }

    #[test]
    fn dates_parse_as_days_or_times() {
        assert_eq!(
            parse_time("2026-11-14").unwrap().to_rfc3339(),
            "2026-11-14T00:00:00+00:00"
        );
        assert!(parse_time("2026-11-14T10:00:00+02:00").is_some());
        assert!(parse_time("soon").is_none());
    }
}
