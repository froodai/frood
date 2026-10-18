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

    // Sections implemented by later milestones. They are accepted so a config can be written
    // ahead of time, but ignored with a warning until the milestone lands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sources: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accounts: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtimes: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imports: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jobs: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channels: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modules: Option<serde_json::Value>,
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

/// Reserved sections and the milestone that starts using each of them.
pub(crate) const RESERVED_SECTIONS: &[(&str, &str)] = &[
    ("sources", "M3"),
    ("accounts", "M4"),
    ("runtimes", "M4"),
    ("imports", "M4"),
    ("jobs", "M4"),
    ("attention", "M5"),
    ("channels", "M5"),
    ("backup", "M6"),
    ("modules", "M6"),
];

impl Config {
    pub(crate) fn reserved_present(&self) -> Vec<&'static str> {
        let present = [
            ("sources", self.sources.is_some()),
            ("accounts", self.accounts.is_some()),
            ("runtimes", self.runtimes.is_some()),
            ("imports", self.imports.is_some()),
            ("jobs", self.jobs.is_some()),
            ("attention", self.attention.is_some()),
            ("channels", self.channels.is_some()),
            ("backup", self.backup.is_some()),
            ("modules", self.modules.is_some()),
        ];
        present
            .into_iter()
            .filter(|(_, is)| *is)
            .map(|(name, _)| name)
            .collect()
    }
}
