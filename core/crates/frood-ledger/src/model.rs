//! Ledger types (docs/11_knowledge_model.md v0.2). They double as the JSON shapes of the API.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type Timestamp = DateTime<Utc>;

/// Who acts: `user:<id>`, `agent:<name>`, `integration:<name>`, `policy:<name>` or `system:<name>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Actor(String);

impl Actor {
    const KINDS: [&'static str; 5] = ["user", "agent", "integration", "policy", "system"];

    pub fn parse(value: &str) -> Result<Self, String> {
        let (kind, name) = value
            .split_once(':')
            .ok_or_else(|| format!("actor {value:?} must look like kind:name"))?;
        if !Self::KINDS.contains(&kind) {
            return Err(format!(
                "actor kind {kind:?} must be one of {}",
                Self::KINDS.join(", ")
            ));
        }
        if name.is_empty() || name.len() > 200 || name.chars().any(char::is_whitespace) {
            return Err(format!(
                "actor name in {value:?} must be 1–200 characters without spaces"
            ));
        }
        Ok(Self(value.to_owned()))
    }

    pub fn policy(name: &str) -> Self {
        Self(format!("policy:{name}"))
    }

    pub fn is_user(&self) -> bool {
        self.0.starts_with("user:")
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Actor {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<Actor> for String {
    fn from(actor: Actor) -> Self {
        actor.0
    }
}

impl fmt::Display for Actor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

macro_rules! text_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),+ }

        impl $name {
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $text),+ }
            }
        }

        impl std::str::FromStr for $name {
            type Err = String;
            fn from_str(value: &str) -> Result<Self, Self::Err> {
                match value {
                    $($text => Ok(Self::$variant),)+
                    other => Err(format!(concat!("unknown ", stringify!($name), " {:?}"), other)),
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

text_enum!(
    /// How firmly a statement holds: discussed, proposed, agreed, decided, reported, observed, inferred.
    Modality {
        Mentioned => "mentioned",
        Proposed => "proposed",
        Agreed => "agreed",
        Decided => "decided",
        Reported => "reported",
        Observed => "observed",
        Inferred => "inferred",
    }
);

text_enum!(
    /// Lifecycle of an assertion version.
    Status {
        Active => "active",
        Superseded => "superseded",
        Completed => "completed",
        Historical => "historical",
        Retired => "retired",
    }
);

text_enum!(
    Risk {
        New => "new",
        Change => "change",
        Conflict => "conflict",
        StaleKnowledge => "stale_knowledge",
        EntityMerge => "entity_merge",
    }
);

text_enum!(
    ProposalState {
        Open => "open",
        Accepted => "accepted",
        Rejected => "rejected",
        Deferred => "deferred",
        Stale => "stale",
    }
);

text_enum!(
    Decision {
        Accept => "accept",
        AcceptWithEdits => "accept_with_edits",
        Reject => "reject",
        Defer => "defer",
        Investigate => "investigate",
        Undo => "undo",
    }
);

text_enum!(
    LinkKind {
        Contradicts => "contradicts",
        DependsOn => "depends_on",
        PartOf => "part_of",
        DerivedFrom => "derived_from",
        RelatesTo => "relates_to",
    }
);

text_enum!(
    Flag {
        Contested => "contested",
        NeedsVerification => "needs_verification",
    }
);

/// Assertion kinds of v1 (docs/11 §3). Modules will be able to register more.
pub const KNOWN_KINDS: [&str; 8] = [
    "decision",
    "commitment",
    "rule",
    "responsibility",
    "fact",
    "status",
    "event",
    "open_question",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewSource {
    pub kind: String,
    /// `capture` for what a user sends; `source:<name>` for connectors.
    #[serde(default = "default_origin")]
    pub origin: String,
    #[serde(default)]
    pub uri: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    /// The text. May be empty when a file comes with the capture (a voice message).
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub said_at: Option<Timestamp>,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

/// A file that comes with a capture: a voice message, a recording, a document.
#[derive(Debug, Clone)]
pub struct NewBlob {
    pub content_type: String,
    pub filename: Option<String>,
    pub data: Vec<u8>,
}

impl NewBlob {
    /// Audio or video that needs transcribing.
    pub fn is_media(&self) -> bool {
        is_media_type(&self.content_type)
    }
}

pub fn is_media_type(content_type: &str) -> bool {
    let t = content_type.to_ascii_lowercase();
    t.starts_with("audio/") || t.starts_with("video/") || t == "application/ogg"
}

/// A stored file, without its bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobInfo {
    pub id: Uuid,
    pub content_type: String,
    pub filename: Option<String>,
    pub size_bytes: i32,
}

fn default_origin() -> String {
    "capture".to_owned()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    pub id: Uuid,
    pub kind: String,
    pub origin: String,
    pub uri: Option<String>,
    pub title: Option<String>,
    pub content: Option<String>,
    pub content_sha256: String,
    pub said_at: Option<Timestamp>,
    pub captured_by: String,
    pub captured_at: Timestamp,
    pub metadata: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<BlobInfo>,
    pub evidence: Vec<Evidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureResult {
    pub source: Source,
    /// False when the same content from the same origin was captured before.
    pub created: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewEvidence {
    pub source_id: Uuid,
    pub anchor: serde_json::Value,
    #[serde(default)]
    pub quote: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence {
    pub id: Uuid,
    pub source_id: Uuid,
    pub anchor: serde_json::Value,
    pub quote: Option<String>,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewObservation {
    pub source_id: Uuid,
    #[serde(default)]
    pub evidence_id: Option<Uuid>,
    pub text: String,
    pub modality: Modality,
    #[serde(default)]
    pub said_at: Option<Timestamp>,
    #[serde(default)]
    pub confidence: Option<f32>,
    #[serde(default)]
    pub flags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    pub id: Uuid,
    pub source_id: Uuid,
    pub evidence_id: Option<Uuid>,
    pub text: String,
    pub modality: Modality,
    pub said_at: Option<Timestamp>,
    pub extracted_by: String,
    pub confidence: Option<f32>,
    pub flags: Vec<String>,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    pub id: Uuid,
    #[serde(rename = "type")]
    pub entity_type: String,
    pub name: String,
    pub aliases: Vec<String>,
    pub merged_into: Option<Uuid>,
    pub created_by: String,
    pub created_at: Timestamp,
}

/// Content of an assertion in `create` and `supersede` operations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssertionContent {
    pub kind: String,
    pub text: String,
    /// Entity ids, or `ref:<key>` of an entity created earlier in the same proposal.
    #[serde(default)]
    pub subjects: Vec<String>,
    #[serde(default = "empty_object")]
    pub slots: serde_json::Value,
    pub modality: Modality,
    /// Who decided or stated it in the real world, and where it was recorded.
    #[serde(default = "empty_array")]
    pub authority: serde_json::Value,
    #[serde(default)]
    pub said_at: Option<Timestamp>,
    /// Start of validity; defaults to `said_at`, then to the moment the verdict applies.
    #[serde(default)]
    pub valid_from: Option<Timestamp>,
    #[serde(default)]
    pub review_after: Option<Timestamp>,
    pub evidence: Vec<Uuid>,
}

fn empty_object() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

fn empty_array() -> serde_json::Value {
    serde_json::Value::Array(Vec::new())
}

/// One change in a proposal (docs/11 §6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Operation {
    Create {
        #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
        assertion: AssertionContent,
    },
    Supersede {
        assertion_id: Uuid,
        assertion: AssertionContent,
    },
    Close {
        assertion_id: Uuid,
        status: Status,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        valid_to: Option<Timestamp>,
    },
    Reverify {
        assertion_id: Uuid,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        review_after: Option<Timestamp>,
    },
    Link {
        /// Assertion id or `ref:<key>`.
        from: String,
        to: String,
        kind: LinkKind,
    },
    Unlink {
        from: Uuid,
        to: Uuid,
        kind: LinkKind,
    },
    CreateEntity {
        #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
        #[serde(rename = "type")]
        entity_type: String,
        name: String,
        #[serde(default)]
        aliases: Vec<String>,
    },
    MergeEntities {
        from: Uuid,
        into: Uuid,
    },
    SplitEntity {
        entity: Uuid,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Alternative {
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub operations: Vec<Operation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaseRef {
    pub assertion_id: Uuid,
    pub version: i32,
}

/// Input of `open_proposal`: either `operations` (one alternative) or `alternatives`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewProposal {
    #[serde(default)]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    pub rationale: Option<String>,
    pub risk: Risk,
    #[serde(default)]
    pub confidence: Option<f32>,
    #[serde(default)]
    pub base: Vec<BaseRef>,
    #[serde(default)]
    pub operations: Option<Vec<Operation>>,
    #[serde(default)]
    pub alternatives: Option<Vec<Alternative>>,
    #[serde(default)]
    pub evidence: Vec<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proposal {
    pub id: Uuid,
    pub idempotency_key: Option<String>,
    pub author: String,
    pub rationale: Option<String>,
    pub risk: Risk,
    pub confidence: Option<f32>,
    pub base: Vec<BaseRef>,
    pub alternatives: Vec<Alternative>,
    pub evidence: Vec<Uuid>,
    pub state: ProposalState,
    pub deferred_until: Option<Timestamp>,
    pub created_at: Timestamp,
    pub decided_at: Option<Timestamp>,
    pub verdicts: Vec<Verdict>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerdictInput {
    pub decision: Decision,
    /// Which alternative to accept; may be omitted when there is only one.
    #[serde(default)]
    pub alternative: Option<String>,
    /// Edited operations for `accept_with_edits`.
    #[serde(default)]
    pub operations: Option<Vec<Operation>>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub deferred_until: Option<Timestamp>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verdict {
    pub id: Uuid,
    pub proposal_id: Uuid,
    pub decision: Decision,
    pub alternative: Option<String>,
    pub edits: Option<serde_json::Value>,
    pub reason: Option<String>,
    pub decided_by: String,
    pub created_at: Timestamp,
}

/// One immutable bitemporal row of an assertion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssertionVersion {
    pub assertion_id: Uuid,
    pub version: i32,
    pub kind: String,
    pub text: String,
    pub subjects: Vec<Uuid>,
    pub slots: serde_json::Value,
    pub modality: Modality,
    pub authority: serde_json::Value,
    pub said_at: Option<Timestamp>,
    pub valid_from: Timestamp,
    pub valid_to: Option<Timestamp>,
    pub recorded_from: Timestamp,
    pub recorded_to: Option<Timestamp>,
    pub status: Status,
    pub review_after: Option<Timestamp>,
    pub evidence: Vec<Uuid>,
    pub proposal_id: Uuid,
    pub verdict_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Link {
    pub from: Uuid,
    pub to: Uuid,
    pub kind: LinkKind,
    pub recorded_from: Timestamp,
}

/// An assertion as seen at one point: the selected version plus current flags and links.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssertionView {
    #[serde(flatten)]
    pub version: AssertionVersion,
    pub flags: Vec<Flag>,
    pub links: Vec<Link>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityView {
    #[serde(flatten)]
    pub entity: Entity,
    /// Entities merged into this one.
    pub merged: Vec<Uuid>,
    pub assertions: Vec<AssertionVersion>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEvent {
    pub position: i64,
    pub id: Uuid,
    #[serde(rename = "type")]
    pub event_type: String,
    pub subject: Option<String>,
    pub actor: String,
    pub data: serde_json::Value,
    pub occurred_at: Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotOp {
    Eq,
    Gte,
    Lt,
    In,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotFilter {
    pub key: String,
    pub op: SlotOp,
    pub value: String,
}

/// Filter of `list_assertions`. Without `valid_at`, each assertion shows its latest validity
/// segment; without `as_of`, current knowledge is used.
#[derive(Debug, Clone, Default)]
pub struct AssertionFilter {
    pub kind: Option<String>,
    pub entity: Option<Uuid>,
    pub status: Option<Status>,
    pub text: Option<String>,
    pub valid_at: Option<Timestamp>,
    pub as_of: Option<Timestamp>,
    pub slots: Vec<SlotFilter>,
    pub limit: Option<i64>,
    pub after: Option<Uuid>,
}
