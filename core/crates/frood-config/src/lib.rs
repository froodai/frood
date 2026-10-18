//! Installation config (`frood.yaml`): parsing, `${secret:...}` / `${env:...}` interpolation,
//! validation and JSON Schema. Design: docs/16_platform.md §5.

mod interpolate;
mod model;

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

pub use interpolate::Secrets;
pub use model::{
    Auth, Config, Deploy, Installation, Policies, Provider, Server, ServerPlacement, Storage,
};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("frood.yaml is not valid YAML: {0}")]
    Syntax(String),
    #[error("frood.yaml does not match the schema: {0}")]
    Schema(String),
    #[error("frood.yaml has problems:\n  - {}", .0.join("\n  - "))]
    Invalid(Vec<String>),
}

/// A parsed and validated config together with what is needed to record it as a revision.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub config: Config,
    /// The file exactly as written, with references unresolved. Safe to store: no secret values.
    pub raw: String,
    /// SHA-256 of `raw`, hex-encoded.
    pub sha256: String,
    /// Names of the secrets the config references.
    pub secrets_used: BTreeSet<String>,
    /// Non-fatal notes, e.g. sections reserved for later milestones.
    pub warnings: Vec<String>,
    /// Directory of the config file; relative paths in the config resolve against it.
    pub base_dir: PathBuf,
}

/// Default secrets file for a config: `.frood/secrets.env` next to it.
pub fn default_secrets_path(config_path: &Path) -> PathBuf {
    base_dir_of(config_path).join(".frood").join("secrets.env")
}

pub fn load(config_path: &Path, secrets_path: Option<&Path>) -> Result<Loaded, ConfigError> {
    let raw = std::fs::read_to_string(config_path).map_err(|source| ConfigError::Io {
        path: config_path.to_owned(),
        source,
    })?;
    let secrets_path =
        secrets_path.map_or_else(|| default_secrets_path(config_path), Path::to_owned);
    let secrets = Secrets::from_file(&secrets_path)?;
    let mut loaded = parse(&raw, &secrets)?;
    loaded.base_dir = base_dir_of(config_path);
    Ok(loaded)
}

pub fn parse(raw: &str, secrets: &Secrets) -> Result<Loaded, ConfigError> {
    let mut document: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(raw).map_err(|e| ConfigError::Syntax(e.to_string()))?;
    let secrets_used = interpolate::interpolate(&mut document, secrets)?;
    let config: Config =
        serde_yaml_ng::from_value(document).map_err(|e| ConfigError::Schema(e.to_string()))?;
    validate(&config)?;

    let warnings = config
        .reserved_present()
        .into_iter()
        .map(|section| {
            let milestone = model::RESERVED_SECTIONS
                .iter()
                .find(|(name, _)| *name == section)
                .map_or("a later milestone", |(_, m)| m);
            format!("section `{section}` is not used yet (arrives in {milestone}); it is ignored")
        })
        .collect();

    Ok(Loaded {
        config,
        raw: raw.to_owned(),
        sha256: hex::encode(Sha256::digest(raw.as_bytes())),
        secrets_used,
        warnings,
        base_dir: PathBuf::from("."),
    })
}

fn validate(config: &Config) -> Result<(), ConfigError> {
    let mut problems = Vec::new();
    if config.version != 1 {
        problems.push(format!(
            "version {} is not supported; use 1",
            config.version
        ));
    }
    if !is_valid_name(&config.installation.name) {
        problems.push(format!(
            "installation.name {:?} must be 1–40 lowercase letters, digits or dashes, starting with a letter",
            config.installation.name
        ));
    }
    if config.server.listen.parse::<SocketAddr>().is_err() {
        problems.push(format!(
            "server.listen {:?} is not an address like 127.0.0.1:8080",
            config.server.listen
        ));
    }
    if let Some(url) = &config.server.public_url
        && !url.starts_with("https://")
    {
        problems.push(format!(
            "server.public_url {url:?} must start with https://"
        ));
    }
    if config.storage.uses != "postgres" {
        problems.push(format!(
            "storage.uses {:?} is not available yet; only `postgres` exists",
            config.storage.uses
        ));
    }
    if !config.storage.url.starts_with("postgres://")
        && !config.storage.url.starts_with("postgresql://")
    {
        problems.push("storage.url must be a postgres:// URL".to_owned());
    }
    if let Some(threshold) = config.policies.entity_merge_threshold
        && !(0.0..=1.0).contains(&threshold)
    {
        problems.push(format!(
            "policies.entity_merge_threshold {threshold} must be between 0 and 1"
        ));
    }
    validate_auth(config, &mut problems);
    if config.deploy.target != "compose" {
        problems.push(format!(
            "deploy.target {:?} is not available yet; only `compose` exists",
            config.deploy.target
        ));
    }
    if config.deploy.server == ServerPlacement::Container
        && config.deploy.image.is_none()
        && config.deploy.build.is_none()
    {
        problems.push(
            "deploy.server is `container` but neither deploy.image nor deploy.build is set"
                .to_owned(),
        );
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(ConfigError::Invalid(problems))
    }
}

fn validate_auth(config: &Config, problems: &mut Vec<String>) {
    let auth = &config.auth;
    let mut ids = BTreeSet::new();
    let loopback_only = config.server.public_url.is_none()
        && config
            .server
            .listen
            .parse::<SocketAddr>()
            .is_ok_and(|a| a.ip().is_loopback());
    for provider in &auth.providers {
        let id = provider.id();
        if !is_valid_name(id) {
            problems.push(format!(
                "auth provider id {id:?} must be lowercase letters, digits or dashes"
            ));
        }
        if !ids.insert(id.to_owned()) {
            problems.push(format!("auth provider id {id:?} is used twice"));
        }
        match provider {
            Provider::Oidc { issuer, client_id, client_secret, .. } => {
                if !(issuer.starts_with("https://") || is_loopback_http(issuer)) {
                    problems.push(format!("auth provider {id}: issuer must be an https:// URL"));
                }
                if client_id.is_empty() || client_secret.is_empty() {
                    problems.push(format!("auth provider {id}: client_id and client_secret are required"));
                }
            }
            Provider::Dev { .. } if !loopback_only => problems.push(format!(
                "auth provider {id}: the dev provider works only on a loopback server.listen without server.public_url"
            )),
            Provider::Dev { .. } => {}
        }
    }
    for domain in &auth.allow_domains {
        if domain.is_empty() || domain.contains('@') || domain.starts_with('.') {
            problems.push(format!(
                "auth.allow_domains entry {domain:?} must be a bare domain like company.com"
            ));
        }
    }
    for email in &auth.allow {
        if !email.contains('@') {
            problems.push(format!(
                "auth.allow entry {email:?} is not an email address"
            ));
        }
    }
    if auth.access_token_minutes == 0 || auth.refresh_token_days == 0 {
        problems.push("auth token lifetimes must be positive".to_owned());
    }
}

fn is_loopback_http(url: &str) -> bool {
    ["http://127.0.0.1", "http://localhost", "http://[::1]"]
        .iter()
        .any(|prefix| url.starts_with(prefix))
}

fn is_valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && name.len() <= 40
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn base_dir_of(config_path: &Path) -> PathBuf {
    match config_path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.to_owned(),
        _ => PathBuf::from("."),
    }
}

/// Options for [`template`].
#[derive(Debug, Clone)]
pub struct TemplateOptions<'a> {
    pub name: &'a str,
    pub server: ServerPlacement,
    /// Build the server image from this path instead of pulling `image`.
    pub build: Option<&'a str>,
    pub image: &'a str,
    pub postgres_port: u16,
}

/// The starter `frood.yaml` written by `frood init`.
pub fn template(options: &TemplateOptions<'_>) -> String {
    let listen = match options.server {
        ServerPlacement::Container => "0.0.0.0:8080",
        ServerPlacement::Host => "127.0.0.1:8080",
    };
    let image_or_build = match (options.server, options.build) {
        (ServerPlacement::Host, _) => String::new(),
        (ServerPlacement::Container, Some(path)) => format!("  build: {path}\n"),
        (ServerPlacement::Container, None) => format!("  image: {}\n", options.image),
    };
    include_str!("template.yaml")
        .replace("__NAME__", options.name)
        .replace("__LISTEN__", listen)
        .replace("__SERVER__", options.server.as_str())
        .replace("__IMAGE_OR_BUILD__", &image_or_build)
        .replace("__PG_PORT__", &options.postgres_port.to_string())
}

/// JSON Schema of `frood.yaml`, for editors and `frood config schema`.
pub fn json_schema() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(Config)).expect("schema serializes to JSON")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secrets() -> Secrets {
        Secrets::from_pairs([("POSTGRES_PASSWORD", "pw")])
    }

    fn template_for(server: ServerPlacement) -> String {
        template(&TemplateOptions {
            name: "personal",
            server,
            build: None,
            image: "ghcr.io/froodai/frood:0.0.1",
            postgres_port: 5432,
        })
    }

    #[test]
    fn templates_parse_and_validate() {
        for server in [ServerPlacement::Host, ServerPlacement::Container] {
            let loaded = parse(&template_for(server), &secrets()).unwrap();
            assert_eq!(loaded.config.installation.name, "personal");
            assert_eq!(loaded.config.deploy.server, server);
            assert!(loaded.config.storage.url.contains(":pw@"));
            assert!(loaded.secrets_used.contains("POSTGRES_PASSWORD"));
            assert!(loaded.warnings.is_empty());
        }
    }

    #[test]
    fn raw_text_keeps_references_unresolved() {
        let loaded = parse(&template_for(ServerPlacement::Host), &secrets()).unwrap();
        assert!(loaded.raw.contains("${secret:POSTGRES_PASSWORD}"));
        assert!(!loaded.raw.contains(":pw@"));
        assert_eq!(loaded.sha256.len(), 64);
    }

    #[test]
    fn rejects_unknown_fields() {
        let raw = template_for(ServerPlacement::Host).replace("installation:", "instalation:");
        assert!(matches!(
            parse(&raw, &secrets()),
            Err(ConfigError::Schema(_))
        ));
    }

    #[test]
    fn collects_semantic_problems() {
        let raw = template_for(ServerPlacement::Host)
            .replace("name: personal", "name: Personal_1")
            .replace("uses: postgres", "uses: mongodb");
        match parse(&raw, &secrets()) {
            Err(ConfigError::Invalid(problems)) => {
                assert!(problems.iter().any(|p| p.contains("installation.name")));
                assert!(problems.iter().any(|p| p.contains("storage.uses")));
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn container_needs_image_or_build() {
        let raw = template_for(ServerPlacement::Container)
            .replace("  image: ghcr.io/froodai/frood:0.0.1\n", "");
        assert!(matches!(
            parse(&raw, &secrets()),
            Err(ConfigError::Invalid(_))
        ));
    }

    #[test]
    fn reserved_sections_warn() {
        let raw = format!(
            "{}\njobs:\n  digest: {{}}\n",
            template_for(ServerPlacement::Host)
        );
        let loaded = parse(&raw, &secrets()).unwrap();
        assert_eq!(loaded.warnings.len(), 1);
        assert!(loaded.warnings[0].contains("`jobs`") && loaded.warnings[0].contains("M4"));
    }

    #[test]
    fn missing_secret_is_reported() {
        let err = parse(&template_for(ServerPlacement::Host), &Secrets::default()).unwrap_err();
        // The variable may exist in a developer's environment; only assert when it does not.
        if std::env::var("POSTGRES_PASSWORD").is_err() {
            assert!(err.to_string().contains("POSTGRES_PASSWORD"));
        }
    }

    #[test]
    fn schema_mentions_top_level_sections() {
        let schema = json_schema().to_string();
        for key in ["installation", "storage", "deploy", "jobs"] {
            assert!(schema.contains(key), "schema lacks {key}");
        }
    }

    #[test]
    fn load_resolves_secrets_file_next_to_config() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("frood.yaml");
        std::fs::write(&config_path, template_for(ServerPlacement::Host)).unwrap();
        std::fs::create_dir(dir.path().join(".frood")).unwrap();
        std::fs::write(
            dir.path().join(".frood/secrets.env"),
            "POSTGRES_PASSWORD=fromfile\n",
        )
        .unwrap();
        let loaded = load(&config_path, None).unwrap();
        assert!(loaded.config.storage.url.contains(":fromfile@"));
        assert_eq!(loaded.base_dir, dir.path());
    }

    #[test]
    fn policies_default_and_override() {
        let loaded = parse(&template_for(ServerPlacement::Host), &secrets()).unwrap();
        assert_eq!(loaded.config.policies, Policies::default());

        let raw = format!(
            "{}\npolicies:\n  auto_accept_own_facts: false\n  entity_merge_threshold: null\n",
            template_for(ServerPlacement::Host)
        );
        let policies = parse(&raw, &secrets()).unwrap().config.policies;
        assert!(!policies.auto_accept_own_facts);
        assert_eq!(policies.entity_merge_threshold, None);

        let raw = format!(
            "{}\npolicies:\n  entity_merge_threshold: 1.5\n",
            template_for(ServerPlacement::Host)
        );
        assert!(matches!(
            parse(&raw, &secrets()),
            Err(ConfigError::Invalid(_))
        ));
    }
}
