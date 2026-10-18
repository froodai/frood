use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Root of `frood.yaml`: one installation, described declaratively.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Config format version. Only `1` exists.
    pub version: u32,
    pub installation: Installation,
    #[serde(default)]
    pub server: Server,
    pub storage: Storage,
    #[serde(default)]
    pub deploy: Deploy,
    #[serde(default)]
    pub policies: Policies,
    #[serde(default)]
    pub auth: Auth,
    /// Accounts of model providers, by name (docs/13 §8).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub accounts: BTreeMap<String, Account>,
    /// Container modules, by name (docs/16 §2).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub modules: BTreeMap<String, Module>,
    /// Jobs: changes to the built-in standard set and new jobs (docs/13 §3).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub jobs: BTreeMap<String, Job>,
    #[serde(default)]
    pub search: Search,
    /// Where knowledge comes from (docs/14 §3), by name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sources: BTreeMap<String, Source>,

    // Sections implemented by later milestones. They are accepted so a config can be written
    // ahead of time, but ignored with a warning until the milestone lands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtimes: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imports: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channels: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Installation {
    /// Short machine name: lowercase letters, digits and dashes, starting with a letter.
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Server {
    /// Address the HTTP server binds to.
    #[serde(default = "default_listen")]
    pub listen: String,
    /// Public HTTPS address; AI clients and OAuth need it from milestone M2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_url: Option<String>,
}

impl Default for Server {
    fn default() -> Self {
        Self {
            listen: default_listen(),
            public_url: None,
        }
    }
}

fn default_listen() -> String {
    "127.0.0.1:8080".to_owned()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Storage {
    /// Storage module. Only `postgres` exists so far.
    pub uses: String,
    /// Connection URL; normally built from `${secret:...}` and `${env:...}` references.
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Deploy {
    /// Deploy target module. Only `compose` (Docker Compose) exists so far.
    #[serde(default = "default_target")]
    pub target: String,
    /// Where the Frood server runs.
    #[serde(default)]
    pub server: ServerPlacement,
    /// Server image when `server: container`; pin by digest in production.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// Build the server image from this directory instead (relative to the config file).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    /// Host port PostgreSQL is published on (bound to 127.0.0.1).
    #[serde(default = "default_postgres_port")]
    pub postgres_port: u16,
}

impl Default for Deploy {
    fn default() -> Self {
        Self {
            target: default_target(),
            server: ServerPlacement::default(),
            image: None,
            build: None,
            postgres_port: default_postgres_port(),
        }
    }
}

fn default_target() -> String {
    "compose".to_owned()
}

fn default_postgres_port() -> u16 {
    5432
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ServerPlacement {
    /// The server runs in Docker next to its database.
    #[default]
    Container,
    /// You run `frood serve` yourself (development).
    Host,
}

impl ServerPlacement {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Container => "container",
            Self::Host => "host",
        }
    }
}

/// Sign-in (docs/12_api_mcp_auth.md §3): who may sign in and through which providers. The
/// installation issues its own tokens; providers only prove who the person is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Auth {
    /// Email addresses allowed to sign in.
    #[serde(default)]
    pub allow: Vec<String>,
    /// Email domains allowed to sign in, e.g. `company.com`.
    #[serde(default)]
    pub allow_domains: Vec<String>,
    #[serde(default)]
    pub providers: Vec<Provider>,
    #[serde(default = "default_access_minutes")]
    pub access_token_minutes: u32,
    #[serde(default = "default_refresh_days")]
    pub refresh_token_days: u32,
}

impl Default for Auth {
    fn default() -> Self {
        Self {
            allow: Vec::new(),
            allow_domains: Vec::new(),
            providers: Vec::new(),
            access_token_minutes: default_access_minutes(),
            refresh_token_days: default_refresh_days(),
        }
    }
}

fn default_access_minutes() -> u32 {
    60
}

fn default_refresh_days() -> u32 {
    30
}

/// An identity provider people sign in with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Provider {
    /// Any OpenID Connect provider: Google (`https://accounts.google.com`), Microsoft Entra,
    /// a corporate SSO.
    Oidc {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        issuer: String,
        client_id: String,
        client_secret: String,
    },
    /// Development only: type an email address. Allowed only on a loopback address without
    /// `server.public_url`.
    Dev {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
}

impl Provider {
    pub fn id(&self) -> &str {
        match self {
            Self::Oidc { id, .. } | Self::Dev { id, .. } => id,
        }
    }

    pub fn label(&self) -> &str {
        match self {
            Self::Oidc { label, id, .. } | Self::Dev { label, id } => {
                label.as_deref().unwrap_or(id)
            }
        }
    }
}

/// Auto-verdict policies of the Ledger (docs/11_knowledge_model.md §8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Policies {
    /// Accept a user's own new facts automatically when they conflict with nothing.
    #[serde(default = "default_true")]
    pub auto_accept_own_facts: bool,
    /// Accept entity merges at or above this confidence automatically; `null` turns it off.
    #[serde(default = "default_merge_threshold")]
    pub entity_merge_threshold: Option<f32>,
}

impl Default for Policies {
    fn default() -> Self {
        Self {
            auto_accept_own_facts: true,
            entity_merge_threshold: default_merge_threshold(),
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_merge_threshold() -> Option<f32> {
    Some(0.9)
}

/// An account that pays for model calls.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Account {
    /// A provider API key, paid by use. Belongs to the installation.
    ApiKey {
        provider: ModelProvider,
        /// The key, e.g. `${secret:ANTHROPIC_API_KEY}`.
        secret: String,
        /// Another endpoint with the same API (a proxy or a compatible service).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_url: Option<String>,
        /// Anthropic: the workspace (`wrkspc_…`) for a key made at organization level, which
        /// must name one in every request.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        budget: Option<AccountBudget>,
    },
    /// One person's own subscription (Claude Pro or Max, ChatGPT Plus or Pro). Its vendors allow
    /// it only inside their own agent, for the subscriber's own work, so only agent jobs that run
    /// that agent (`claude-code`, `codex`) on the owner's behalf may use it.
    Subscription {
        provider: ModelProvider,
        /// The subscriber's email; only runs on their behalf may use the account.
        owner: String,
        /// Anthropic: the token from `claude setup-token`. OpenAI: the contents of
        /// `~/.codex/auth.json` after `codex login`.
        secret: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        budget: Option<AccountBudget>,
    },
}

/// A limit on what an account spends in a calendar month (UTC), counted from what agent runs
/// report. Runs that would go over it are not started, or are stopped.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AccountBudget {
    pub monthly_usd: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ModelProvider {
    Anthropic,
    Openai,
}

impl Account {
    pub fn provider(&self) -> ModelProvider {
        match self {
            Self::ApiKey { provider, .. } | Self::Subscription { provider, .. } => *provider,
        }
    }

    pub fn budget(&self) -> Option<&AccountBudget> {
        match self {
            Self::ApiKey { budget, .. } | Self::Subscription { budget, .. } => budget.as_ref(),
        }
    }

    /// The subscriber, for a subscription.
    pub fn owner(&self) -> Option<&str> {
        match self {
            Self::ApiKey { .. } => None,
            Self::Subscription { owner, .. } => Some(owner),
        }
    }
}

impl ModelProvider {
    /// The vendor's own agent, the only one a subscription may be used in.
    pub fn own_agent(self) -> &'static str {
        match self {
            Self::Anthropic => "claude-code",
            Self::Openai => "codex",
        }
    }
}

/// A container module (docs/16 §2): where it comes from, its secrets and settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Module {
    /// Image to run; pin by digest in production.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// Or build the image from this directory (relative to the config file).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    /// Or a module that already runs elsewhere, e.g. during development.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Host port (127.0.0.1) for a server that runs on the host; assigned when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Secrets the module receives as environment variables. Others stay hidden from it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets: Vec<String>,
    /// Module settings, checked against the schema in its manifest.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub config: serde_json::Map<String, serde_json::Value>,
}

/// A job: what capability to call, on which events, with which inputs (docs/13 §3–4).
/// Jobs of the standard set (`transcribe`, `extract`, `embed`, `daily-sweep`) exist without
/// being written; writing one changes only the fields given.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Job {
    /// Capability the job calls: `llm.complete`, `agents.execute`, `media.transcribe`, …
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uses: Option<String>,
    /// `false` turns the job off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Events and schedules that start the job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<Vec<Trigger>>,
    /// Instructions: inline text, or a path (`./agents/daily-sweep.md`, relative to the config).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    /// MCP servers the agent may use: `frood` and `source:<name>` (or `source:${source}` with
    /// `for_each: source`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
    /// Module that runs the agent (gives `agents.execute`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    /// Which agent the runtime starts: `claude-code`, `codex`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Model, always named: there is no default (D4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Account from `accounts` that pays.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// Rights of the run token; agents never get `knowledge:decide` or `admin`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<Vec<String>>,
    /// Email of the person the agent works for; required with a subscription account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_behalf_of: Option<String>,
    /// Whether and how the agent may ask a person.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ask_user: Option<AskUser>,
    /// Limits of one run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<Budget>,
    /// Let a policy accept the agent's proposals (W4′): only from an authoritative source and
    /// never a conflict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_accept: Option<AutoAccept>,
    /// Other inputs for the capability (`module`, `effort`, …).
    #[serde(
        default,
        rename = "with",
        skip_serializing_if = "serde_json::Map::is_empty"
    )]
    pub with: serde_json::Map<String, serde_json::Value>,
}

/// An event or a schedule that starts a job.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Trigger {
    /// Journal event type, e.g. `source.captured`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    /// Event data fields that must equal these values, e.g. `{ has_text: true }`.
    #[serde(default, rename = "where", skip_serializing_if = "BTreeMap::is_empty")]
    pub filter: BTreeMap<String, serde_json::Value>,
    /// Cron schedule, e.g. `"0 3 * * *"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
    /// IANA time zone of the schedule, e.g. `Europe/Amsterdam`; UTC when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    /// Run once per item: `source` runs the job for each source in `sources`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub for_each: Option<String>,
}

impl Trigger {
    pub fn on_event(event: &str) -> Self {
        Self {
            event: Some(event.to_owned()),
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AskUser {
    #[serde(default = "default_true")]
    pub allowed: bool,
    /// How long a question waits, e.g. `3d`, `12h`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expire_after: Option<String>,
    /// What happens when nobody answers in time.
    #[serde(default)]
    pub on_expire: OnExpire,
    /// The answer used with `on_expire: default_answer`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_answer: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OnExpire {
    /// The agent continues without the answer.
    #[default]
    Skip,
    /// The run is cancelled.
    Cancel,
    /// The agent continues with `default_answer`.
    DefaultAnswer,
}

impl OnExpire {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Skip => "skip",
            Self::Cancel => "cancel",
            Self::DefaultAnswer => "default_answer",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    /// Stop the run when it has cost this much.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_usd: Option<f64>,
    /// Stop the run after this many minutes of work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_minutes: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AutoAccept {
    /// `authoritative_source`: the run reads one source, marked `authoritative`.
    pub when: String,
    /// Conflicts always wait for a person; only `true` is accepted.
    #[serde(default = "yes")]
    pub no_conflict: bool,
}

fn yes() -> bool {
    true
}

/// A place knowledge comes from (docs/14 §3). This version has MCP servers that agents read
/// (`uses: mcp`); pull and push connectors come as modules later.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Source {
    /// `mcp`: an MCP server an agent reads through its tools.
    pub uses: String,
    /// The MCP server's URL (Streamable HTTP).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    /// Headers for the server, e.g. `Authorization: Bearer ${secret:SLACK_MCP_TOKEN}` for a
    /// service account.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// A person's own connection (`user:<email>`) instead of a service account. Later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection: Option<String>,
    /// What to read there, for the agent: channels, folders, projects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<serde_json::Value>,
    /// What Frood keeps from it: `excerpts` (quotes in evidence) or `full`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retain: Option<String>,
    /// Decisions from here may be accepted by a job's policy (W4′).
    #[serde(default)]
    pub authoritative: bool,
}

/// Search settings.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Search {
    /// Embeddings for search by meaning and across languages. Without them, search matches
    /// words only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embeddings: Option<Embeddings>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Embeddings {
    /// Account from `accounts` that computes them.
    pub account: String,
    /// Model, always named: Frood has no default model (D4). E.g. `text-embedding-3-small`.
    pub model: String,
}

/// Reserved sections and the milestone that starts using each of them.
pub(crate) const RESERVED_SECTIONS: &[(&str, &str)] = &[
    (
        "runtimes",
        "a later milestone; name a runtime module in the job's `runtime` for now",
    ),
    ("imports", "a later milestone"),
    ("attention", "M5"),
    ("channels", "M5"),
    ("backup", "M6"),
];

impl Config {
    pub(crate) fn reserved_present(&self) -> Vec<&'static str> {
        let present = [
            ("runtimes", self.runtimes.is_some()),
            ("imports", self.imports.is_some()),
            ("attention", self.attention.is_some()),
            ("channels", self.channels.is_some()),
            ("backup", self.backup.is_some()),
        ];
        present
            .into_iter()
            .filter(|(_, is)| *is)
            .map(|(name, _)| name)
            .collect()
    }
}
