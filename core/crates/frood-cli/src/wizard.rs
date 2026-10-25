//! The questions of `frood init` in a terminal (docs/27 §2): where Frood runs, who signs in, what
//! reads notes, Telegram, voice messages, backups, the morning digest. Every question has an
//! answer by default (Enter takes it), and checking an answer spends nothing: a model key by
//! listing models, a bot by `getMe`, a backup folder by writing a file into it.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use serde_json::Value;

const ANTHROPIC_URL: &str = "https://api.anthropic.com";
const OPENAI_URL: &str = "https://api.openai.com";
const TELEGRAM_URL: &str = "https://api.telegram.org";
/// When the digest runs unless asked otherwise: the server's own default (docs/27 §3).
pub const DIGEST_AT: &str = "08:30";
/// Languages the Telegram bot speaks.
const BOT_LANGUAGES: [&str; 8] = ["en", "ru", "es", "fr", "pt", "zh", "hi", "ar"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Anthropic,
    Openai,
}

impl Provider {
    fn name(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::Openai => "openai",
        }
    }

    fn secret(self) -> &'static str {
        match self {
            Self::Anthropic => "ANTHROPIC_API_KEY",
            Self::Openai => "OPENAI_API_KEY",
        }
    }
}

/// A key that reads notes, and the model it reads them with.
#[derive(Debug, Clone)]
pub struct Model {
    pub provider: Provider,
    pub key: String,
    pub model: String,
}

/// What `frood init` writes. Without a terminal, or with `--yes`, the answers by default.
#[derive(Debug, Clone, Default)]
pub struct Answers {
    /// A server with this public HTTPS address; none: this computer (a laptop, a home server).
    pub public_url: Option<String>,
    /// Who signs in; on this computer, without a password.
    pub email: Option<String>,
    pub timezone: Option<String>,
    pub model: Option<Model>,
    /// The token from @BotFather, and the bot's name.
    pub telegram: Option<(String, String)>,
    /// Voice messages transcribed with the OpenAI key.
    pub voice: bool,
    /// A folder for backups (an external disk, say).
    pub backups: Option<PathBuf>,
    /// Weekdays at this time, when not 08:30.
    pub digest_at: Option<String>,
    /// The language the bot speaks.
    pub language: String,
}

impl Answers {
    pub fn defaults(
        email: Option<String>,
        public_url: Option<String>,
        timezone: Option<String>,
    ) -> Self {
        Self {
            public_url,
            email,
            timezone: timezone.or_else(system_timezone),
            language: system_language(),
            ..Self::default()
        }
    }
}

/// Whether to ask: a person at a terminal who did not pass `--yes`.
pub fn interactive(yes: bool) -> bool {
    !yes && std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

/// The zone this computer is set to: `TZ`, or what `/etc/localtime` points at.
pub fn system_timezone() -> Option<String> {
    if let Ok(zone) = std::env::var("TZ")
        && zone.contains('/')
    {
        return Some(zone.trim_start_matches(':').to_owned());
    }
    let target = std::fs::read_link("/etc/localtime").ok()?;
    let target = target.to_string_lossy();
    target
        .split_once("zoneinfo/")
        .map(|(_, zone)| zone.to_owned())
}

fn system_language() -> String {
    let lang = std::env::var("LANG").unwrap_or_default();
    let code = lang.get(..2).unwrap_or("en").to_lowercase();
    if BOT_LANGUAGES.contains(&code.as_str()) {
        code
    } else {
        "en".to_owned()
    }
}

/// The address `git` knows for this person, as a default.
pub fn git_email() -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["config", "--get", "user.email"])
        .output()
        .ok()?;
    let email = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    // GitHub's hidden address is no one's inbox.
    (email.contains('@') && !email.contains("noreply")).then_some(email)
}

/// Asks the questions; Enter takes each default.
pub async fn ask(mut answers: Answers) -> anyhow::Result<Answers> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()?;
    println!("A few questions; Enter takes the answer in brackets.\n");

    let place = choose(
        "Where does Frood run?",
        &[
            "this computer (a laptop, a home server)",
            "a server with its own address (https)",
        ],
        usize::from(answers.public_url.is_some()),
    )?;
    if place == 1 {
        loop {
            let url = input("Its public address", answers.public_url.as_deref())?;
            if url.starts_with("https://") {
                answers.public_url = Some(url.trim_end_matches('/').to_owned());
                break;
            }
            println!("  an https:// address, e.g. https://mem.example.com");
        }
    } else {
        answers.public_url = None;
    }

    loop {
        let email = input("Your email (who signs in)", answers.email.as_deref())?;
        if email.contains('@') {
            answers.email = Some(email.to_lowercase());
            break;
        }
        println!("  an email address");
    }
    let zone = input(
        "Your time zone",
        answers.timezone.as_deref().or(Some("UTC")),
    )?;
    answers.timezone = Some(zone).filter(|z| z != "UTC");

    let in_env = [Provider::Anthropic, Provider::Openai]
        .into_iter()
        .find(|p| std::env::var(p.secret()).is_ok_and(|k| !k.is_empty()));
    let which = choose(
        "Reading notes into knowledge takes a model. Which key?",
        &["an Anthropic API key", "an OpenAI API key", "later"],
        match in_env {
            Some(Provider::Anthropic) => 0,
            Some(Provider::Openai) => 1,
            None => 2,
        },
    )?;
    if which < 2 {
        let provider = if which == 0 {
            Provider::Anthropic
        } else {
            Provider::Openai
        };
        let found = std::env::var(provider.secret())
            .ok()
            .filter(|k| !k.is_empty());
        let key = secret(
            &format!("{} key", provider.name()),
            found.as_deref().map(|_| "the one in your environment"),
        )?
        .or(found);
        if let Some(key) = key {
            match cheapest_model(&http, provider, &key, None).await {
                Ok(model) => {
                    println!("  the key works; notes are read with {model}");
                    answers.model = Some(Model {
                        provider,
                        key,
                        model,
                    });
                }
                Err(err) => {
                    println!("  the key does not work: {err}");
                    if confirm("Keep it anyway?", false)? {
                        let model = input("Model to read notes with", None)?;
                        answers.model = Some(Model {
                            provider,
                            key,
                            model,
                        });
                    }
                }
            }
        }
    }

    if let Some(token) = secret("Telegram: the bot token from @BotFather", Some("skip"))? {
        match bot_name(&http, &token, None).await {
            Ok(name) => {
                println!("  the bot is @{name}");
                answers.telegram = Some((token, name));
            }
            Err(err) => println!("  Telegram does not take this token ({err}); skipped"),
        }
    }

    if answers
        .model
        .as_ref()
        .is_some_and(|m| m.provider == Provider::Openai)
    {
        answers.voice = confirm("Transcribe voice messages with the same OpenAI key?", true)?;
    }

    let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_default();
    let suggested = home.join("frood-backups");
    let folder = input(
        "Backups: a folder for them, an external disk is best ('-' skips)",
        Some(&suggested.to_string_lossy()),
    )?;
    if folder != "-" {
        let folder = PathBuf::from(expand_home(&folder, &home));
        match writable(&folder) {
            Ok(()) => answers.backups = Some(folder),
            Err(err) => println!("  cannot write there ({err}); backups skipped"),
        }
    }

    loop {
        let at = input("Morning digest on weekdays at", Some(DIGEST_AT))?;
        if chrono::NaiveTime::parse_from_str(&at, "%H:%M").is_ok() {
            answers.digest_at = (at != DIGEST_AT).then_some(at);
            break;
        }
        println!("  a time like 08:30");
    }
    println!();
    Ok(answers)
}

fn expand_home(path: &str, home: &Path) -> String {
    match path.strip_prefix("~/") {
        Some(rest) => home.join(rest).to_string_lossy().into_owned(),
        None => path.to_owned(),
    }
}

/// A folder that exists and takes files.
fn writable(folder: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(folder)?;
    let probe = folder.join(".frood-write-check");
    std::fs::write(&probe, b"ok")?;
    std::fs::remove_file(&probe)?;
    Ok(())
}

fn prompt(question: &str, default: Option<&str>) -> anyhow::Result<String> {
    match default {
        Some(d) if !d.is_empty() => print!("{question} [{d}]: "),
        _ => print!("{question}: "),
    }
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim().to_owned())
}

fn input(question: &str, default: Option<&str>) -> anyhow::Result<String> {
    let answer = prompt(question, default)?;
    Ok(if answer.is_empty() {
        default.unwrap_or_default().to_owned()
    } else {
        answer
    })
}

fn confirm(question: &str, default: bool) -> anyhow::Result<bool> {
    let answer = prompt(question, Some(if default { "Y/n" } else { "y/N" }))?;
    Ok(match answer.to_lowercase().as_str() {
        "" => default,
        a => a.starts_with('y'),
    })
}

fn choose(question: &str, options: &[&str], default: usize) -> anyhow::Result<usize> {
    println!("{question}");
    for (i, option) in options.iter().enumerate() {
        println!("  {}. {option}", i + 1);
    }
    loop {
        let answer = prompt("Choose", Some(&(default + 1).to_string()))?;
        if answer.is_empty() {
            return Ok(default);
        }
        if let Ok(n) = answer.parse::<usize>()
            && (1..=options.len()).contains(&n)
        {
            return Ok(n - 1);
        }
    }
}

/// A secret typed without echo; Enter keeps the default (none: nothing).
fn secret(question: &str, default: Option<&str>) -> anyhow::Result<Option<String>> {
    let echo = |on: bool| {
        if cfg!(unix) {
            let _ = std::process::Command::new("stty")
                .arg(if on { "echo" } else { "-echo" })
                .stdin(std::process::Stdio::inherit())
                .status();
        }
    };
    echo(false);
    let answer = prompt(question, default);
    echo(true);
    println!();
    let answer = answer?;
    Ok((!answer.is_empty()).then_some(answer))
}

/// The cheapest model fit for reading notes, from the provider's list — which costs nothing and
/// shows that the key works. Anthropic: the newest Haiku; OpenAI: the newest `-mini`.
pub async fn cheapest_model(
    http: &reqwest::Client,
    provider: Provider,
    key: &str,
    base: Option<&str>,
) -> anyhow::Result<String> {
    let request = match provider {
        Provider::Anthropic => http
            .get(format!(
                "{}/v1/models?limit=100",
                base.unwrap_or(ANTHROPIC_URL)
            ))
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01"),
        Provider::Openai => http
            .get(format!("{}/v1/models", base.unwrap_or(OPENAI_URL)))
            .bearer_auth(key),
    };
    let response = request
        .send()
        .await
        .context("the provider is not reachable")?;
    let status = response.status();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        let message = body["error"]["message"].as_str().unwrap_or("refused");
        anyhow::bail!("{status}: {message}");
    }
    // Newest first, by when each model was made.
    let mut models: Vec<(String, i64)> = body["data"]
        .as_array()
        .map(|all| {
            all.iter()
                .filter_map(|m| {
                    let id = m["id"].as_str()?.to_owned();
                    let made = m["created"].as_i64().unwrap_or_else(|| {
                        m["created_at"]
                            .as_str()
                            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                            .map_or(0, |t| t.timestamp())
                    });
                    Some((id, made))
                })
                .collect()
        })
        .unwrap_or_default();
    models.sort_by_key(|m| std::cmp::Reverse(m.1));
    let fits = |id: &str| match provider {
        Provider::Anthropic => id.contains("haiku"),
        Provider::Openai => id.starts_with("gpt-") && id.ends_with("-mini"),
    };
    models
        .into_iter()
        .map(|(id, _)| id)
        .find(|id| fits(id))
        .context("no small model in the provider's list; name one by hand")
}

/// The bot's name, which also shows that Telegram takes the token.
pub async fn bot_name(
    http: &reqwest::Client,
    token: &str,
    base: Option<&str>,
) -> anyhow::Result<String> {
    let body: Value = http
        .get(format!("{}/bot{token}/getMe", base.unwrap_or(TELEGRAM_URL)))
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("Telegram is not reachable"))?
        .json()
        .await
        .unwrap_or(Value::Null);
    if body["ok"] != true {
        anyhow::bail!(
            "{}",
            body["description"].as_str().unwrap_or("not a bot token")
        );
    }
    body["result"]["username"]
        .as_str()
        .map(str::to_owned)
        .context("Telegram did not say the bot's name")
}

/// The starter config with the answers written in: plain lines, no templates (docs/27 §3).
pub fn config_text(
    template: &str,
    name: &str,
    answers: &Answers,
    recovery_key: Option<&str>,
) -> String {
    let mut text = template.to_owned();
    if let Some(zone) = &answers.timezone {
        text = text.replacen(
            &format!("  name: {name}\n"),
            &format!("  name: {name}\n  timezone: {zone}\n"),
            1,
        );
    }
    if let Some(url) = &answers.public_url {
        text = text.replacen(
            "  # public_url: https://mem.example.com   # needed for cloud clients: Claude, ChatGPT",
            &format!("  public_url: {url}"),
            1,
        );
    }
    if let Some(email) = &answers.email {
        text = text.replacen("  allow: []", &format!("  allow: [{email}]"), 1);
        if answers.public_url.is_none() {
            text = text.replacen(
                "  providers: []",
                "  providers:\n    - { kind: local, id: local, label: On this computer }",
                1,
            );
        }
    }
    let version = env!("CARGO_PKG_VERSION");
    let mut out = vec![String::new(), "# Written by frood init.".to_owned()];
    if let Some(model) = &answers.model {
        let account = model.provider.name();
        out.push("accounts:".to_owned());
        out.push(format!(
            "  {account}: {{ kind: api_key, provider: {account}, secret: '${{secret:{}}}' }}",
            model.provider.secret()
        ));
        out.push(String::new());
    }
    let digest = answers.digest_at.as_deref().and_then(|at| {
        let (hour, minute) = at.split_once(':')?;
        Some(format!(
            "  digest: {{ on: [{{ schedule: '{} {} * * 1-5', timezone: {} }}] }}",
            minute.trim_start_matches('0').parse::<u32>().unwrap_or(0),
            hour.trim_start_matches('0').parse::<u32>().unwrap_or(0),
            answers.timezone.as_deref().unwrap_or("UTC")
        ))
    });
    if answers.model.is_some() || digest.is_some() {
        out.push("jobs:".to_owned());
        if let Some(model) = &answers.model {
            let effort = if model.provider == Provider::Anthropic {
                ", effort: low"
            } else {
                ""
            };
            out.push(format!(
                "  extract: {{ with: {{ account: {}, model: {}{effort} }} }}",
                model.provider.name(),
                model.model
            ));
        }
        out.extend(digest);
        out.push(String::new());
    }
    let mut modules = Vec::new();
    if answers.telegram.is_some() {
        modules.push(format!(
            "  telegram: {{ image: ghcr.io/froodai/telegram:{version}, secrets: [TELEGRAM_BOT_TOKEN], config: {{ language: {} }} }}",
            answers.language
        ));
    }
    if answers.voice {
        modules.push(format!(
            "  stt: {{ image: ghcr.io/froodai/stt-openai:{version}, secrets: [OPENAI_API_KEY], config: {{ model: gpt-4o-mini-transcribe }} }}"
        ));
    }
    if let Some(folder) = &answers.backups {
        modules.push(format!(
            "  backup: {{ image: ghcr.io/froodai/backup-restic:{version}, volumes: [\"{}:/repos/disk\"] }}",
            folder.display()
        ));
    }
    if !modules.is_empty() {
        out.push("modules:".to_owned());
        out.extend(modules);
        out.push(String::new());
    }
    if let (Some(_), Some(key)) = (&answers.backups, recovery_key) {
        out.push("backup:".to_owned());
        out.push(format!("  recovery_key: {key}"));
        out.push("  targets:".to_owned());
        out.push(
            "    disk: { repository: /repos/disk, password: '${secret:BACKUP_DISK_PASSWORD}' }"
                .to_owned(),
        );
        out.push("  keep: { daily: 30, monthly: 12 }".to_owned());
        out.push(String::new());
    }
    if out.len() > 2 {
        text.push_str(&out.join("\n"));
    }
    text
}

/// Secrets the answers bring, as `KEY=value` pairs.
pub fn secrets(answers: &Answers) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Some(model) = &answers.model {
        out.push((model.provider.secret().to_owned(), model.key.clone()));
    }
    if let Some((token, _)) = &answers.telegram {
        out.push(("TELEGRAM_BOT_TOKEN".to_owned(), token.clone()));
    }
    if answers.backups.is_some() {
        out.push((
            "BACKUP_DISK_PASSWORD".to_owned(),
            format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            ),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use axum::routing::get;
    use axum::{Json, Router};
    use serde_json::json;

    use super::*;

    async fn fake() -> String {
        let app = Router::new()
            .route(
                "/v1/models",
                get(|headers: axum::http::HeaderMap| async move {
                    if headers.get("x-api-key").is_some() {
                        Json(json!({ "data": [
                            { "id": "claude-opus-5-5", "created_at": "2026-08-01T00:00:00Z" },
                            { "id": "claude-haiku-4-5", "created_at": "2025-10-01T00:00:00Z" },
                            { "id": "claude-haiku-5-5", "created_at": "2026-08-01T00:00:00Z" }
                        ] }))
                    } else {
                        Json(json!({ "data": [
                            { "id": "gpt-6", "created": 300 },
                            { "id": "gpt-6-mini", "created": 300 },
                            { "id": "gpt-5-mini", "created": 200 }
                        ] }))
                    }
                }),
            )
            .route(
                "/botGOOD/getMe",
                get(|| async {
                    Json(json!({ "ok": true, "result": { "username": "my_frood_bot" } }))
                }),
            )
            .route(
                "/botBAD/getMe",
                get(|| async { Json(json!({ "ok": false, "description": "Unauthorized" })) }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        url
    }

    #[tokio::test]
    async fn keys_and_bots_are_checked_without_spending() {
        let base = fake().await;
        let http = reqwest::Client::new();
        assert_eq!(
            cheapest_model(&http, Provider::Anthropic, "k", Some(&base))
                .await
                .unwrap(),
            "claude-haiku-5-5"
        );
        assert_eq!(
            cheapest_model(&http, Provider::Openai, "k", Some(&base))
                .await
                .unwrap(),
            "gpt-6-mini"
        );
        assert_eq!(
            bot_name(&http, "GOOD", Some(&base)).await.unwrap(),
            "my_frood_bot"
        );
        assert!(bot_name(&http, "BAD", Some(&base)).await.is_err());
    }

    fn template() -> String {
        frood_config::template(&frood_config::TemplateOptions {
            name: "personal",
            server: frood_config::ServerPlacement::Container,
            build: None,
            image: "ghcr.io/froodai/frood:0.0.1",
            postgres_port: 5432,
        })
    }

    fn load(text: &str, secrets: &[(String, String)]) -> frood_config::Config {
        let mut pairs: Vec<(String, String)> = vec![("POSTGRES_PASSWORD".into(), "pw".into())];
        pairs.extend(secrets.iter().cloned());
        frood_config::parse(text, &frood_config::Secrets::from_pairs(pairs))
            .unwrap_or_else(|e| panic!("{e}\n{text}"))
            .config
    }

    #[test]
    fn a_laptop_with_everything() {
        let answers = Answers {
            email: Some("alice@example.com".into()),
            timezone: Some("Europe/Belgrade".into()),
            model: Some(Model {
                provider: Provider::Openai,
                key: "sk-test".into(),
                model: "gpt-6-mini".into(),
            }),
            telegram: Some(("123:abc".into(), "my_frood_bot".into())),
            voice: true,
            backups: Some(PathBuf::from("/Volumes/Backup/frood")),
            digest_at: Some("09:05".into()),
            language: "ru".into(),
            ..Answers::default()
        };
        let key = age::x25519::Identity::generate().to_public().to_string();
        let text = config_text(&template(), "personal", &answers, Some(&key));
        let config = load(&text, &secrets(&answers));
        assert_eq!(
            config.installation.timezone.as_deref(),
            Some("Europe/Belgrade")
        );
        assert_eq!(config.auth.allow, ["alice@example.com"]);
        assert!(matches!(
            config.auth.providers.as_slice(),
            [frood_config::Provider::Local { .. }]
        ));
        assert_eq!(config.accounts.len(), 1);
        let extract = &config.jobs["extract"];
        assert_eq!(extract.with["model"], "gpt-6-mini");
        assert_eq!(
            config.jobs["digest"].on.as_ref().unwrap()[0]
                .schedule
                .as_deref(),
            Some("5 9 * * 1-5")
        );
        let modules: Vec<&str> = config.modules.keys().map(String::as_str).collect();
        assert_eq!(modules, ["backup", "stt", "telegram"]);
        assert_eq!(config.backup.unwrap().recovery_key, key);
    }

    #[test]
    fn the_defaults_write_nothing_extra_and_a_server_keeps_its_sign_in() {
        let bare = template();
        assert_eq!(
            config_text(&bare, "personal", &Answers::default(), None),
            bare
        );
        let server = Answers {
            public_url: Some("https://mem.example.com".into()),
            email: Some("alice@example.com".into()),
            ..Answers::default()
        };
        let config = load(&config_text(&bare, "personal", &server, None), &[]);
        assert_eq!(
            config.server.public_url.as_deref(),
            Some("https://mem.example.com")
        );
        assert!(
            config.auth.providers.is_empty(),
            "a server signs in through OIDC"
        );
    }
}
