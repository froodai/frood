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
    /// Areas of life knowledge belongs to (docs/26 §3): a label for reading and filters, not a
    /// boundary of who sees what. Without this section: `tech`, `business` and `personal`; an
    /// empty list turns spheres off.
    #[serde(default = "default_spheres")]
    pub spheres: Vec<Sphere>,
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
    /// Who is told what, when and where (docs/15).
    #[serde(default)]
    pub attention: Attention,
    /// Backups (docs/16 §7). When they run is `jobs.backup` and `jobs.backup-verify`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup: Option<Backup>,

    // Sections implemented by later milestones. They are accepted so a config can be written
    // ahead of time, but ignored with a warning until the milestone lands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtimes: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imports: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channels: Option<serde_json::Value>,
}

/// Backups (C4): a dump of the database (knowledge, journal, files, config revisions, Inbox),
/// `frood.yaml`, and the secrets file encrypted to the recovery key, into every target.
/// Nothing here has a default: what to keep and where is the installation's to say.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Backup {
    /// The module that gives `backup.target@1`; needed only when several modules give it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    /// The public half of your recovery key (`age1…`, from `frood backup key`). The secrets in a
    /// backup are encrypted to it; the secret half stays with you, off the server.
    pub recovery_key: String,
    /// Where backups go, by name. Two independent ones protect against losing a provider.
    pub targets: BTreeMap<String, BackupTarget>,
    /// Which backups to keep in each target.
    pub keep: BackupKeep,
}

/// A restic repository.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BackupTarget {
    /// As restic names it: a folder inside the module (`/repos/disk`, from the module's
    /// `volumes`), `s3:https://host/bucket/path`, `b2:bucket:path`, `sftp:user@host:path`,
    /// `rest:https://…`.
    pub repository: String,
    /// The password restic encrypts the repository with, e.g. `${secret:BACKUP_DISK_PASSWORD}`.
    pub password: String,
    /// Environment restic needs for this repository, e.g. `AWS_ACCESS_KEY_ID` and
    /// `AWS_SECRET_ACCESS_KEY` for S3, `B2_ACCOUNT_ID` and `B2_ACCOUNT_KEY` for B2.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

/// restic's `forget` rules: the last N backups, and the last backup of each of N hours, days, …
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BackupKeep {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hourly: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weekly: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monthly: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yearly: Option<u32>,
}

impl BackupKeep {
    /// The rules that are set, as restic's flag names without `--keep-`.
    pub fn rules(&self) -> Vec<(&'static str, u32)> {
        [
            ("last", self.last),
            ("hourly", self.hourly),
            ("daily", self.daily),
            ("weekly", self.weekly),
            ("monthly", self.monthly),
            ("yearly", self.yearly),
        ]
        .into_iter()
        .filter_map(|(name, n)| n.map(|n| (name, n)))
        .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Installation {
    /// Short machine name: lowercase letters, digits and dashes, starting with a letter.
    pub name: String,
    /// IANA zone, e.g. Europe/Amsterdam: the default for built-in schedules (the morning digest)
    /// and for people who set none (quiet hours, reminders). Without it, UTC.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Server {
    /// Address the HTTP server binds to.
    #[serde(default = "default_listen")]
    pub listen: String,
    /// Public HTTPS address; AI clients and OAuth need it. It may end in a path
    /// (`https://example.com/frood`) when the host serves other things as well.
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
    /// Where files go (docs/26 §2): photos, videos, voice messages, documents. Without this
    /// section: a folder on the server's disk.
    #[serde(default)]
    pub objects: Objects,
}

/// A store of files.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "uses", rename_all = "snake_case", deny_unknown_fields)]
pub enum Objects {
    /// A folder on the machine with `frood.yaml`: `.frood/objects` next to it unless `path`
    /// names another. Backups take it.
    Disk {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        /// The largest file, in megabytes.
        #[serde(default = "default_max_mb")]
        max_mb: u32,
    },
    /// An S3-compatible storage: AWS S3, Backblaze B2, Cloudflare R2, SeaweedFS, MinIO. Backups
    /// do not take it: keep it safe with the storage's own versioning.
    S3 {
        /// e.g. `https://s3.eu-central-003.backblazeb2.com`.
        endpoint: String,
        bucket: String,
        /// The storage's region; `us-east-1` when it has none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        region: Option<String>,
        /// Keys start with it, e.g. `frood/`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prefix: Option<String>,
        access_key_id: String,
        /// e.g. `${secret:S3_SECRET_KEY}`.
        secret_access_key: String,
        /// `endpoint/bucket/key` instead of `bucket.endpoint/key` (SeaweedFS, MinIO).
        #[serde(default)]
        path_style: bool,
        #[serde(default = "default_max_mb")]
        max_mb: u32,
    },
    /// In the database, as before 0.1.7: for small installations and tests.
    Database {
        #[serde(default = "default_database_max_mb")]
        max_mb: u32,
    },
}

impl Default for Objects {
    fn default() -> Self {
        Self::Disk {
            path: None,
            max_mb: default_max_mb(),
        }
    }
}

impl Objects {
    /// The largest file, in megabytes.
    pub fn max_mb(&self) -> u32 {
        match self {
            Self::Disk { max_mb, .. } | Self::S3 { max_mb, .. } | Self::Database { max_mb } => {
                *max_mb
            }
        }
    }
}

fn default_max_mb() -> u32 {
    100
}

fn default_database_max_mb() -> u32 {
    25
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
    /// The HTTPS front (Caddy) that runs when `server.public_url` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<Proxy>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Proxy {
    /// A file of Caddy routes for the same site, such as another app under `/pages`
    /// (relative to the config file). Caddy sees its folder at the same path, so the file
    /// may import files next to it. Frood's own routes win only for its paths.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routes: Option<String>,
    /// The server's own proxy is in front (docs/29): Frood runs no Caddy and writes only its
    /// routes into this file of that proxy (relative to the config file). With a path in
    /// `server.public_url` they are routes for the shared site; without one, a site block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external: Option<String>,
}

impl Default for Deploy {
    fn default() -> Self {
        Self {
            target: default_target(),
            server: ServerPlacement::default(),
            image: None,
            build: None,
            postgres_port: default_postgres_port(),
            proxy: None,
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
    /// Sign-in on this computer (docs/27 §1): no password, for the addresses in `auth.allow`
    /// only. Allowed only while the installation has no public address (a laptop).
    Local {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    /// Sign-in with a button in the person's linked Telegram (docs/28 §2), for the addresses in
    /// `auth.allow` only. Works with a public address; the channel module only shows the button
    /// and passes the press on.
    Telegram {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        /// The channel the button goes to.
        #[serde(default = "default_sign_in_channel")]
        channel: String,
    },
    /// Sign-in with a passkey — a fingerprint, Face ID or a security key (docs/28 §2), for the
    /// addresses in `auth.allow` only. The first passkey comes through a one-time link
    /// (`frood passkey`, or the console after another sign-in). Needs `server.public_url`.
    Passkey {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
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
            Self::Oidc { id, .. }
            | Self::Local { id, .. }
            | Self::Telegram { id, .. }
            | Self::Passkey { id, .. }
            | Self::Dev { id, .. } => id,
        }
    }

    pub fn label(&self) -> &str {
        match self {
            Self::Oidc { label, id, .. }
            | Self::Local { label, id }
            | Self::Telegram { label, id, .. }
            | Self::Passkey { label, id }
            | Self::Dev { label, id } => label.as_deref().unwrap_or(id),
        }
    }
}

fn default_sign_in_channel() -> String {
    "telegram".to_owned()
}

/// Auto-verdict policies of the Ledger (docs/11_knowledge_model.md §8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Policies {
    /// Accept a user's own new facts automatically when they conflict with nothing.
    #[serde(default = "default_true")]
    pub auto_accept_own_facts: bool,
    /// Accept a user's change of knowledge only they wrote (their notes, imports, edits) when it
    /// conflicts with nothing (docs/27 §4).
    #[serde(default = "default_true")]
    pub auto_accept_own_changes: bool,
    /// Accept new knowledge agents find in ordinary sources at once, marked not verified;
    /// changes, conflicts and merges still wait for a person (docs/27 §6). `false`: everything
    /// agents find waits in the Inbox.
    #[serde(default = "default_true")]
    pub accept_agent_findings: bool,
    /// Accept entity merges at or above this confidence automatically; `null` turns it off.
    #[serde(default = "default_merge_threshold")]
    pub entity_merge_threshold: Option<f32>,
}

impl Default for Policies {
    fn default() -> Self {
        Self {
            auto_accept_own_facts: true,
            auto_accept_own_changes: true,
            accept_agent_findings: true,
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

/// One sphere. The built-in ones (`tech`, `business`, `personal`) need only the id: the
/// interface names them in its language, and the model knows what goes there.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Sphere {
    /// Lowercase letters, digits and dashes, e.g. `family`.
    pub id: String,
    /// What people see, e.g. `Семья`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// What belongs there, for the model that picks the sphere, e.g. `home, children, relatives`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub about: Option<String>,
}

/// Built-in spheres: id, English name, what belongs there.
pub const BUILT_IN_SPHERES: [(&str, &str, &str); 3] = [
    (
        "tech",
        "Work · tech",
        "technical work: architecture, code, infrastructure, technical decisions and agreements",
    ),
    (
        "business",
        "Work · business",
        "the business side of work: deadlines, agreements with people, clients, money, priorities",
    ),
    ("personal", "Personal", "life outside work"),
];

impl Sphere {
    fn built_in(&self) -> Option<&'static (&'static str, &'static str, &'static str)> {
        BUILT_IN_SPHERES.iter().find(|(id, ..)| *id == self.id)
    }

    pub fn is_built_in(&self) -> bool {
        self.built_in().is_some()
    }

    /// The configured name, else the built-in English one, else the id.
    pub fn name(&self) -> &str {
        self.name
            .as_deref()
            .or(self.built_in().map(|b| b.1))
            .unwrap_or(&self.id)
    }

    /// What belongs there: the configured text, else the built-in one.
    pub fn about(&self) -> Option<&str> {
        self.about.as_deref().or(self.built_in().map(|b| b.2))
    }
}

fn default_spheres() -> Vec<Sphere> {
    BUILT_IN_SPHERES
        .iter()
        .map(|(id, ..)| Sphere {
            id: (*id).to_owned(),
            name: None,
            about: None,
        })
        .collect()
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
    /// Give the module the host's Docker, as agent runtimes need. That is root on the host
    /// (A2): only for modules you trust.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub host_docker: bool,
    /// Host folders the module sees, as `host-path:module-path`, e.g. a backup disk:
    /// `/Volumes/Backup/frood:/repos/disk`. A relative host path is relative to the config.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volumes: Vec<String>,
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
    /// The server's tools the agent may call; all of them when left out. Name only reading
    /// tools for a source the agent must not change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
}

/// Inbox and notifications (docs/15, docs/22).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Attention {
    /// Personal settings, by email.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub people: BTreeMap<String, AttentionPerson>,
    /// Changes to how urgent kinds of situations are; the built-in table covers the rest.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub routes: Vec<AttentionRoute>,
    /// Situations an agent looks into before anyone is told (N6).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub before_notify: Vec<BeforeNotify>,
    /// Most push messages a person gets in a day: a fuse against floods. Default 30.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_push_per_day: Option<u32>,
    /// When to remind of due dates; a person's own list replaces it. Without it, the day before
    /// and on the day at 09:00 (docs/27 §3); `[]` turns reminders off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reminders: Option<Vec<Reminder>>,
    /// When something counts as stalled (C3); each has a built-in value.
    #[serde(default)]
    pub detectors: Detectors,
}

impl Attention {
    /// The reminders everyone gets unless they set their own: the config's, or the built-in ones.
    pub fn shared_reminders(&self) -> Vec<Reminder> {
        self.reminders.clone().unwrap_or_else(default_reminders)
    }
}

/// The day before the due date and on the day, at 09:00.
pub fn default_reminders() -> Vec<Reminder> {
    [-1, 0]
        .into_iter()
        .map(|days| Reminder {
            days,
            at: "09:00".to_owned(),
        })
        .collect()
}

/// Thresholds of the "something stalled" detectors (C3). Durations like 10m, 48h.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Detectors {
    /// Journal events wait for the dispatcher longer than this. Default 10m.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatcher_lag: Option<String>,
    /// A run waits in the queue longer than this. Default 30m.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued: Option<String>,
    /// An agent run works this much longer than its time budget (or an hour without one). Default 10m.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overrun: Option<String>,
    /// A job failed this many times in a row. Default 3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failures_in_a_row: Option<u32>,
    /// A source was not read successfully for this long. Default 48h.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_stale: Option<String>,
    /// Share of an account's monthly budget spent. Default 0.8.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_share: Option<f64>,
    /// A module does not answer for this long. Default 5m.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module_down: Option<String>,
    /// No successful backup for this many intervals of `jobs.backup`'s schedule. Default 2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backups_missed: Option<u32>,
}

/// One reminder of a due date.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Reminder {
    /// Days from the due date: -3 three days before, 0 on the day, 1 the day after.
    pub days: i32,
    /// Time of day in the person's zone, e.g. "09:00".
    pub at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AttentionPerson {
    /// IANA zone, e.g. Europe/Amsterdam: for quiet hours and reminder times.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    /// No push in these hours, e.g. "23:00-08:00"; urgent things wait for the morning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quiet_hours: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_push_per_day: Option<u32>,
    /// This person's reminders of due dates, instead of `attention.reminders`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reminders: Option<Vec<Reminder>>,
}

/// Which situations a rule matches.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SituationMatch {
    /// proposal, question, reminder or alert.
    pub kind: String,
    /// For proposals: new, change, conflict, stale_knowledge or entity_merge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AttentionRoute {
    #[serde(flatten)]
    pub when: SituationMatch,
    /// now (push), digest (in the next digest) or silent (Inbox only).
    pub urgency: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BeforeNotify {
    pub when: SituationMatch,
    /// The agent job that investigates, e.g. investigate.
    pub run: String,
    /// Tell people anyway after this long, e.g. 30m.
    pub max_wait: String,
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
    ("channels", "M5"),
];

impl Config {
    pub(crate) fn reserved_present(&self) -> Vec<&'static str> {
        let present = [
            ("runtimes", self.runtimes.is_some()),
            ("imports", self.imports.is_some()),
            ("channels", self.channels.is_some()),
        ];
        present
            .into_iter()
            .filter(|(_, is)| *is)
            .map(|(name, _)| name)
            .collect()
    }
}
