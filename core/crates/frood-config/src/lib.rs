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
    Account, AccountBudget, AskUser, Attention, AttentionPerson, AttentionRoute, Auth, AutoAccept,
    Backup, BackupKeep, BackupTarget, BeforeNotify, Budget, Config, Deploy, Detectors, Embeddings,
    Installation, Job, ModelProvider, Module, Objects, OnExpire, Policies, Provider, Proxy,
    Reminder, Search, Server, ServerPlacement, SituationMatch, Source, Sphere, Storage, Trigger,
};
pub use model::{BUILT_IN_SPHERES, default_reminders};

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

    let mut warnings: Vec<String> = config
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
    warnings.extend(backup_warnings(&config));

    Ok(Loaded {
        config,
        raw: raw.to_owned(),
        sha256: hex::encode(Sha256::digest(raw.as_bytes())),
        secrets_used,
        warnings,
        base_dir: PathBuf::from("."),
    })
}

/// Checks a config built in code, as `parse` checks one read from a file: the console's
/// changes go through the same rules as `frood.yaml`.
pub fn check(config: &Config) -> Result<(), ConfigError> {
    validate(config)
}

fn validate(config: &Config) -> Result<(), ConfigError> {
    let mut problems = Vec::new();
    if config.version != 1 {
        problems.push(format!(
            "version {} is not supported; use 1",
            config.version
        ));
    }
    if let Some(zone) = &config.installation.timezone
        && zone.parse::<chrono_tz::Tz>().is_err()
    {
        problems.push(format!(
            "installation.timezone {zone:?} is not an IANA zone like Europe/Amsterdam"
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
    if let Some(url) = &config.server.public_url {
        match url::Url::parse(url) {
            _ if !url.starts_with("https://") => problems.push(format!(
                "server.public_url {url:?} must start with https://"
            )),
            Ok(parsed) if parsed.query().is_none() && parsed.fragment().is_none() => {}
            _ => problems.push(format!(
                "server.public_url {url:?} must be an address like https://example.com or https://example.com/frood"
            )),
        }
    }
    if let Some(proxy) = &config.deploy.proxy {
        if proxy.routes.is_some() && config.server.public_url.is_none() {
            problems.push(
                "deploy.proxy.routes needs server.public_url: Caddy runs only for a public address"
                    .to_owned(),
            );
        }
        match proxy.external.as_deref() {
            Some("") => problems.push("deploy.proxy.external must name a file".to_owned()),
            Some(_) if config.server.public_url.is_none() => problems.push(
                "deploy.proxy.external needs server.public_url: the proxy in front serves that address"
                    .to_owned(),
            ),
            Some(_) if proxy.routes.is_some() => problems.push(
                "deploy.proxy: routes and external do not go together; with a proxy in front, other apps keep their own files there"
                    .to_owned(),
            ),
            _ => {}
        }
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
    validate_objects(&config.storage.objects, &mut problems);
    if let Some(threshold) = config.policies.entity_merge_threshold
        && !(0.0..=1.0).contains(&threshold)
    {
        problems.push(format!(
            "policies.entity_merge_threshold {threshold} must be between 0 and 1"
        ));
    }
    validate_spheres(config, &mut problems);
    validate_auth(config, &mut problems);
    validate_work(config, &mut problems);
    validate_backup(config, &mut problems);
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

fn validate_objects(objects: &model::Objects, problems: &mut Vec<String>) {
    let (limit, most) = match objects {
        model::Objects::Database { .. } => (objects.max_mb(), 512),
        _ => (objects.max_mb(), 4096),
    };
    if limit == 0 || limit > most {
        problems.push(format!(
            "storage.objects.max_mb {limit} must be between 1 and {most}"
        ));
    }
    if let model::Objects::S3 {
        endpoint, bucket, ..
    } = objects
    {
        if !endpoint.starts_with("https://")
            && !is_loopback_http(endpoint)
            && !is_internal_http(endpoint)
        {
            problems.push(format!(
                "storage.objects.endpoint {endpoint:?} must be an https:// address"
            ));
        }
        if bucket.trim().is_empty() {
            problems.push("storage.objects.bucket must not be empty".to_owned());
        }
    }
}

fn validate_spheres(config: &Config, problems: &mut Vec<String>) {
    let mut seen = BTreeSet::new();
    for sphere in &config.spheres {
        let id = &sphere.id;
        if !is_valid_name(id) || id.len() > 32 || id == "none" {
            problems.push(format!(
                "spheres: {id:?} must be up to 32 lowercase letters, digits and dashes, starting with a letter, and not `none`"
            ));
        }
        if !seen.insert(id.as_str()) {
            problems.push(format!("spheres: {id:?} is listed twice"));
        }
        if !sphere.is_built_in() && (sphere.name.is_none() || sphere.about.is_none()) {
            problems.push(format!(
                "spheres: {id:?} needs `name` (what people see) and `about` (what belongs there, for the model)"
            ));
        }
    }
}

fn validate_backup(config: &Config, problems: &mut Vec<String>) {
    let Some(backup) = &config.backup else {
        for job in ["backup", "backup-verify"] {
            if config.jobs.contains_key(job) {
                problems.push(format!(
                    "job {job}: there is no `backup` section saying where backups go"
                ));
            }
        }
        return;
    };
    let key = backup.recovery_key.trim();
    if !key.starts_with("age1") || key.len() < 20 {
        problems.push(
            "backup.recovery_key must be the public half of an age key (age1…); `frood backup key` makes one"
                .to_owned(),
        );
    }
    if backup.targets.is_empty() {
        problems.push("backup.targets is empty: name where backups go".to_owned());
    }
    for (name, target) in &backup.targets {
        if !is_valid_name(name) {
            problems.push(format!(
                "backup target name {name:?} must be lowercase letters, digits or dashes"
            ));
        }
        if target.repository.trim().is_empty() {
            problems.push(format!("backup target {name}: repository is empty"));
        }
        if target.password.is_empty() {
            problems.push(format!("backup target {name}: password is empty"));
        }
    }
    let rules = backup.keep.rules();
    if rules.is_empty() {
        problems.push(
            "backup.keep says nothing: name what to keep, e.g. { daily: 30, monthly: 12 }"
                .to_owned(),
        );
    }
    if rules.iter().any(|(_, n)| *n == 0) {
        problems.push("backup.keep: every number must be at least 1".to_owned());
    }
    if let Some(module) = &backup.module
        && !config.modules.contains_key(module)
    {
        problems.push(format!("backup.module {module} is not in modules"));
    }
}

/// What `frood plan` should point out about backups, though the config is valid.
fn backup_warnings(config: &Config) -> Vec<String> {
    let Some(backup) = &config.backup else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if backup.targets.len() == 1 {
        out.push(
            "backup has one target: a second, independent one protects against losing it"
                .to_owned(),
        );
    }
    let scheduled = |job: &str| {
        config
            .jobs
            .get(job)
            .and_then(|j| j.on.as_ref())
            .is_some_and(|on| on.iter().any(|t| t.schedule.is_some()))
    };
    if !scheduled("backup") {
        out.push("backups run only when started by hand: give jobs.backup a schedule".to_owned());
    }
    if !scheduled("backup-verify") {
        out.push(
            "restoring from backups is never checked: give jobs.backup-verify a schedule"
                .to_owned(),
        );
    }
    out
}

fn validate_auth(config: &Config, problems: &mut Vec<String>) {
    let auth = &config.auth;
    let mut ids = BTreeSet::new();
    // A server in a container listens on all interfaces inside it, but compose publishes the
    // port on 127.0.0.1 only, so it is as local as a loopback listen address.
    let loopback_only = config.server.public_url.is_none()
        && (config.deploy.server == ServerPlacement::Container
            || config
                .server
                .listen
                .parse::<SocketAddr>()
                .is_ok_and(|a| a.ip().is_loopback()));
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
                "auth provider {id}: the dev provider works only on a local server (loopback server.listen or a container) without server.public_url"
            )),
            Provider::Local { .. } if !loopback_only => problems.push(format!(
                "auth provider {id}: sign-in on this computer works only without server.public_url, on a loopback address or in a container; a server signs in through Google or another OIDC provider"
            )),
            Provider::Local { .. } if auth.allow.is_empty() => problems.push(format!(
                "auth provider {id}: sign-in on this computer needs your address in auth.allow"
            )),
            Provider::Telegram { .. } if auth.allow.is_empty() => problems.push(format!(
                "auth provider {id}: sign-in through Telegram needs the addresses that may sign in in auth.allow"
            )),
            Provider::Telegram { channel, .. } if !is_valid_name(channel) => problems.push(format!(
                "auth provider {id}: channel {channel:?} must be lowercase letters, digits or dashes"
            )),
            Provider::Local { .. } | Provider::Telegram { .. } | Provider::Dev { .. } => {}
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

/// Accounts, modules, jobs and search (M3).
fn validate_work(config: &Config, problems: &mut Vec<String>) {
    for (name, account) in &config.accounts {
        if !is_valid_name(name) {
            problems.push(format!(
                "account name {name:?} must be lowercase letters, digits or dashes"
            ));
        }
        match account {
            Account::ApiKey {
                provider,
                secret,
                base_url,
                workspace,
                ..
            } => {
                if secret.trim().is_empty() {
                    problems.push(format!("account {name}: secret is empty"));
                }
                if let Some(workspace) = workspace
                    && (*provider != ModelProvider::Anthropic || !workspace.starts_with("wrkspc_"))
                {
                    problems.push(format!(
                        "account {name}: workspace is an Anthropic workspace id like wrkspc_…, got {workspace:?}"
                    ));
                }
                if let Some(url) = base_url
                    && !(url.starts_with("https://") || is_loopback_http(url))
                {
                    problems.push(format!("account {name}: base_url must be an https:// URL"));
                }
            }
            Account::Subscription { owner, secret, .. } => {
                if secret.trim().is_empty() {
                    problems.push(format!("account {name}: secret is empty"));
                }
                if !owner.contains('@') {
                    problems.push(format!(
                        "account {name}: owner is the subscriber's email, got {owner:?}"
                    ));
                }
            }
        }
        if let Some(budget) = account.budget()
            && !(budget.monthly_usd.is_finite() && budget.monthly_usd >= 0.0)
        {
            problems.push(format!(
                "account {name}: budget.monthly_usd must be a sum in dollars"
            ));
        }
    }
    let mut ports = BTreeSet::new();
    for (name, module) in &config.modules {
        if !is_valid_name(name) {
            problems.push(format!(
                "module name {name:?} must be lowercase letters, digits or dashes"
            ));
        }
        let sources = [&module.image, &module.build, &module.url]
            .iter()
            .filter(|s| s.is_some())
            .count();
        if sources != 1 {
            problems.push(format!(
                "module {name}: set exactly one of image, build or url"
            ));
        }
        if let Some(url) = &module.url
            && !(url.starts_with("http://") || url.starts_with("https://"))
        {
            problems.push(format!("module {name}: url must be http:// or https://"));
        }
        for secret in &module.secrets {
            if secret.is_empty()
                || !secret
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            {
                problems.push(format!(
                    "module {name}: secret name {secret:?} must be like TELEGRAM_BOT_TOKEN"
                ));
            }
        }
        if let Some(port) = module.port
            && !ports.insert(port)
        {
            problems.push(format!("module {name}: port {port} is used twice"));
        }
        for volume in &module.volumes {
            let ok = volume
                .split_once(':')
                .is_some_and(|(host, inside)| !host.is_empty() && inside.starts_with('/'));
            if !ok {
                problems.push(format!(
                    "module {name}: volume {volume:?} must be host-path:/path/in/module"
                ));
            }
        }
    }
    for (name, job) in &config.jobs {
        if !is_valid_name(name) {
            problems.push(format!(
                "job name {name:?} must be lowercase letters, digits or dashes"
            ));
        }
        if let Some(account) = job.with.get("account") {
            match account.as_str().and_then(|a| config.accounts.get(a)) {
                Some(Account::ApiKey { .. }) => {}
                Some(Account::Subscription { .. }) => problems.push(format!(
                    "job {name}: account {account} is a subscription, which only agent jobs may use; use an api_key account"
                )),
                None => problems.push(format!("job {name}: account {account} is not in accounts")),
            }
        }
        if let Some(module) = job.with.get("module") {
            match module.as_str() {
                Some(module) if config.modules.contains_key(module) => {}
                _ => problems.push(format!("job {name}: module {module} is not in modules")),
            }
        }
        for trigger in job.on.iter().flatten() {
            validate_trigger(name, trigger, problems);
        }
        match job.account.as_ref().map(|a| (a, config.accounts.get(a))) {
            Some((account, None)) => {
                problems.push(format!("job {name}: account {account} is not in accounts"));
            }
            Some((account, Some(subscription @ Account::Subscription { .. }))) => {
                validate_subscription_use(name, job, account, subscription, problems);
            }
            _ => {}
        }
        if let Some(runtime) = &job.runtime
            && !config.modules.contains_key(runtime)
        {
            problems.push(format!("job {name}: runtime {runtime} is not in modules"));
        }
        for permission in job.permissions.iter().flatten() {
            if ![
                "knowledge:read",
                "knowledge:capture",
                "knowledge:propose",
                "journal:read",
            ]
            .contains(&permission.as_str())
            {
                problems.push(format!(
                    "job {name}: agents may hold knowledge:read, capture, propose and journal:read, not {permission}"
                ));
            }
        }
        for tool in job.tools.iter().flatten() {
            match tool.strip_prefix("source:") {
                _ if tool == "frood" => {}
                Some("${source}") => {}
                Some(source) if config.sources.contains_key(source) => {}
                Some(source) => problems.push(format!(
                    "job {name}: tool {tool} names source {source}, which is not in sources"
                )),
                None => problems.push(format!(
                    "job {name}: tool {tool:?} is not frood or source:<name>"
                )),
            }
        }
        if job
            .on
            .iter()
            .flatten()
            .any(|t| t.for_each.as_deref() == Some("source"))
            && config.sources.is_empty()
        {
            problems.push(format!(
                "job {name}: for_each: source, but there are no sources"
            ));
        }
        if let Some(auto) = &job.auto_accept {
            if auto.when != "authoritative_source" {
                problems.push(format!(
                    "job {name}: auto_accept.when can only be authoritative_source, not {:?}",
                    auto.when
                ));
            }
            if !auto.no_conflict {
                problems.push(format!(
                    "job {name}: auto_accept.no_conflict must stay true: conflicts always wait for a person"
                ));
            }
        }
        if let Some(expire) = job
            .ask_user
            .as_ref()
            .and_then(|a| a.expire_after.as_deref())
            && humantime::parse_duration(expire).is_err()
        {
            problems.push(format!(
                "job {name}: ask_user.expire_after {expire:?} is not a duration like 3d or 12h"
            ));
        }
    }
    for (name, source) in &config.sources {
        if !is_valid_name(name) {
            problems.push(format!(
                "source name {name:?} must be lowercase letters, digits or dashes"
            ));
        }
        if source.uses != "mcp" {
            problems.push(format!(
                "source {name}: uses {:?} is not available yet; this version reads MCP servers (uses: mcp)",
                source.uses
            ));
        }
        match &source.server {
            None => problems.push(format!("source {name}: server is the MCP server's URL")),
            Some(url) if !(url.starts_with("https://") || is_internal_http(url)) => {
                problems.push(format!(
                    "source {name}: server must be an https:// URL (plain http only inside the deployment)"
                ));
            }
            Some(_) => {}
        }
        if source.connection.is_some() {
            problems.push(format!(
                "source {name}: personal connections come later; give a service account's token in headers"
            ));
        }
        for tool in source.tools.iter().flatten() {
            if tool.is_empty()
                || !tool
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                problems.push(format!(
                    "source {name}: tool {tool:?} must be a tool name like search_issues"
                ));
            }
        }
        if let Some(retain) = &source.retain
            && retain != "excerpts"
            && retain != "full"
        {
            problems.push(format!(
                "source {name}: retain is excerpts or full, not {retain:?}"
            ));
        }
    }
    validate_attention(config, problems);
    if let Some(embeddings) = &config.search.embeddings {
        match config.accounts.get(&embeddings.account) {
            None => problems.push(format!(
                "search.embeddings.account {:?} is not in accounts",
                embeddings.account
            )),
            Some(account) if account.provider() == ModelProvider::Anthropic => problems.push(
                "search.embeddings: Anthropic has no embeddings API; use an openai account"
                    .to_owned(),
            ),
            Some(Account::Subscription { .. }) => problems.push(
                "search.embeddings: a subscription cannot be used for embeddings; use an api_key account"
                    .to_owned(),
            ),
            Some(_) => {}
        }
    }
}

/// A subscription is the owner's: only an agent job that runs the vendor's own agent on the
/// owner's behalf may use it.
fn validate_subscription_use(
    name: &str,
    job: &Job,
    account: &str,
    subscription: &Account,
    problems: &mut Vec<String>,
) {
    let provider = subscription.provider();
    let owner = subscription.owner().unwrap_or_default();
    // Standard jobs leave `uses` out; the dispatcher refuses a subscription for non-agent ones.
    if job
        .uses
        .as_deref()
        .is_some_and(|uses| uses != "agents.execute")
    {
        problems.push(format!(
            "job {name}: account {account} is a subscription, which only agent jobs may use"
        ));
    }
    if let Some(agent) = &job.agent
        && agent != provider.own_agent()
    {
        problems.push(format!(
            "job {name}: account {account} is a subscription, usable only inside {}, not {agent}",
            provider.own_agent()
        ));
    }
    if !job
        .on_behalf_of
        .as_deref()
        .is_some_and(|who| who.eq_ignore_ascii_case(owner))
    {
        problems.push(format!(
            "job {name}: account {account} is {owner}'s subscription; set on_behalf_of: {owner}"
        ));
    }
}

/// "HH:MM-HH:MM" as two times of day.
pub fn parse_quiet_hours(value: &str) -> Option<(chrono::NaiveTime, chrono::NaiveTime)> {
    let (from, to) = value.split_once('-')?;
    let time = |t: &str| chrono::NaiveTime::parse_from_str(t.trim(), "%H:%M").ok();
    Some((time(from)?, time(to)?))
}

fn validate_situation(place: &str, when: &model::SituationMatch, problems: &mut Vec<String>) {
    if !["proposal", "question", "reminder", "alert"].contains(&when.kind.as_str()) {
        problems.push(format!(
            "{place}: kind is proposal, question, reminder or alert, not {:?}",
            when.kind
        ));
    }
    if let Some(risk) = &when.risk
        && (when.kind != "proposal"
            || ![
                "new",
                "change",
                "conflict",
                "stale_knowledge",
                "entity_merge",
            ]
            .contains(&risk.as_str()))
    {
        problems.push(format!(
            "{place}: risk {risk:?} applies to proposals: new, change, conflict, stale_knowledge or entity_merge"
        ));
    }
}

fn validate_reminders(place: &str, reminders: &[model::Reminder], problems: &mut Vec<String>) {
    for (i, reminder) in reminders.iter().enumerate() {
        if !(-365..=365).contains(&reminder.days) {
            problems.push(format!(
                "{place}[{i}]: days is from -365 to 365 (days from the due date), not {}",
                reminder.days
            ));
        }
        if chrono::NaiveTime::parse_from_str(&reminder.at, "%H:%M").is_err() {
            problems.push(format!(
                "{place}[{i}]: at {:?} is a time of day like \"09:00\"",
                reminder.at
            ));
        }
    }
}

fn validate_attention(config: &Config, problems: &mut Vec<String>) {
    let attention = &config.attention;
    if let Some(reminders) = &attention.reminders {
        validate_reminders("attention.reminders", reminders, problems);
    }
    // People without a zone of their own take the installation's.
    let zone_known = config.installation.timezone.is_some();
    let detectors = &attention.detectors;
    for (name, value) in [
        ("dispatcher_lag", &detectors.dispatcher_lag),
        ("queued", &detectors.queued),
        ("overrun", &detectors.overrun),
        ("source_stale", &detectors.source_stale),
        ("module_down", &detectors.module_down),
    ] {
        if let Some(value) = value
            && humantime::parse_duration(value).is_err()
        {
            problems.push(format!(
                "attention.detectors.{name}: {value:?} is not a duration like 10m or 48h"
            ));
        }
    }
    if let Some(share) = detectors.budget_share
        && !(0.0..=1.0).contains(&share)
    {
        problems.push(format!(
            "attention.detectors.budget_share is a share from 0 to 1, not {share}"
        ));
    }
    for (email, person) in &attention.people {
        if let Some(reminders) = &person.reminders {
            validate_reminders(
                &format!("attention.people.{email}.reminders"),
                reminders,
                problems,
            );
            if person.timezone.is_none() && !zone_known && !reminders.is_empty() {
                problems.push(format!(
                    "attention.people.{email}: reminders need a timezone (set it, or installation.timezone)"
                ));
            }
        }
        if !email.contains('@') {
            problems.push(format!("attention.people: {email:?} is not an email"));
        }
        if let Some(zone) = &person.timezone
            && zone.parse::<chrono_tz::Tz>().is_err()
        {
            problems.push(format!(
                "attention.people.{email}: timezone {zone:?} is not an IANA zone like Europe/Amsterdam"
            ));
        }
        if let Some(quiet) = &person.quiet_hours {
            if parse_quiet_hours(quiet).is_none() {
                problems.push(format!(
                    "attention.people.{email}: quiet_hours {quiet:?} is not like \"23:00-08:00\""
                ));
            }
            if person.timezone.is_none() && !zone_known {
                problems.push(format!(
                    "attention.people.{email}: quiet hours need a timezone (set it, or installation.timezone)"
                ));
            }
        }
    }
    for (i, route) in attention.routes.iter().enumerate() {
        validate_situation(&format!("attention.routes[{i}]"), &route.when, problems);
        if !["now", "digest", "silent"].contains(&route.urgency.as_str()) {
            problems.push(format!(
                "attention.routes[{i}]: urgency is now, digest or silent, not {:?}",
                route.urgency
            ));
        }
    }
    for (i, rule) in attention.before_notify.iter().enumerate() {
        let place = format!("attention.before_notify[{i}]");
        validate_situation(&place, &rule.when, problems);
        if humantime::parse_duration(&rule.max_wait).is_err() {
            problems.push(format!(
                "{place}: max_wait {:?} is not a duration like 30m",
                rule.max_wait
            ));
        }
        let job = config.jobs.get(&rule.run);
        if rule.run != "investigate"
            && job.is_none_or(|j| j.uses.as_deref() != Some("agents.execute"))
        {
            problems.push(format!(
                "{place}: run names an agent job, and {} is not one",
                rule.run
            ));
        }
    }
}

fn validate_trigger(job: &str, trigger: &Trigger, problems: &mut Vec<String>) {
    match (&trigger.event, &trigger.schedule) {
        (Some(event), None) if !event.trim().is_empty() => {}
        (None, Some(schedule)) => {
            if schedule.parse::<croner::Cron>().is_err() {
                problems.push(format!(
                    "job {job}: schedule {schedule:?} is not a cron expression like \"0 3 * * *\""
                ));
            }
            if trigger.timezone.is_none() {
                problems.push(format!(
                    "job {job}: schedule {schedule:?} needs a timezone, like Europe/Amsterdam (there is no default)"
                ));
            }
        }
        _ => problems.push(format!(
            "job {job}: a trigger needs either an event or a schedule"
        )),
    }
    if let Some(zone) = &trigger.timezone
        && zone.parse::<chrono_tz::Tz>().is_err()
    {
        problems.push(format!(
            "job {job}: timezone {zone:?} is not an IANA zone like Europe/Amsterdam"
        ));
    }
    if let Some(each) = &trigger.for_each
        && each != "source"
    {
        problems.push(format!(
            "job {job}: for_each can only be `source`, not {each:?}"
        ));
    }
}

fn is_loopback_http(url: &str) -> bool {
    ["http://127.0.0.1", "http://localhost", "http://[::1]"]
        .iter()
        .any(|prefix| url.starts_with(prefix))
}

/// Plain HTTP to a loopback address or to a service inside the deployment (a host name without
/// dots, like `team-chat` in Compose).
fn is_internal_http(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let host = rest.split(['/', ':']).next().unwrap_or_default();
    is_loopback_http(url) || (!host.is_empty() && !host.contains('.'))
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
        .replace("__VERSION__", env!("CARGO_PKG_VERSION"))
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
            "{}\nruntimes:\n  local: {{}}\n",
            template_for(ServerPlacement::Host)
        );
        let loaded = parse(&raw, &secrets()).unwrap();
        assert_eq!(loaded.warnings.len(), 1);
        assert!(
            loaded.warnings[0].contains("`runtimes`")
                && loaded.warnings[0].contains("runtime module")
        );
    }

    const WORK: &str = "
accounts:
  anthropic: { kind: api_key, provider: anthropic, secret: '${secret:POSTGRES_PASSWORD}', workspace: wrkspc_01test }
  openai: { kind: api_key, provider: openai, secret: sk-test }
modules:
  stt: { build: ../modules/stt-openai, secrets: [OPENAI_API_KEY], config: { model: whisper-1 } }
  telegram: { image: 'ghcr.io/froodai/telegram:0.0.1', secrets: [TELEGRAM_BOT_TOKEN] }
jobs:
  extract: { with: { account: anthropic, model: claude-sonnet-5-5 } }
  transcribe: { with: { module: stt } }
  embed: { enabled: false }
search:
  embeddings: { account: openai, model: text-embedding-3-small }
";

    #[test]
    fn work_sections_parse() {
        let raw = format!("{}{WORK}", template_for(ServerPlacement::Host));
        let config = parse(&raw, &secrets()).unwrap().config;
        assert_eq!(
            config.accounts["anthropic"].provider(),
            ModelProvider::Anthropic
        );
        assert_eq!(
            config.accounts["anthropic"],
            Account::ApiKey {
                provider: ModelProvider::Anthropic,
                secret: "pw".into(),
                base_url: None,
                workspace: Some("wrkspc_01test".into()),
                budget: None,
            }
        );
        assert_eq!(config.modules["stt"].secrets, ["OPENAI_API_KEY"]);
        assert_eq!(config.jobs["extract"].with["model"], "claude-sonnet-5-5");
        assert_eq!(config.jobs["embed"].enabled, Some(false));
        assert_eq!(config.search.embeddings.unwrap().account, "openai");
    }

    #[test]
    fn agent_jobs_parse_and_are_checked() {
        let agent = "
sources:
  team-chat:
    uses: mcp
    server: https://mcp.example.com/mcp
    headers: { Authorization: 'Bearer ${secret:POSTGRES_PASSWORD}' }
    scope: { channels: ['#release'] }
    tools: [search_messages, read_thread]
    authoritative: true
jobs:
  daily-sweep:
    uses: agents.execute
    on:
      - { schedule: '0 3 * * *', timezone: Europe/Amsterdam, for_each: source }
    instructions: ./agents/daily-sweep.md
    tools: [frood, 'source:${source}']
    auto_accept: { when: authoritative_source }
    runtime: stt
    agent: claude-code
    model: claude-haiku-5-5
    account: anthropic
    permissions: [knowledge:read, knowledge:propose]
    on_behalf_of: alice@example.com
    ask_user: { allowed: true, expire_after: 3d, on_expire: skip }
    budget: { max_usd: 0.5, max_minutes: 15 }
";
        let raw =
            format!("{}{WORK}", template_for(ServerPlacement::Host)).replace("\njobs:\n", agent);
        let loaded = parse(&raw, &secrets()).unwrap();
        let job = &loaded.config.jobs["daily-sweep"];
        assert_eq!(
            job.on.as_ref().unwrap()[0].schedule.as_deref(),
            Some("0 3 * * *")
        );
        assert_eq!(job.ask_user.as_ref().unwrap().on_expire, OnExpire::Skip);
        assert_eq!(job.budget.as_ref().unwrap().max_minutes, Some(15));
        let source = &loaded.config.sources["team-chat"];
        assert_eq!(source.headers["Authorization"], "Bearer pw");
        assert!(source.authoritative);
        assert_eq!(
            source.tools.as_deref(),
            Some(&["search_messages".to_owned(), "read_thread".to_owned()][..])
        );

        let bad = raw
            .replace("uses: mcp", "uses: slack-events")
            .replace("when: authoritative_source", "when: always")
            .replace("'0 3 * * *'", "'every night'")
            .replace("Europe/Amsterdam", "Mars/Base")
            .replace("knowledge:propose]", "knowledge:decide]")
            .replace("expire_after: 3d", "expire_after: soon")
            .replace(
                "  daily-sweep:\n",
                "  other:\n    uses: agents.execute\n    on: [{ schedule: '0 * * * *' }]\n  daily-sweep:\n",
            );
        match parse(&bad, &secrets()) {
            Err(ConfigError::Invalid(problems)) => {
                for needle in [
                    "cron expression",
                    "IANA zone",
                    "not knowledge:decide",
                    "duration like",
                    "needs a timezone",
                    "this version reads MCP servers",
                    "auto_accept.when can only be",
                ] {
                    assert!(
                        problems.iter().any(|p| p.contains(needle)),
                        "{needle}: {problems:?}"
                    );
                }
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn subscriptions_serve_only_their_owner_inside_the_vendor_agent() {
        let raw = format!("{}{WORK}", template_for(ServerPlacement::Host))
            .replace(
                "  openai: { kind: api_key",
                "  max: { kind: subscription, provider: anthropic, owner: alice@example.com, secret: oauth, budget: { monthly_usd: 20 } }\n  openai: { kind: api_key",
            )
            .replace(
                "  embed: { enabled: false }\n",
                "  embed: { enabled: false }\n  mine:\n    uses: agents.execute\n    runtime: stt\n    agent: claude-code\n    \
                 model: claude-sonnet-5-5\n    account: max\n    instructions: Do my weekly review.\n    on_behalf_of: Alice@Example.com\n",
            );
        let config = parse(&raw, &secrets()).unwrap().config;
        assert_eq!(config.accounts["max"].owner(), Some("alice@example.com"));
        assert_eq!(config.accounts["max"].budget().unwrap().monthly_usd, 20.0);

        let bad = raw
            .replace("agent: claude-code", "agent: codex")
            .replace(
                "on_behalf_of: Alice@Example.com",
                "on_behalf_of: bob@example.com",
            )
            .replace("account: anthropic, model", "account: max, model");
        match parse(&bad, &secrets()) {
            Err(ConfigError::Invalid(problems)) => {
                for needle in [
                    "usable only inside claude-code, not codex",
                    "alice@example.com's subscription; set on_behalf_of",
                    "only agent jobs may use; use an api_key account",
                ] {
                    assert!(
                        problems.iter().any(|p| p.contains(needle)),
                        "{needle}: {problems:?}"
                    );
                }
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn work_sections_are_checked() {
        let raw = format!("{}{WORK}", template_for(ServerPlacement::Host))
            .replace("account: anthropic, model", "account: nobody, model")
            .replace(
                "embeddings: { account: openai,",
                "embeddings: { account: anthropic,",
            )
            .replace(
                "image: 'ghcr.io/froodai/telegram:0.0.1'",
                "image: x, url: 'http://127.0.0.1:9000'",
            );
        match parse(&raw, &secrets()) {
            Err(ConfigError::Invalid(problems)) => {
                assert!(
                    problems.iter().any(|p| p.contains("job extract: account")),
                    "{problems:?}"
                );
                assert!(
                    problems
                        .iter()
                        .any(|p| p.contains("Anthropic has no embeddings"))
                );
                assert!(
                    problems
                        .iter()
                        .any(|p| p.contains("module telegram: set exactly one"))
                );
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
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
    fn dev_sign_in_only_on_local_servers() {
        let with_dev = |server| {
            template_for(server).replace(
                "  providers: []",
                "  providers:\n    - {kind: dev, id: dev}",
            )
        };
        // A loopback listen address, or a container (published on 127.0.0.1 only).
        for server in [ServerPlacement::Host, ServerPlacement::Container] {
            assert!(parse(&with_dev(server), &secrets()).is_ok(), "{server:?}");
        }
        let open = with_dev(ServerPlacement::Host)
            .replace("listen: \"127.0.0.1:8080\"", "listen: \"0.0.0.0:8080\"");
        let public = with_dev(ServerPlacement::Container).replace(
            "# public_url: https://mem.example.com",
            "public_url: https://mem.example.com",
        );
        for raw in [open, public] {
            match parse(&raw, &secrets()) {
                Err(ConfigError::Invalid(problems)) => {
                    assert!(
                        problems.iter().any(|p| p.contains("dev provider")),
                        "{problems:?}"
                    );
                }
                other => panic!("expected Invalid, got {other:?}"),
            }
        }
    }

    #[test]
    fn telegram_sign_in_works_on_a_server_for_listed_addresses_only() {
        let public = template_for(ServerPlacement::Container)
            .replace(
                "# public_url: https://mem.example.com",
                "public_url: https://mem.example.com",
            )
            .replace(
                "  providers: []",
                "  providers:\n    - {kind: telegram, id: telegram}",
            );
        match parse(&public, &secrets()) {
            Err(ConfigError::Invalid(problems)) => assert!(
                problems
                    .iter()
                    .any(|p| p.contains("sign-in through Telegram needs")),
                "{problems:?}"
            ),
            other => panic!("expected Invalid, got {other:?}"),
        }
        let listed = public.replace("  allow: []", "  allow: [alice@example.com]");
        let config = parse(&listed, &secrets())
            .expect("a server with a listed address")
            .config;
        assert!(matches!(
            config.auth.providers.as_slice(),
            [Provider::Telegram { channel, .. }] if channel == "telegram"
        ));
    }

    #[test]
    fn a_public_address_may_have_a_path() {
        let with = |url: &str| {
            template_for(ServerPlacement::Container).replace(
                "# public_url: https://mem.example.com",
                &format!("public_url: {url}"),
            )
        };
        let loaded = parse(&with("https://mem.example.com/frood"), &secrets()).unwrap();
        assert_eq!(
            loaded.config.server.public_url.as_deref(),
            Some("https://mem.example.com/frood")
        );
        for bad in [
            "http://mem.example.com",
            "https://mem.example.com/frood?x=1",
        ] {
            match parse(&with(bad), &secrets()) {
                Err(ConfigError::Invalid(problems)) => {
                    assert!(
                        problems.iter().any(|p| p.contains("public_url")),
                        "{problems:?}"
                    );
                }
                other => panic!("{bad}: expected Invalid, got {other:?}"),
            }
        }
        // Caddy, and so the routes of other apps, exists only for a public address.
        let routes = template_for(ServerPlacement::Container)
            .replace("deploy:\n", "deploy:\n  proxy: { routes: pages.caddy }\n");
        match parse(&routes, &secrets()) {
            Err(ConfigError::Invalid(problems)) => {
                assert!(
                    problems.iter().any(|p| p.contains("deploy.proxy.routes")),
                    "{problems:?}"
                );
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn a_proxy_in_front_needs_an_address_and_takes_no_routes() {
        let public = |proxy: &str| {
            template_for(ServerPlacement::Container)
                .replace(
                    "  listen: \"0.0.0.0:8080\"\n",
                    "  listen: \"0.0.0.0:8080\"\n  public_url: https://1-2-3-4.sslip.io/frood\n",
                )
                .replace("deploy:\n", &format!("deploy:\n  proxy: {proxy}\n"))
        };
        let loaded = parse(
            &public("{ external: /srv/edge/paths/frood.caddy }"),
            &secrets(),
        )
        .unwrap();
        assert_eq!(
            loaded.config.deploy.proxy.unwrap().external.as_deref(),
            Some("/srv/edge/paths/frood.caddy")
        );
        let refused = |text: &str, says: &str| match parse(text, &secrets()) {
            Err(ConfigError::Invalid(problems)) => {
                assert!(problems.iter().any(|p| p.contains(says)), "{problems:?}");
            }
            other => panic!("expected Invalid, got {other:?}"),
        };
        refused(
            &public("{ external: frood.caddy, routes: pages.caddy }"),
            "do not go together",
        );
        refused(&public("{ external: \"\" }"), "must name a file");
        refused(
            &template_for(ServerPlacement::Container)
                .replace("deploy:\n", "deploy:\n  proxy: { external: frood.caddy }\n"),
            "deploy.proxy.external needs server.public_url",
        );
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

    #[test]
    fn files_go_to_a_folder_unless_the_config_says_otherwise() {
        let loaded = parse(&template_for(ServerPlacement::Host), &secrets()).unwrap();
        assert_eq!(loaded.config.storage.objects, Objects::default());
        assert_eq!(loaded.config.storage.objects.max_mb(), 100);
        let with = |objects: &str| {
            template_for(ServerPlacement::Host).replace(
                "storage:\n  uses: postgres\n",
                &format!("storage:\n  uses: postgres\n  objects: {objects}\n"),
            )
        };
        let s3 = parse(
            &with("{ uses: s3, endpoint: 'https://s3.example', bucket: frood, access_key_id: k, secret_access_key: s }"),
            &secrets(),
        )
        .unwrap();
        assert!(matches!(s3.config.storage.objects, Objects::S3 { .. }));
        let db = parse(&with("{ uses: database }"), &secrets()).unwrap();
        assert_eq!(db.config.storage.objects.max_mb(), 25);
        for bad in [
            "{ uses: s3, endpoint: 'http://s3.example', bucket: frood, access_key_id: k, secret_access_key: s }",
            "{ uses: disk, max_mb: 0 }",
            "{ uses: database, max_mb: 1000 }",
        ] {
            assert!(
                matches!(parse(&with(bad), &secrets()), Err(ConfigError::Invalid(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn spheres_default_to_three_and_can_change() {
        let loaded = parse(&template_for(ServerPlacement::Host), &secrets()).unwrap();
        let ids: Vec<&str> = loaded
            .config
            .spheres
            .iter()
            .map(|s| s.id.as_str())
            .collect();
        assert_eq!(ids, ["tech", "business", "personal"]);
        assert_eq!(loaded.config.spheres[2].name(), "Personal");
        assert!(loaded.config.spheres[0].about().is_some());

        let raw = format!(
            "{}\nspheres:\n  - id: personal\n  - id: family\n    name: Семья\n    about: home and relatives\n",
            template_for(ServerPlacement::Host)
        );
        let spheres = parse(&raw, &secrets()).unwrap().config.spheres;
        assert_eq!(spheres.len(), 2);
        assert_eq!(spheres[1].name(), "Семья");

        let off = format!("{}\nspheres: []\n", template_for(ServerPlacement::Host));
        assert!(parse(&off, &secrets()).unwrap().config.spheres.is_empty());

        for bad in [
            "spheres:\n  - id: family\n",
            "spheres:\n  - id: Work\n",
            "spheres:\n  - id: none\n    name: x\n    about: y\n",
            "spheres:\n  - id: tech\n  - id: tech\n",
        ] {
            let raw = format!("{}\n{bad}", template_for(ServerPlacement::Host));
            assert!(
                matches!(parse(&raw, &secrets()), Err(ConfigError::Invalid(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn backups_say_where_and_what_to_keep() {
        let key = "age1ql3z7hjy54pw3hyww5ayyfg7zqgvc7w3j2elw8zmrj2kg5sfn9aqmcac8p";
        let ok = format!(
            "{}\nbackup:\n  recovery_key: {key}\n  targets:\n    disk: {{ repository: /repos/disk, password: '${{secret:POSTGRES_PASSWORD}}' }}\n  keep: {{ daily: 7 }}\n",
            template_for(ServerPlacement::Host)
        );
        let loaded = parse(&ok, &secrets()).unwrap();
        let backup = loaded.config.backup.as_ref().unwrap();
        assert_eq!(backup.targets["disk"].password, "pw");
        assert_eq!(backup.keep.rules(), [("daily", 7)]);
        // One target, no schedules: said, not refused.
        assert_eq!(loaded.warnings.len(), 3, "{:?}", loaded.warnings);
        assert!(loaded.warnings[0].contains("one target"));

        let scheduled = format!(
            "{ok}jobs:\n  backup: {{ on: [{{ schedule: '0 */6 * * *', timezone: Europe/Amsterdam }}] }}\n  backup-verify: {{ on: [{{ schedule: '0 4 * * 0', timezone: Europe/Amsterdam }}] }}\n"
        );
        assert_eq!(parse(&scheduled, &secrets()).unwrap().warnings.len(), 1);

        let problems = |raw: &str| match parse(raw, &secrets()) {
            Err(ConfigError::Invalid(problems)) => problems.join("\n"),
            other => panic!("expected problems, got {other:?}"),
        };
        assert!(problems(&ok.replace("{ daily: 7 }", "{}")).contains("backup.keep says nothing"));
        assert!(problems(&ok.replace(key, "ssh-ed25519 AAAA")).contains("recovery_key"));
        assert!(
            problems(&format!(
                "{}\njobs:\n  backup: {{ on: [{{ schedule: '0 3 * * *', timezone: UTC }}] }}\n",
                template_for(ServerPlacement::Host)
            ))
            .contains("no `backup` section")
        );
        let volume = format!(
            "{}\nmodules:\n  backup: {{ image: b, volumes: [relative-only] }}\n",
            template_for(ServerPlacement::Host)
        );
        assert!(problems(&volume).contains("host-path:/path/in/module"));
    }
}
