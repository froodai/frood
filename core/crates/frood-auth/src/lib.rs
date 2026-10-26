//! Sign-in and tokens (docs/12_api_mcp_auth.md §3).
//!
//! The installation is its own OAuth 2.1 authorization server for MCP and API clients:
//! authorization code + PKCE, clients identified by Client ID Metadata Documents or registered
//! dynamically, refresh-token rotation. People prove who they are through an external OpenID
//! Connect provider (or, on loopback only, a development form). Personal tokens serve scripts
//! and bots. Every secret is stored as a hash.

mod channels;
mod clients;
pub use clients::ADMIN_CLIENT;
mod oidc;
mod pages;
mod passkey;
mod secret;
mod sign_in;
mod tokens;
mod users;
mod web;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sqlx::PgPool;
use uuid::Uuid;

pub use channels::{ChannelLink, LinkCode};
pub use clients::{ClientInfo, is_public_ip};
pub use passkey::{LINK_MINUTES as PASSKEY_LINK_MINUTES, PasskeyInfo};
pub use sign_in::{ASK_MINUTES, SignInAsk};
pub use tokens::RunToken;
pub use tokens::{PersonalToken, TokenPair};
pub use web::{problem_page, routes};

/// Scopes a token can carry (docs/12 §3.3). They are rights of the token, not roles.
pub mod scope {
    pub const READ: &str = "knowledge:read";
    pub const CAPTURE: &str = "knowledge:capture";
    pub const PROPOSE: &str = "knowledge:propose";
    pub const DECIDE: &str = "knowledge:decide";
    pub const JOURNAL: &str = "journal:read";
    pub const ADMIN: &str = "admin";

    pub const ALL: [&str; 6] = [READ, CAPTURE, PROPOSE, DECIDE, JOURNAL, ADMIN];
    /// Modules only: link channel accounts and act for the linked people. Never granted to a
    /// person's token.
    pub const CHANNELS_ACT: &str = "channels:act";
    /// What an agent's run token may ever hold (W4′): never decide, never admin.
    pub const AGENT_ALLOWED: [&str; 4] = [READ, CAPTURE, PROPOSE, JOURNAL];
    /// What an AI client gets unless the person ticks more on the consent page.
    pub const CLIENT_DEFAULT: [&str; 3] = [READ, CAPTURE, PROPOSE];

    pub fn describe(scope: &str) -> &'static str {
        match scope {
            READ => "read knowledge, sources, history and the inbox",
            CAPTURE => "send notes and files",
            PROPOSE => "propose changes to knowledge",
            DECIDE => "accept or reject proposals on your behalf",
            JOURNAL => "read the event journal",
            ADMIN => "manage users, tokens and settings",
            _ => "unknown permission",
        }
    }
}

/// Which protected resource a token is presented to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resource {
    Api,
    Mcp,
}

/// A person who holds a token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub id: Uuid,
    pub email: String,
}

/// The run an agent's token belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRef {
    pub id: Uuid,
    pub job: String,
}

#[derive(Debug, Clone)]
pub struct Principal {
    /// The person; `None` for a module's service token. For a run token, the person the run
    /// acts for, if any.
    pub user: Option<User>,
    /// For a run token: the agent's run.
    pub run: Option<RunRef>,
    /// The module a service token belongs to.
    pub module: Option<String>,
    pub scopes: Vec<String>,
    pub client_id: Option<String>,
    pub token_id: Uuid,
}

impl Principal {
    pub fn has(&self, scope: &str) -> bool {
        self.scopes.iter().any(|s| s == scope)
    }

    /// The person behind the token. A run token acts for a person but is not that person.
    pub fn user_id(&self) -> Option<Uuid> {
        if self.run.is_some() {
            return None;
        }
        self.user.as_ref().map(|u| u.id)
    }

    /// Actor of a channel module acting for the person whose channel account it serves:
    /// the person's action, through the module (`user:<id>/via/<module>`).
    pub fn acting_for(&self, user_id: Uuid) -> Option<String> {
        let module = self.module.as_deref()?;
        self.has(scope::CHANNELS_ACT)
            .then(|| format!("user:{user_id}/via/{module}"))
    }

    /// Ledger actor string of this principal.
    ///
    /// Both personal tokens and tokens of OAuth clients act as the person: an OAuth client
    /// (Claude, ChatGPT, Claude Code) is driven by the person sitting in front of it, so what
    /// it writes on their word is theirs (`user:<id>`). The client is kept for the audit:
    /// `user:<id>/via/<client>`. Agents that run on their own (scheduled jobs, M4) get run
    /// tokens and act as `agent:<job>` on the person's behalf, which policies treat as an
    /// agent's proposal.
    /// A module's service token acts as the module (`integration:<module>`).
    /// A run token acts as the agent's job, for the person the run belongs to
    /// (`agent:<job>/for/<user id>`).
    pub fn actor(&self) -> String {
        if let Some(run) = &self.run {
            return match &self.user {
                Some(user) => format!("agent:{}/for/{}", run.job, user.id),
                None => format!("agent:{}", run.job),
            };
        }
        match (&self.user, &self.module, &self.client_id) {
            (Some(user), _, None) => format!("user:{}", user.id),
            (Some(user), _, Some(client)) => {
                format!("user:{}/via/{}", user.id, client_label(client))
            }
            (None, Some(module), _) => format!("integration:{module}"),
            (None, None, _) => "system:unknown".to_owned(),
        }
    }
}

/// Short, stable name of a client for audit: the host of a metadata-document client id,
/// otherwise the registered id.
fn client_label(client_id: &str) -> String {
    let label = url::Url::parse(client_id)
        .ok()
        .filter(|url| url.scheme() == "https")
        .and_then(|url| url.host_str().map(str::to_owned))
        .unwrap_or_else(|| client_id.to_owned());
    label
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '/')
        .take(120)
        .collect()
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// No usable token. `invalid` is true when a token was presented but is not valid.
    #[error("{message}")]
    Unauthorized { invalid: bool, message: String },
    #[error("this token lacks the {0} permission")]
    InsufficientScope(String),
    #[error("{0}")]
    Invalid(String),
    #[error("{0} not found")]
    NotFound(String),
    #[error("identity provider error: {0}")]
    Upstream(String),
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
}

pub type Result<T> = std::result::Result<T, AuthError>;

/// Settings derived from `frood.yaml`.
#[derive(Debug, Clone)]
pub struct AuthSettings {
    /// Base URL of this installation without a trailing slash; the OAuth issuer.
    pub issuer: String,
    /// The issuer's path (`/frood` for `https://host/frood`, empty at the root). A proxy in
    /// front strips it: the server's own routes stay at the root.
    pub base_path: String,
    pub installation: String,
    pub allow: Vec<String>,
    pub allow_domains: Vec<String>,
    pub providers: Vec<frood_config::Provider>,
    pub access_ttl: Duration,
    pub refresh_ttl: Duration,
}

impl AuthSettings {
    pub fn from_config(config: &frood_config::Config) -> Self {
        let issuer = match &config.server.public_url {
            Some(url) => url.trim_end_matches('/').to_owned(),
            None => {
                let mut addr: SocketAddr = config
                    .server
                    .listen
                    .parse()
                    .unwrap_or_else(|_| ([127, 0, 0, 1], 8080).into());
                if addr.ip().is_unspecified() {
                    addr.set_ip([127, 0, 0, 1].into());
                }
                format!("http://{addr}")
            }
        };
        let base_path = url::Url::parse(&issuer)
            .map(|url| url.path().trim_end_matches('/').to_owned())
            .unwrap_or_default();
        Self {
            issuer,
            base_path,
            installation: config.installation.name.clone(),
            allow: config.auth.allow.iter().map(|e| e.to_lowercase()).collect(),
            allow_domains: config
                .auth
                .allow_domains
                .iter()
                .map(|d| d.to_lowercase())
                .collect(),
            providers: config.auth.providers.clone(),
            access_ttl: Duration::from_secs(u64::from(config.auth.access_token_minutes) * 60),
            refresh_ttl: Duration::from_secs(u64::from(config.auth.refresh_token_days) * 86_400),
        }
    }

    pub fn resource_url(&self, resource: Resource) -> String {
        match resource {
            Resource::Api => format!("{}/v1", self.issuer),
            Resource::Mcp => format!("{}/mcp", self.issuer),
        }
    }

    /// Where a client learns how to obtain a token for `resource` (RFC 9728): the well-known
    /// part goes between the host and the resource's path.
    pub fn resource_metadata_url(&self, resource: Resource) -> String {
        let name = match resource {
            Resource::Api => "v1",
            Resource::Mcp => "mcp",
        };
        format!(
            "{}/.well-known/oauth-protected-resource{}/{name}",
            self.origin(),
            self.base_path
        )
    }

    /// The console's address for links from channels; only for a public HTTPS address, since a
    /// phone cannot open this machine's loopback.
    pub fn console_url(&self) -> Option<String> {
        self.issuer
            .starts_with("https://")
            .then(|| format!("{}/admin/", self.issuer))
    }

    /// A path of this server as the browser sees it, behind the issuer's path.
    pub fn path(&self, path: &str) -> String {
        format!("{}{path}", self.base_path)
    }

    fn origin(&self) -> String {
        url::Url::parse(&self.issuer)
            .map(|url| url.origin().ascii_serialization())
            .unwrap_or_else(|_| self.issuer.clone())
    }

    fn secure_cookies(&self) -> bool {
        self.issuer.starts_with("https://")
    }
}

#[derive(Clone)]
pub struct Auth {
    inner: Arc<Inner>,
}

struct Inner {
    pool: PgPool,
    settings: AuthSettings,
    http: reqwest::Client,
    cimd_cache: Mutex<HashMap<String, (ClientInfo, std::time::Instant)>>,
    oidc_cache: Mutex<HashMap<String, oidc::ProviderMetadata>>,
    /// A passkey's challenge between its two steps, by the link or the sign-in request.
    passkey_steps: Mutex<HashMap<String, (passkey::Step, std::time::Instant)>>,
}

impl Auth {
    pub fn new(pool: PgPool, settings: AuthSettings) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("HTTP client builds");
        Self {
            inner: Arc::new(Inner {
                pool,
                settings,
                http,
                cimd_cache: Mutex::new(HashMap::new()),
                oidc_cache: Mutex::new(HashMap::new()),
                passkey_steps: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub fn settings(&self) -> &AuthSettings {
        &self.inner.settings
    }

    fn pool(&self) -> &PgPool {
        &self.inner.pool
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn principal(client_id: Option<&str>) -> Principal {
        Principal {
            user: Some(User {
                id: Uuid::nil(),
                email: "a@example.com".into(),
            }),
            run: None,
            module: None,
            scopes: vec![],
            client_id: client_id.map(str::to_owned),
            token_id: Uuid::nil(),
        }
    }

    #[test]
    fn run_tokens_act_as_their_job() {
        let nil = Uuid::nil();
        let mut agent = principal(None);
        agent.run = Some(RunRef {
            id: nil,
            job: "daily-sweep".into(),
        });
        assert_eq!(agent.actor(), format!("agent:daily-sweep/for/{nil}"));
        assert_eq!(agent.user_id(), None, "a run is not the person");
        agent.user = None;
        assert_eq!(agent.actor(), "agent:daily-sweep");
    }

    #[test]
    fn modules_act_as_themselves_or_for_linked_people() {
        let mut module = Principal {
            user: None,
            run: None,
            module: Some("telegram".into()),
            scopes: vec![scope::CAPTURE.into()],
            client_id: None,
            token_id: Uuid::nil(),
        };
        assert_eq!(module.actor(), "integration:telegram");
        let nil = Uuid::nil();
        assert_eq!(module.acting_for(nil), None);
        module.scopes.push(scope::CHANNELS_ACT.into());
        assert_eq!(
            module.acting_for(nil),
            Some(format!("user:{nil}/via/telegram"))
        );
        assert_eq!(principal(None).acting_for(nil), None);
    }

    #[test]
    fn oauth_clients_act_as_the_person_and_are_named_for_audit() {
        let nil = Uuid::nil();
        assert_eq!(principal(None).actor(), format!("user:{nil}"));
        assert_eq!(
            principal(Some("https://claude.ai/oauth/mcp-oauth-client-metadata")).actor(),
            format!("user:{nil}/via/claude.ai")
        );
        assert_eq!(
            principal(Some("frood_client_x")).actor(),
            format!("user:{nil}/via/frood_client_x")
        );
    }
}
