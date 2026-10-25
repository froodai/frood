//! Sources connected from the console (docs/26 §4): a layer over `frood.yaml` kept in the
//! database with revisions, its tokens sealed with the installation's vault key. A source of
//! `frood.yaml` wins over the console's of the same name and is changed only there.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use frood_config::Source;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::Row;

use crate::Dispatch;

/// The sources agents read, as they are now; a change from the console replaces them whole.
#[derive(Clone, Default)]
pub struct Sources(Arc<RwLock<Arc<BTreeMap<String, Source>>>>);

impl Sources {
    pub fn now(&self) -> Arc<BTreeMap<String, Source>> {
        self.0.read().expect("sources lock").clone()
    }

    pub fn set(&self, sources: BTreeMap<String, Source>) {
        *self.0.write().expect("sources lock") = Arc::new(sources);
    }
}

/// Seals and opens what the console stores: tokens, encrypted to the installation's key.
pub struct Vault {
    identity: Option<age::x25519::Identity>,
}

impl Vault {
    /// From `FROOD_VAULT_KEY` (an age identity, `AGE-SECRET-KEY-1…`); none closes the vault.
    pub fn new(key: Option<&str>) -> Self {
        Self {
            identity: key.and_then(|k| k.trim().parse().ok()),
        }
    }

    pub fn is_open(&self) -> bool {
        self.identity.is_some()
    }

    fn seal(&self, plain: &str) -> Result<Vec<u8>, ConnectError> {
        let identity = self.identity.as_ref().ok_or(ConnectError::NoVault)?;
        age::encrypt(&identity.to_public(), plain.as_bytes())
            .map_err(|e| ConnectError::Invalid(format!("cannot seal the token: {e}")))
    }

    fn open(&self, sealed: &[u8]) -> Result<String, String> {
        let identity = self.identity.as_ref().ok_or("no vault key")?;
        let plain = age::decrypt(identity, sealed).map_err(|e| e.to_string())?;
        String::from_utf8(plain).map_err(|e| e.to_string())
    }
}

/// The console's layer of the config. Only sources so far.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Overlay {
    #[serde(default)]
    pub sources: BTreeMap<String, Source>,
}

/// A source as the console shows it: where it comes from, and whether a token is stored.
#[derive(Debug, Clone, Serialize)]
pub struct Connection {
    pub name: String,
    /// `config` (frood.yaml, changed only there) or `console`.
    pub from: &'static str,
    pub server: Option<String>,
    pub scope: Option<serde_json::Value>,
    pub tools: Option<Vec<String>>,
    pub authoritative: bool,
    pub has_token: bool,
}

/// What a person sets for a source in the console.
#[derive(Debug, Clone, Deserialize)]
pub struct NewConnection {
    pub server: String,
    /// The token; kept as it is when left out on a change.
    #[serde(default)]
    pub token: Option<String>,
    /// The header that carries it, `Authorization` by default, and what goes before it there
    /// (`Bearer ` for that header).
    #[serde(default)]
    pub header: Option<String>,
    #[serde(default)]
    pub scheme: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub tools: Option<Vec<String>>,
    #[serde(default)]
    pub authoritative: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error("{0}")]
    Invalid(String),
    #[error("source {0} is in frood.yaml: change it there")]
    InConfig(String),
    #[error("no source {0} was connected from the console")]
    NotFound(String),
    #[error("tokens cannot be stored: the secrets have no FROOD_VAULT_KEY; run `frood apply`")]
    NoVault,
    #[error("database: {0}")]
    Database(#[from] sqlx::Error),
}

const SECRET_REF: &str = "${vault:";

fn vault_name(source: &str) -> String {
    format!("source.{source}")
}

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && name.len() <= 40
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

impl Dispatch {
    /// The console's layer as its latest revision has it, with that revision's number.
    pub async fn overlay(&self) -> sqlx::Result<(i64, Overlay)> {
        let row =
            sqlx::query("SELECT revision, data FROM config_overlay ORDER BY revision DESC LIMIT 1")
                .fetch_optional(&self.ctx.pool)
                .await?;
        Ok(match row {
            Some(row) => (
                row.try_get("revision")?,
                serde_json::from_value(row.try_get("data")?).unwrap_or_default(),
            ),
            None => (0, Overlay::default()),
        })
    }

    /// Sources from `frood.yaml` and the console's layer, with stored tokens opened. A source
    /// whose token cannot be opened is left out, with the reason in the log.
    pub async fn reload_sources(&self) -> sqlx::Result<()> {
        let (_, overlay) = self.overlay().await?;
        let mut all: BTreeMap<String, Source> = (*self.ctx.config_sources).clone();
        for (name, mut source) in overlay.sources {
            if all.contains_key(&name) {
                continue;
            }
            match self.open_headers(&mut source).await {
                Ok(()) => {
                    all.insert(name, source);
                }
                Err(err) => {
                    tracing::warn!(source = %name, error = %err, "a source connected in the console is left out")
                }
            }
        }
        self.ctx.sources.set(all);
        Ok(())
    }

    async fn open_headers(&self, source: &mut Source) -> Result<(), String> {
        for value in source.headers.values_mut() {
            while let Some(start) = value.find(SECRET_REF) {
                let end = start
                    + value[start..]
                        .find('}')
                        .ok_or("a header has an unclosed ${vault:")?;
                let name = value[start + SECRET_REF.len()..end].to_owned();
                let sealed: Option<Vec<u8>> =
                    sqlx::query_scalar("SELECT sealed FROM vault WHERE name = $1")
                        .bind(&name)
                        .fetch_optional(&self.ctx.pool)
                        .await
                        .map_err(|e| e.to_string())?;
                let plain = self
                    .ctx
                    .vault
                    .open(&sealed.ok_or_else(|| format!("no stored secret {name}"))?)?;
                value.replace_range(start..=end, &plain);
            }
        }
        Ok(())
    }

    /// Every source, `frood.yaml`'s first; no token is shown.
    pub async fn connections(&self) -> sqlx::Result<Vec<Connection>> {
        let (_, overlay) = self.overlay().await?;
        let view = |name: &str, source: &Source, from| Connection {
            name: name.to_owned(),
            from,
            server: source.server.clone(),
            scope: source.scope.clone(),
            tools: source.tools.clone(),
            authoritative: source.authoritative,
            has_token: !source.headers.is_empty(),
        };
        let mut out: Vec<Connection> = self
            .ctx
            .config_sources
            .iter()
            .map(|(name, source)| view(name, source, "config"))
            .collect();
        out.extend(
            overlay
                .sources
                .iter()
                .filter(|(name, _)| !self.ctx.config_sources.contains_key(*name))
                .map(|(name, source)| view(name, source, "console")),
        );
        Ok(out)
    }

    /// Connects a source, or changes one the console connected; a new revision of the layer.
    /// The source is read from the next pass on.
    pub async fn connect(
        &self,
        actor: &str,
        name: &str,
        input: NewConnection,
    ) -> Result<i64, ConnectError> {
        if !valid_name(name) {
            return Err(ConnectError::Invalid(format!(
                "{name:?} must be up to 40 lowercase letters, digits and dashes, starting with a letter"
            )));
        }
        if self.ctx.config_sources.contains_key(name) {
            return Err(ConnectError::InConfig(name.to_owned()));
        }
        let server = input.server.trim().to_owned();
        let parsed = url::Url::parse(&server)
            .map_err(|_| ConnectError::Invalid(format!("{server:?} is not an address")))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(ConnectError::Invalid(format!(
                "{server:?} is not an http or https address"
            )));
        }
        let (_, mut overlay) = self.overlay().await?;
        let previous = overlay.sources.get(name).cloned();
        let header = input
            .header
            .filter(|h| !h.trim().is_empty())
            .unwrap_or_else(|| "Authorization".to_owned());
        let scheme = input.scheme.unwrap_or_else(|| {
            if header.eq_ignore_ascii_case("authorization") {
                "Bearer ".to_owned()
            } else {
                String::new()
            }
        });
        let mut tx = self.ctx.pool.begin().await?;
        let headers = match input
            .token
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            Some(token) => {
                let sealed = self.ctx.vault.seal(token)?;
                sqlx::query(
                    "INSERT INTO vault (name, sealed, updated_by) VALUES ($1, $2, $3) \
                     ON CONFLICT (name) DO UPDATE SET sealed = excluded.sealed, \
                     updated_by = excluded.updated_by, updated_at = now()",
                )
                .bind(vault_name(name))
                .bind(&sealed)
                .bind(actor)
                .execute(&mut *tx)
                .await?;
                BTreeMap::from([(
                    header,
                    format!("{scheme}{SECRET_REF}{}}}", vault_name(name)),
                )])
            }
            None => previous.map(|p| p.headers).unwrap_or_default(),
        };
        let source = Source {
            uses: "mcp".to_owned(),
            server: Some(server),
            headers,
            connection: None,
            scope: input
                .scope
                .filter(|s| !s.trim().is_empty())
                .map(|s| json!(s.trim())),
            retain: None,
            authoritative: input.authoritative,
            tools: input.tools.filter(|t| !t.is_empty()),
        };
        overlay.sources.insert(name.to_owned(), source);
        let revision = record(&mut tx, actor, &overlay, &format!("connected {name}")).await?;
        tx.commit().await?;
        self.reload_sources().await?;
        Ok(revision)
    }

    /// Takes a source the console connected away, with its stored token.
    pub async fn disconnect(&self, actor: &str, name: &str) -> Result<i64, ConnectError> {
        if self.ctx.config_sources.contains_key(name) {
            return Err(ConnectError::InConfig(name.to_owned()));
        }
        let (_, mut overlay) = self.overlay().await?;
        if overlay.sources.remove(name).is_none() {
            return Err(ConnectError::NotFound(name.to_owned()));
        }
        let mut tx = self.ctx.pool.begin().await?;
        sqlx::query("DELETE FROM vault WHERE name = $1")
            .bind(vault_name(name))
            .execute(&mut *tx)
            .await?;
        let revision = record(&mut tx, actor, &overlay, &format!("disconnected {name}")).await?;
        tx.commit().await?;
        self.reload_sources().await?;
        Ok(revision)
    }

    /// A source as agents get it now, its tokens opened.
    pub fn source(&self, name: &str) -> Option<Source> {
        self.ctx.sources.now().get(name).cloned()
    }

    /// Whether the console can store tokens.
    pub fn vault_open(&self) -> bool {
        self.ctx.vault.is_open()
    }
}

async fn record(
    tx: &mut sqlx::PgConnection,
    actor: &str,
    overlay: &Overlay,
    note: &str,
) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "INSERT INTO config_overlay (data, changed_by, note) VALUES ($1, $2, $3) RETURNING revision",
    )
    .bind(serde_json::to_value(overlay).unwrap_or_default())
    .bind(actor)
    .bind(note)
    .fetch_one(&mut *tx)
    .await
}

pub use crate::mcp::{Checked, ToolInfo, check};
