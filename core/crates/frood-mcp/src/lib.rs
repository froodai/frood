//! MCP adapter of the Frood core (docs/12_api_mcp_auth.md §4): Streamable HTTP without
//! sessions, tools that wrap Ledger operations, the list filtered by the token's scopes.
//!
//! Authentication happens in front of this service: an HTTP middleware resolves the bearer
//! token and puts the [`Principal`] into the request extensions.

use std::sync::Arc;

use chrono::{DateTime, NaiveDate, Utc};
use frood_auth::{Principal, scope};
use frood_dispatch::Dispatch;
use frood_ledger::model::*;
use frood_ledger::{Ledger, LedgerError, ProposalContext, SearchQuery};
use frood_search::{SearchError, Searcher};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::tool::{Extension, ToolCallContext};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CacheScope, CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock,
    Implementation, ListToolsResult, PaginatedRequestParams, ProtocolVersion, ResultType,
    ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData, RoleServer, ServerHandler, tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

const INSTRUCTIONS: &str = "Frood is the team's memory: decisions, commitments, rules, \
responsibilities, facts and statuses, each with its sources and history. Search before \
answering and cite assertion ids and source ids. Never present something as known without a \
source in Frood. To remember something new, call capture_note; to change knowledge in a \
structured way, call propose_change. Proposals wait for a person's verdict unless a policy \
accepts them.";

/// Tools only an agent's run token gets.
const RUN: &str = "an agent's run";
/// Tools only a person gets (not a run token acting for them): answering for oneself.
const PERSON: &str = "a person's";

/// The scope each tool needs; tools whose scope the token lacks are not listed.
const TOOL_SCOPES: [(&str, &str); 14] = [
    ("search_knowledge", scope::READ),
    ("list_assertions", scope::READ),
    ("get_context", scope::READ),
    ("get_entity", scope::READ),
    ("get_history", scope::READ),
    ("list_inbox", scope::READ),
    ("get_proposal", scope::READ),
    ("capture_note", scope::CAPTURE),
    ("propose_change", scope::PROPOSE),
    ("submit_verdict", scope::DECIDE),
    ("list_questions", scope::READ),
    ("answer_question", PERSON),
    ("ask_user", RUN),
    ("report_progress", RUN),
];

/// The Streamable HTTP service to mount at `/mcp`. `allowed_hosts` guards against DNS
/// rebinding: the hosts this installation is reached by.
pub fn service(
    ledger: Ledger,
    searcher: Searcher,
    dispatch: Dispatch,
    allowed_hosts: Vec<String>,
) -> StreamableHttpService<FroodMcp, NeverSessionManager> {
    let config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_sse_keep_alive(None)
        .with_allowed_hosts(allowed_hosts);
    StreamableHttpService::new(
        move || {
            Ok(FroodMcp::new(
                ledger.clone(),
                searcher.clone(),
                dispatch.clone(),
            ))
        },
        Arc::new(NeverSessionManager::default()),
        config,
    )
}

#[derive(Clone)]
pub struct FroodMcp {
    ledger: Ledger,
    searcher: Searcher,
    dispatch: Dispatch,
    tool_router: ToolRouter<Self>,
}

type ToolResult = Result<String, String>;

fn principal(parts: &http::request::Parts) -> Result<Principal, String> {
    parts
        .extensions
        .get::<Principal>()
        .cloned()
        .ok_or_else(|| "the request is not authenticated".to_owned())
}

fn actor(principal: &Principal) -> Actor {
    Actor::parse(&principal.actor()).expect("principal actors are well-formed")
}

fn render(value: impl serde::Serialize) -> ToolResult {
    serde_json::to_string_pretty(&value).map_err(|err| err.to_string())
}

fn ledger_error(err: LedgerError) -> String {
    match err {
        LedgerError::Database(db) => {
            tracing::error!(error = %db, "database error in an MCP tool");
            "internal error; see server logs".to_owned()
        }
        other => other.to_string(),
    }
}

fn search_error(err: SearchError) -> String {
    match err {
        SearchError::Ledger(e) => ledger_error(e),
        SearchError::Llm(e) => e.to_string(),
    }
}

/// RFC 3339 timestamp, or a bare date meaning its start in UTC.
fn parse_time(value: &str) -> Result<DateTime<Utc>, String> {
    if let Ok(at) = DateTime::parse_from_rfc3339(value) {
        return Ok(at.with_timezone(&Utc));
    }
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .ok()
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|naive| naive.and_utc())
        .ok_or_else(|| format!("{value:?} is not an RFC 3339 time or YYYY-MM-DD date"))
}

fn parse_opt_time(value: Option<&str>) -> Result<Option<DateTime<Utc>>, String> {
    value.map(parse_time).transpose()
}

fn parse_uuid(value: &str, what: &str) -> Result<Uuid, String> {
    value
        .parse()
        .map_err(|_| format!("{what} {value:?} is not a valid id"))
}

#[derive(Deserialize, JsonSchema)]
pub struct SearchParams {
    /// Words to look for in assertion texts.
    pub query: String,
    /// Assertion kind: decision, commitment, rule, responsibility, fact, status, event, open_question.
    pub kind: Option<String>,
    /// Only assertions about this entity id.
    pub entity: Option<String>,
    /// active (default), superseded, completed, historical or retired.
    pub status: Option<String>,
    /// What was true at this time (RFC 3339 or YYYY-MM-DD); default now.
    pub valid_at: Option<String>,
    /// At most this many results (default 20, at most 100).
    pub limit: Option<i64>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ListParams {
    pub kind: Option<String>,
    pub entity: Option<String>,
    pub status: Option<String>,
    /// Only assertions whose `due` slot is on or after this date.
    pub due_from: Option<String>,
    /// Only assertions whose `due` slot is before this date.
    pub due_to: Option<String>,
    /// Only assertions whose `owner` slot is this entity id.
    pub owner: Option<String>,
    /// What was true at this time; default now.
    pub valid_at: Option<String>,
    /// What Frood knew at this time (its records as of then); default now.
    pub as_of: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ContextParams {
    /// A project, person or subject name to summarize.
    pub topic: Option<String>,
    /// Or an entity id.
    pub entity_id: Option<String>,
    /// What Frood knew at this time; default now.
    pub as_of: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct EntityParams {
    pub entity_id: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct HistoryParams {
    /// History of one assertion.
    pub assertion_id: Option<String>,
    /// Or of every assertion about an entity.
    pub entity_id: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct CaptureParams {
    /// What to remember, in the words of the person if possible.
    pub text: String,
    /// What it is about (a project or person), as a hint for later processing.
    pub about: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ProposeParams {
    /// Operations as in the Frood API (docs/11 §6), each an object with `op`: create
    /// {assertion: {kind, text, modality, subjects?, slots?, said_at?, valid_from?, evidence?}},
    /// supersede {assertion_id, assertion}, close {assertion_id, status, valid_to?},
    /// reverify {assertion_id}, link {from, to, kind}, create_entity {ref?, type, name, aliases?}.
    /// kind: decision, commitment, rule, responsibility, fact, status, event or open_question.
    /// modality: mentioned, proposed, agreed, decided, reported, observed or inferred.
    /// subjects: entity ids, or `ref:<key>` of a create_entity earlier in the same list.
    /// slots: an object such as {"due": "2026-11-14", "owner": "<entity id>"}.
    /// A supersede or close needs the assertion and its version in `base`.
    pub operations: Vec<serde_json::Map<String, Value>>,
    /// Why this change: shown to the person who decides.
    pub rationale: String,
    /// The words this change is based on (a quote from the conversation). Stored as a source and
    /// used as evidence for assertions that name none.
    pub source_text: Option<String>,
    /// Evidence ids (from captured sources) for the whole proposal.
    pub evidence: Option<Vec<String>>,
    /// Current versions the change is based on: [{assertion_id, version}].
    pub base: Option<Vec<serde_json::Map<String, Value>>>,
    /// new (default), change, conflict, stale_knowledge or entity_merge.
    pub risk: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ProposalParams {
    pub proposal_id: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct VerdictParams {
    pub proposal_id: String,
    /// accept, accept_with_edits, reject, defer or investigate.
    pub verdict: String,
    /// Key of the alternative to accept when the proposal has several.
    pub alternative: Option<String>,
    /// Edited operations for accept_with_edits.
    pub edits: Option<Vec<serde_json::Map<String, Value>>>,
    pub reason: Option<String>,
    /// For defer: until when (RFC 3339 or YYYY-MM-DD).
    pub deferred_until: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct AskParams {
    /// One short question.
    pub question: String,
    /// Answers to choose from, if the question has them.
    pub options: Option<Vec<String>>,
    /// What the person needs to know to answer: the quote, the link, what you found.
    pub context: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ProgressParams {
    /// One line on what was done.
    pub message: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct QuestionsParams {
    /// open (default), answered, expired or cancelled.
    pub status: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Deserialize, JsonSchema)]
pub struct AnswerParams {
    pub question_id: String,
    pub answer: String,
}

fn run_of(principal: &Principal) -> Result<Uuid, String> {
    principal
        .run
        .as_ref()
        .map(|run| run.id)
        .ok_or_else(|| "only an agent's run can do this".to_owned())
}

fn status(value: Option<&str>) -> Result<Option<Status>, String> {
    value.map(str::parse).transpose()
}

fn limit(value: Option<i64>, default: i64) -> i64 {
    value.unwrap_or(default).clamp(1, 100)
}

fn operations(values: Vec<serde_json::Map<String, Value>>) -> Result<Vec<Operation>, String> {
    values
        .into_iter()
        .enumerate()
        .map(|(i, op)| {
            let mut op = Value::Object(op);
            // `evidence` may be left out here; it is filled in from source_text.
            if let Some(assertion) = op.get_mut("assertion").and_then(Value::as_object_mut) {
                assertion.entry("evidence").or_insert_with(|| json!([]));
            }
            serde_json::from_value(op).map_err(|err| format!("operation {}: {err}", i + 1))
        })
        .collect()
}

#[tool_router]
impl FroodMcp {
    pub fn new(ledger: Ledger, searcher: Searcher, dispatch: Dispatch) -> Self {
        Self {
            ledger,
            searcher,
            dispatch,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "Search knowledge by words and meaning, across languages. Returns current assertions with their ids, kinds, subjects, slots, validity, a relevance score and the sources they rest on (source id, kind, when said, the quoted words) to cite.",
        annotations(read_only_hint = true)
    )]
    async fn search_knowledge(&self, Parameters(p): Parameters<SearchParams>) -> ToolResult {
        let query = SearchQuery {
            text: p.query,
            embedding: None,
            kind: p.kind,
            entity: p
                .entity
                .as_deref()
                .map(|e| parse_uuid(e, "entity"))
                .transpose()?,
            status: status(p.status.as_deref())?,
            valid_at: parse_opt_time(p.valid_at.as_deref())?,
            limit: limit(p.limit, 20),
        };
        let hits = self.searcher.search(query).await.map_err(search_error)?;
        render(self.with_sources(hits).await?)
    }

    /// Search hits with what each rests on — the source, its kind, when it was said, the quoted
    /// words — so an answer can cite them, not only an evidence id.
    async fn with_sources(
        &self,
        hits: Vec<frood_ledger::SearchHit>,
    ) -> Result<Vec<serde_json::Value>, String> {
        let ids: Vec<Uuid> = hits
            .iter()
            .flat_map(|h| h.version.evidence.iter().copied())
            .collect();
        let evidence = self.ledger.evidence(&ids).await.map_err(ledger_error)?;
        let mut sources = std::collections::HashMap::new();
        for e in &evidence {
            if !sources.contains_key(&e.source_id)
                && let Ok(source) = self.ledger.source(e.source_id).await
            {
                sources.insert(e.source_id, source);
            }
        }
        Ok(hits
            .into_iter()
            .map(|hit| {
                let cites: Vec<serde_json::Value> = evidence
                    .iter()
                    .filter(|e| hit.version.evidence.contains(&e.id))
                    .map(|e| {
                        let source = sources.get(&e.source_id);
                        json!({
                            "source_id": e.source_id,
                            "kind": source.map(|s| s.kind.as_str()),
                            "title": source.and_then(|s| s.title.as_deref()),
                            "uri": source.and_then(|s| s.uri.as_deref()),
                            "said_at": source.and_then(|s| s.said_at),
                            "captured_by": source.map(|s| s.captured_by.as_str()),
                            "quote": e.quote.as_deref().or_else(|| source.and_then(|s| s.content.as_deref())),
                        })
                    })
                    .collect();
                let mut value = serde_json::to_value(&hit).unwrap_or_default();
                value["sources"] = json!(cites);
                value
            })
            .collect())
    }

    #[tool(
        description = "List assertions by structure: kind, entity, status, due dates, owner, and points in time (valid_at: what was true then; as_of: what Frood knew then).",
        annotations(read_only_hint = true)
    )]
    async fn list_assertions(&self, Parameters(p): Parameters<ListParams>) -> ToolResult {
        let mut slots = Vec::new();
        if let Some(from) = p.due_from {
            slots.push(SlotFilter {
                key: "due".into(),
                op: SlotOp::Gte,
                value: from,
            });
        }
        if let Some(to) = p.due_to {
            slots.push(SlotFilter {
                key: "due".into(),
                op: SlotOp::Lt,
                value: to,
            });
        }
        if let Some(owner) = p.owner {
            slots.push(SlotFilter {
                key: "owner".into(),
                op: SlotOp::Eq,
                value: owner,
            });
        }
        let filter = AssertionFilter {
            kind: p.kind,
            entity: p
                .entity
                .as_deref()
                .map(|e| parse_uuid(e, "entity"))
                .transpose()?,
            status: status(p.status.as_deref())?,
            valid_at: parse_opt_time(p.valid_at.as_deref())?,
            as_of: parse_opt_time(p.as_of.as_deref())?,
            slots,
            limit: Some(limit(p.limit, 50)),
            ..Default::default()
        };
        render(
            self.ledger
                .assertions(&filter)
                .await
                .map_err(ledger_error)?,
        )
    }

    #[tool(
        description = "Summary of a project, person or subject: matching entities with their current assertions, and assertions that mention the topic.",
        annotations(read_only_hint = true)
    )]
    async fn get_context(&self, Parameters(p): Parameters<ContextParams>) -> ToolResult {
        let as_of = parse_opt_time(p.as_of.as_deref())?;
        if let Some(id) = p.entity_id {
            let id = parse_uuid(&id, "entity")?;
            let entity = self.ledger.entity(id).await.map_err(ledger_error)?;
            let filter = AssertionFilter {
                entity: Some(id),
                as_of,
                limit: Some(100),
                ..Default::default()
            };
            let assertions = self
                .ledger
                .assertions(&filter)
                .await
                .map_err(ledger_error)?;
            return render(json!({ "entity": entity, "assertions": assertions }));
        }
        let Some(topic) = p.topic.filter(|t| !t.trim().is_empty()) else {
            return Err("give a topic or an entity_id".into());
        };
        let entities = self
            .ledger
            .entities(Some(&topic), None, 5)
            .await
            .map_err(ledger_error)?;
        let mut about = Vec::new();
        for entity in &entities {
            let filter = AssertionFilter {
                entity: Some(entity.id),
                as_of,
                limit: Some(50),
                ..Default::default()
            };
            let assertions = self
                .ledger
                .assertions(&filter)
                .await
                .map_err(ledger_error)?;
            about.push(json!({ "entity": entity, "assertions": assertions }));
        }
        let filter = AssertionFilter {
            text: Some(topic),
            as_of,
            limit: Some(30),
            ..Default::default()
        };
        let mentions = self
            .ledger
            .assertions(&filter)
            .await
            .map_err(ledger_error)?;
        render(json!({ "entities": about, "mentions": mentions }))
    }

    #[tool(
        description = "An entity (person, project, team, …) with its aliases and merge history.",
        annotations(read_only_hint = true)
    )]
    async fn get_entity(&self, Parameters(p): Parameters<EntityParams>) -> ToolResult {
        let id = parse_uuid(&p.entity_id, "entity")?;
        render(self.ledger.entity(id).await.map_err(ledger_error)?)
    }

    #[tool(
        description = "How knowledge changed: every recorded version of an assertion, or of each assertion about an entity, with validity and record times.",
        annotations(read_only_hint = true)
    )]
    async fn get_history(&self, Parameters(p): Parameters<HistoryParams>) -> ToolResult {
        if let Some(id) = p.assertion_id {
            let id = parse_uuid(&id, "assertion")?;
            return render(self.ledger.history(id).await.map_err(ledger_error)?);
        }
        let Some(entity) = p.entity_id else {
            return Err("give an assertion_id or an entity_id".into());
        };
        let entity = parse_uuid(&entity, "entity")?;
        let mut histories = Vec::new();
        for status in [
            Status::Active,
            Status::Superseded,
            Status::Completed,
            Status::Historical,
            Status::Retired,
        ] {
            let filter = AssertionFilter {
                entity: Some(entity),
                status: Some(status),
                limit: Some(50),
                ..Default::default()
            };
            for version in self
                .ledger
                .assertions(&filter)
                .await
                .map_err(ledger_error)?
            {
                if histories.iter().any(|(id, _)| *id == version.assertion_id) {
                    continue;
                }
                let history = self
                    .ledger
                    .history(version.assertion_id)
                    .await
                    .map_err(ledger_error)?;
                histories.push((version.assertion_id, history));
            }
        }
        render(
            histories
                .into_iter()
                .map(|(id, versions)| json!({ "assertion_id": id, "versions": versions }))
                .collect::<Vec<_>>(),
        )
    }

    #[tool(
        description = "What waits for a person: proposals to decide, agents' questions, reminders of due dates, failed runs. Each item says what happened and what can be done; the person's own items first.",
        annotations(read_only_hint = true)
    )]
    async fn list_inbox(&self, Extension(parts): Extension<http::request::Parts>) -> ToolResult {
        let user = principal(&parts)?.user_id();
        fn db(err: impl std::fmt::Display) -> String {
            tracing::error!(error = %err, "database error in list_inbox");
            "internal error; see server logs".to_owned()
        }
        let items = self.dispatch.inbox(false, 50).await.map_err(db)?;
        let mut shown = Vec::with_capacity(items.len());
        for item in items {
            let for_me = user.is_some_and(|u| item.for_users.contains(&u));
            let entry = self.dispatch.inbox_shown(item).await.map_err(db)?;
            shown.push((for_me, entry));
        }
        shown.sort_by_key(|(for_me, _)| !*for_me);
        render(
            shown
                .into_iter()
                .map(|(for_me, s)| json!({
                    "id": s.item.id, "kind": s.item.kind, "urgency": s.item.urgency, "for_me": for_me,
                    "created_at": s.item.created_at, "content": s.content, "investigation": s.item.investigation,
                    "actions": s.actions,
                }))
                .collect::<Vec<_>>(),
        )
    }

    #[tool(
        description = "A proposal with its alternatives, operations, evidence, state and verdicts.",
        annotations(read_only_hint = true)
    )]
    async fn get_proposal(&self, Parameters(p): Parameters<ProposalParams>) -> ToolResult {
        let id = parse_uuid(&p.proposal_id, "proposal")?;
        render(self.ledger.proposal(id).await.map_err(ledger_error)?)
    }

    #[tool(
        description = "Remember something: stores the note as a source in Frood and returns its id and evidence id."
    )]
    async fn capture_note(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(p): Parameters<CaptureParams>,
    ) -> ToolResult {
        let principal = principal(&parts)?;
        let metadata = match &p.about {
            Some(about) => json!({ "about": about, "via": "mcp" }),
            None => json!({ "via": "mcp" }),
        };
        let input = NewSource {
            kind: "note".into(),
            origin: "capture".into(),
            uri: None,
            title: None,
            content: p.text,
            said_at: Some(Utc::now()),
            metadata,
        };
        let captured = self
            .ledger
            .capture_source(&actor(&principal), input)
            .await
            .map_err(ledger_error)?;
        let evidence = captured.source.evidence.first().map(|e| e.id);
        render(json!({
            "source_id": captured.source.id,
            "evidence_id": evidence,
            "already_known": !captured.created,
            "next": "The note is stored. Automatic extraction into knowledge is not enabled on this installation yet; to record structured knowledge now, call propose_change with this evidence_id.",
        }))
    }

    #[tool(
        description = "Propose a change to knowledge. It waits for a person's verdict unless a policy accepts it. Returns the proposal with its state."
    )]
    async fn propose_change(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(p): Parameters<ProposeParams>,
    ) -> ToolResult {
        let principal = principal(&parts)?;
        let actor = actor(&principal);
        // An agent's run: the source it reads goes with the quote, and may let a policy accept.
        let (run_source, context) = match &principal.run {
            Some(run) => (
                self.dispatch.run_source(run.id).await,
                ProposalContext {
                    authoritative_source: self.dispatch.authoritative_source(run.id).await,
                },
            ),
            None => (None, ProposalContext::default()),
        };
        let mut operations = operations(p.operations)?;
        let mut evidence: Vec<Uuid> = p
            .evidence
            .unwrap_or_default()
            .iter()
            .map(|e| parse_uuid(e, "evidence"))
            .collect::<Result<_, _>>()?;
        let needs_evidence = operations.iter().any(|op| match op {
            Operation::Create { assertion, .. } | Operation::Supersede { assertion, .. } => {
                assertion.evidence.is_empty()
            }
            _ => false,
        });
        if needs_evidence {
            let quoted = match (&p.source_text, evidence.first()) {
                (Some(text), _) => {
                    let input = NewSource {
                        kind: "conversation".into(),
                        origin: "capture".into(),
                        uri: None,
                        title: None,
                        content: text.clone(),
                        said_at: Some(Utc::now()),
                        metadata: match (&principal.run, &run_source) {
                            (Some(run), Some(source)) => {
                                json!({ "via": "mcp", "run_id": run.id, "source": source })
                            }
                            (Some(run), None) => json!({ "via": "mcp", "run_id": run.id }),
                            _ => json!({ "via": "mcp" }),
                        },
                    };
                    let captured = self
                        .ledger
                        .capture_source(&actor, input)
                        .await
                        .map_err(ledger_error)?;
                    let id = captured
                        .source
                        .evidence
                        .first()
                        .map(|e| e.id)
                        .ok_or("the source has no evidence")?;
                    evidence.push(id);
                    id
                }
                (None, Some(first)) => *first,
                (None, None) => {
                    return Err("assertions need evidence: pass source_text (the words this is based on) or evidence ids".into());
                }
            };
            for op in &mut operations {
                if let Operation::Create { assertion, .. } | Operation::Supersede { assertion, .. } =
                    op
                    && assertion.evidence.is_empty()
                {
                    assertion.evidence.push(quoted);
                }
            }
        }
        let base = p
            .base
            .unwrap_or_default()
            .into_iter()
            .map(|b| serde_json::from_value(Value::Object(b)).map_err(|err| format!("base: {err}")))
            .collect::<Result<_, _>>()?;
        let input = NewProposal {
            idempotency_key: None,
            rationale: Some(p.rationale),
            risk: p.risk.as_deref().unwrap_or("new").parse()?,
            confidence: None,
            base,
            operations: Some(operations),
            alternatives: None,
            evidence,
        };
        render(
            self.ledger
                .open_proposal_in(&actor, input, &context)
                .await
                .map_err(ledger_error)?,
        )
    }

    #[tool(
        description = "Decide on a proposal on the person's behalf: accept, accept_with_edits, reject, defer or investigate. Only offered when the person granted this permission."
    )]
    async fn submit_verdict(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(p): Parameters<VerdictParams>,
    ) -> ToolResult {
        let principal = principal(&parts)?;
        let id = parse_uuid(&p.proposal_id, "proposal")?;
        let input = VerdictInput {
            decision: p.verdict.parse()?,
            alternative: p.alternative,
            operations: p.edits.map(operations).transpose()?,
            reason: p.reason,
            deferred_until: parse_opt_time(p.deferred_until.as_deref())?,
        };
        render(
            self.ledger
                .submit_verdict(&actor(&principal), id, input)
                .await
                .map_err(ledger_error)?,
        )
    }

    #[tool(
        description = "Ask a person a question when you cannot decide yourself and it matters (for example, whether something is a decision or only a discussion). After asking, end your turn: the answer arrives as the next message in this session. Do not ask what you can look up."
    )]
    async fn ask_user(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(p): Parameters<AskParams>,
    ) -> ToolResult {
        let run = run_of(&principal(&parts)?)?;
        let question = self
            .dispatch
            .ask_user(
                run,
                &p.question,
                &p.options.unwrap_or_default(),
                p.context.as_deref(),
            )
            .await?;
        render(json!({
            "question_id": question.id,
            "expires_at": question.expires_at,
            "next": "Asked. End your turn now; the answer will come as the next message.",
        }))
    }

    #[tool(
        description = "Note progress of this run in one line: what part of the work is done. People see it in the run's log."
    )]
    async fn report_progress(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(p): Parameters<ProgressParams>,
    ) -> ToolResult {
        let run = run_of(&principal(&parts)?)?;
        self.dispatch
            .report_progress(run, &p.message)
            .await
            .map_err(|err| {
                tracing::error!(error = %err, "database error in report_progress");
                "internal error; see server logs".to_owned()
            })?;
        Ok("Noted.".into())
    }

    #[tool(
        description = "Questions agents asked people, newest first: open ones by default.",
        annotations(read_only_hint = true)
    )]
    async fn list_questions(&self, Parameters(p): Parameters<QuestionsParams>) -> ToolResult {
        let status = p.status.unwrap_or_else(|| "open".into());
        render(
            self.dispatch
                .questions(Some(&status), limit(p.limit, 50))
                .await
                .map_err(|err| {
                    tracing::error!(error = %err, "database error in list_questions");
                    "internal error; see server logs".to_owned()
                })?,
        )
    }

    #[tool(
        description = "Answer an agent's question in the person's words; the agent continues in the same session."
    )]
    async fn answer_question(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(p): Parameters<AnswerParams>,
    ) -> ToolResult {
        let principal = principal(&parts)?;
        let id = parse_uuid(&p.question_id, "question")?;
        render(
            self.dispatch
                .answer_question(id, &p.answer, &principal.actor())
                .await?,
        )
    }
}

impl FroodMcp {
    fn allowed(context: &RequestContext<RoleServer>, tool: &str) -> bool {
        let Some(principal) = context
            .extensions
            .get::<http::request::Parts>()
            .and_then(|parts| parts.extensions.get::<Principal>())
        else {
            return false;
        };
        TOOL_SCOPES
            .iter()
            .find(|(name, _)| *name == tool)
            .is_some_and(|(_, needed)| match *needed {
                RUN => principal.run.is_some(),
                PERSON => principal.user_id().is_some() && principal.has(scope::CAPTURE),
                needed => principal.has(needed),
            })
    }
}

impl ServerHandler for FroodMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("frood", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let supports_cache_hints = context
            .protocol_version()
            .is_some_and(|version| version >= ProtocolVersion::V_2026_07_28);
        let tools: Vec<Tool> = self
            .tool_router
            .list_all()
            .into_iter()
            .filter(|tool| Self::allowed(&context, &tool.name))
            .collect();
        Ok(ListToolsResult {
            result_type: Some(ResultType::COMPLETE),
            tools,
            meta: None,
            next_cursor: None,
            ttl_ms: supports_cache_hints.then_some(0),
            // The list depends on the token's scopes.
            cache_scope: supports_cache_hints.then_some(CacheScope::Private),
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        if !Self::allowed(&context, &request.name) {
            let needed = TOOL_SCOPES
                .iter()
                .find(|(name, _)| *name == request.name)
                .map(|(_, scope)| *scope);
            return match needed {
                Some(RUN) => Ok(CallToolResult::error(vec![ContentBlock::text(
                    "Only an agent's run can call this tool.",
                )])
                .into()),
                Some(PERSON) => Ok(CallToolResult::error(vec![ContentBlock::text(
                    "Only a person can answer questions, with the knowledge:capture permission.",
                )])
                .into()),
                Some(needed) => Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                    "This connection was not granted the {needed} permission. The person can reconnect Frood and grant it on the consent page."
                ))])
                .into()),
                None => Err(ErrorData::invalid_params("tool not found", None)),
            };
        }
        let tcc = ToolCallContext::new(self, request, context);
        self.tool_router.call(tcc).await
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tool_router.get(name).cloned()
    }
}
