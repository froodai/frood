//! Jobs: the built-in standard set and how `frood.yaml` changes it (docs/13 §3, §10).

pub(crate) mod embed;
pub(crate) mod extract;
pub(crate) mod transcribe;

use std::collections::BTreeMap;
use std::path::Path;

use frood_config::{AskUser, Budget, Config, Job, OnExpire, Trigger};
use serde_json::{Map, Value, json};

/// What the platform does around the capability call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Task {
    /// Media → text of the source (`media.transcribe`).
    Transcribe,
    /// Text → observations, entities, proposals (`llm.complete`).
    Extract,
    /// Embeddings of new knowledge for search (`llm.embed`).
    Embed,
    /// An agent in a runtime module (`agents.execute`).
    Agent,
}

/// What an agent job needs to run (docs/13 §3); all of it named in the config (A3).
#[derive(Debug, Clone)]
pub struct AgentSettings {
    pub runtime: String,
    pub agent: String,
    pub model: String,
    pub account: String,
    pub instructions: String,
    pub tools: Vec<String>,
    pub permissions: Vec<String>,
    pub on_behalf_of: Option<String>,
    pub ask_user: AskUser,
    pub budget: Budget,
}

/// Instructions of the standard `daily-sweep`, used unless the config gives its own.
const DAILY_SWEEP: &str = include_str!("../../prompts/daily-sweep.md");

#[derive(Debug, Clone)]
pub struct JobSpec {
    pub name: String,
    pub uses: String,
    pub task: Task,
    pub triggers: Vec<Trigger>,
    pub with: Map<String, Value>,
    /// Agent jobs only.
    pub agent: Option<AgentSettings>,
}

impl JobSpec {
    pub(crate) fn matches(&self, event_type: &str, data: &Value) -> bool {
        if event_type == crate::schedules::TICK {
            return data.get("job").and_then(Value::as_str) == Some(self.name.as_str());
        }
        self.triggers.iter().any(|trigger| {
            trigger.event.as_deref() == Some(event_type)
                && trigger
                    .filter
                    .iter()
                    .all(|(key, want)| data.get(key) == Some(want))
        })
    }

    pub(crate) fn with_str(&self, key: &str) -> Option<&str> {
        self.with.get(key).and_then(Value::as_str)
    }
}

fn trigger(event: &str, filter: &[(&str, Value)]) -> Trigger {
    Trigger {
        filter: filter
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect::<BTreeMap<_, _>>(),
        ..Trigger::on_event(event)
    }
}

/// The standard set (bundle `frood/standard`, built in until the registry exists).
fn standard() -> Vec<JobSpec> {
    vec![
        JobSpec {
            name: "transcribe".into(),
            uses: "media.transcribe".into(),
            task: Task::Transcribe,
            triggers: vec![trigger("source.captured", &[("has_media", json!(true))])],
            with: Map::new(),
            agent: None,
        },
        JobSpec {
            name: "extract".into(),
            uses: "llm.complete".into(),
            task: Task::Extract,
            triggers: vec![
                trigger("source.captured", &[("has_text", json!(true))]),
                trigger("source.transcribed", &[]),
            ],
            with: Map::new(),
            agent: None,
        },
        JobSpec {
            name: "embed".into(),
            uses: "llm.embed".into(),
            task: Task::Embed,
            triggers: vec![
                trigger("assertion.created", &[]),
                trigger("assertion.superseded", &[]),
            ],
            with: Map::new(),
            agent: None,
        },
        JobSpec {
            name: "daily-sweep".into(),
            uses: "agents.execute".into(),
            task: Task::Agent,
            triggers: vec![],
            with: Map::new(),
            agent: None,
        },
    ]
}

/// What the installation can run, decided at startup.
pub struct Capabilities<'a> {
    /// Accounts the core can call models with directly (API keys, not subscriptions).
    pub api_accounts: &'a [String],
    pub embeddings: bool,
    pub modules: usize,
    /// Directory of the config file, for instructions given as paths.
    pub base_dir: &'a Path,
}

/// Instructions as written in the config: inline text, or a path to a file.
fn read_instructions(value: &str, base_dir: &Path) -> Result<String, String> {
    let looks_like_path = !value.contains('\n')
        && (value.starts_with("./") || value.starts_with('/') || value.ends_with(".md"));
    if !looks_like_path {
        return Ok(value.to_owned());
    }
    let path = base_dir.join(value);
    std::fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))
}

/// The agent part of a job from its config (A3: runtime, agent, model and account are named;
/// so is the most a run may spend).
fn agent_settings(
    name: &str,
    job: &Job,
    builtin_instructions: Option<&str>,
    base_dir: &Path,
) -> Result<AgentSettings, String> {
    let missing: Vec<&str> = [
        ("runtime", job.runtime.is_none()),
        ("agent", job.agent.is_none()),
        ("model", job.model.is_none()),
        ("account", job.account.is_none()),
        (
            "budget.max_usd",
            job.budget.as_ref().is_none_or(|b| b.max_usd.is_none()),
        ),
    ]
    .iter()
    .filter(|(_, m)| *m)
    .map(|(f, _)| *f)
    .collect();
    if !missing.is_empty() {
        return Err(format!(
            "job {name} is off: name its {} (there are no defaults)",
            missing.join(", ")
        ));
    }
    let instructions = match (&job.instructions, builtin_instructions) {
        (Some(text), _) => {
            read_instructions(text, base_dir).map_err(|e| format!("job {name}: {e}"))?
        }
        (None, Some(builtin)) => builtin.to_owned(),
        (None, None) => return Err(format!("job {name} is off: it has no instructions")),
    };
    Ok(AgentSettings {
        runtime: job.runtime.clone().unwrap_or_default(),
        agent: job.agent.clone().unwrap_or_default(),
        model: job.model.clone().unwrap_or_default(),
        account: job.account.clone().unwrap_or_default(),
        instructions,
        tools: job.tools.clone().unwrap_or_else(|| vec!["frood".into()]),
        permissions: job.permissions.clone().unwrap_or_else(|| {
            vec![
                "knowledge:read".into(),
                "knowledge:capture".into(),
                "knowledge:propose".into(),
            ]
        }),
        on_behalf_of: job.on_behalf_of.clone(),
        ask_user: job.ask_user.clone().unwrap_or(AskUser {
            allowed: true,
            expire_after: Some("3d".into()),
            on_expire: OnExpire::Skip,
            default_answer: None,
        }),
        budget: job.budget.clone().unwrap_or_default(),
    })
}

/// The jobs this installation runs, with a warning for each job that cannot run.
pub fn resolve(config: &Config, caps: &Capabilities<'_>) -> (Vec<JobSpec>, Vec<String>) {
    let mut jobs = Vec::new();
    let mut warnings = Vec::new();
    for mut job in standard() {
        if let Some(change) = config.jobs.get(&job.name) {
            if change.enabled == Some(false) {
                continue;
            }
            if let Some(uses) = &change.uses
                && uses != &job.uses
            {
                warnings.push(format!(
                    "job {}: only `{}` is available in this version, not `{uses}`; the job is off",
                    job.name, job.uses
                ));
                continue;
            }
            if let Some(triggers) = &change.on {
                job.triggers = triggers.clone();
            }
            for (key, value) in &change.with {
                job.with.insert(key.clone(), value.clone());
            }
            for (key, value) in [("account", &change.account), ("model", &change.model)] {
                if let Some(value) = value {
                    job.with.insert(key.into(), json!(value));
                }
            }
        }
        match job.task {
            Task::Extract => {
                // No default account or model (D4): the installation names them.
                match job.with_str("account") {
                    None => {
                        warnings.push(
                            "job extract is off: name its account in jobs.extract.with.account"
                                .to_owned(),
                        );
                        continue;
                    }
                    Some(account) if !caps.api_accounts.iter().any(|a| a == account) => {
                        warnings.push(format!(
                            "job extract is off: account {account} is not an api_key account"
                        ));
                        continue;
                    }
                    Some(_) => {}
                }
                if job.with_str("model").is_none() {
                    warnings.push(
                        "job extract is off: name its model in jobs.extract.with.model".to_owned(),
                    );
                    continue;
                }
            }
            Task::Agent => {
                let Some(change) = config.jobs.get(&job.name) else {
                    continue;
                };
                match agent_settings(&job.name, change, Some(DAILY_SWEEP), caps.base_dir) {
                    Ok(settings) => job.agent = Some(settings),
                    Err(warning) => {
                        warnings.push(warning);
                        continue;
                    }
                }
            }
            Task::Embed if !caps.embeddings => continue,
            Task::Transcribe if caps.modules == 0 => {
                warnings.push(
                    "job transcribe is off: no module gives media.transcribe; voice messages are kept but not transcribed"
                        .to_owned(),
                );
                continue;
            }
            _ => {}
        }
        jobs.push(job);
    }
    for (name, job) in &config.jobs {
        if matches!(
            name.as_str(),
            "transcribe" | "extract" | "embed" | "daily-sweep"
        ) || job.enabled == Some(false)
        {
            continue;
        }
        match job.uses.as_deref() {
            Some("agents.execute") => match agent_settings(name, job, None, caps.base_dir) {
                Ok(settings) => jobs.push(JobSpec {
                    name: name.clone(),
                    uses: "agents.execute".into(),
                    task: Task::Agent,
                    triggers: job.on.clone().unwrap_or_default(),
                    with: job.with.clone(),
                    agent: Some(settings),
                }),
                Err(warning) => warnings.push(warning),
            },
            other => warnings.push(format!(
                "job {name}: only agent jobs (uses: agents.execute) can be added for now, not {}",
                other.unwrap_or("a job without uses")
            )),
        }
    }
    (jobs, warnings)
}

/// A failed attempt; transient ones are retried.
#[derive(Debug)]
pub struct JobError {
    pub message: String,
    pub transient: bool,
}

impl JobError {
    pub fn transient(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            transient: true,
        }
    }

    pub fn permanent(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            transient: false,
        }
    }
}

impl From<frood_ledger::LedgerError> for JobError {
    fn from(err: frood_ledger::LedgerError) -> Self {
        match err {
            frood_ledger::LedgerError::Database(db) => Self::transient(format!("database: {db}")),
            other => Self::permanent(other.to_string()),
        }
    }
}

impl From<frood_llm::LlmError> for JobError {
    fn from(err: frood_llm::LlmError) -> Self {
        Self {
            transient: err.is_transient(),
            message: err.to_string(),
        }
    }
}

impl From<frood_modhost::ModuleError> for JobError {
    fn from(err: frood_modhost::ModuleError) -> Self {
        Self {
            transient: err.is_transient(),
            message: err.to_string(),
        }
    }
}

impl From<frood_search::SearchError> for JobError {
    fn from(err: frood_search::SearchError) -> Self {
        match err {
            frood_search::SearchError::Ledger(e) => e.into(),
            frood_search::SearchError::Llm(e) => e.into(),
        }
    }
}

impl From<sqlx::Error> for JobError {
    fn from(err: sqlx::Error) -> Self {
        Self::transient(format!("database: {err}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(jobs: &str) -> Config {
        let raw = format!(
            "version: 1\ninstallation: {{name: t}}\nstorage: {{uses: postgres, url: 'postgres://x'}}\n\
             deploy: {{server: host}}\naccounts:\n  claude: {{ kind: api_key, provider: anthropic, secret: k }}\n{jobs}"
        );
        frood_config::parse(
            &raw,
            &frood_config::Secrets::from_pairs(Vec::<(String, String)>::new()),
        )
        .unwrap()
        .config
    }

    fn names(jobs: &[JobSpec]) -> Vec<&str> {
        jobs.iter().map(|j| j.name.as_str()).collect()
    }

    fn caps<'a>(accounts: &'a [String], embeddings: bool, modules: usize) -> Capabilities<'a> {
        Capabilities {
            api_accounts: accounts,
            embeddings,
            modules,
            base_dir: Path::new("."),
        }
    }

    #[test]
    fn standard_jobs_follow_what_the_installation_has() {
        let claude = ["claude".to_owned()];
        let caps = caps(&claude, false, 0);
        let (jobs, warnings) = resolve(&config(""), &caps);
        assert!(
            jobs.is_empty(),
            "no account or model named, so no extraction"
        );
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("jobs.extract.with.account"))
        );
        let (_, warnings) = resolve(
            &config("jobs:\n  extract: { with: { account: claude } }\n"),
            &caps,
        );
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("jobs.extract.with.model"))
        );
        let (jobs, warnings) = resolve(
            &config("jobs:\n  extract: { with: { account: claude, model: claude-haiku-5-5 } }\n"),
            &caps,
        );
        assert_eq!(names(&jobs), ["extract"]);
        assert!(warnings.iter().any(|w| w.contains("transcribe is off")));

        let (jobs, warnings) = resolve(&config(""), &self::caps(&[], true, 1));
        assert_eq!(names(&jobs), ["transcribe", "embed"]);
        assert!(warnings.iter().any(|w| w.contains("extract is off")));
    }

    #[test]
    fn config_changes_and_turns_off_standard_jobs() {
        let claude = ["claude".to_owned()];
        let (jobs, warnings) = resolve(
            &config(
                "jobs:\n  extract: { account: claude, model: claude-sonnet-5-5 }\n  embed: { enabled: false }\n  digest: { uses: llm.complete }\n",
            ),
            &caps(&claude, true, 1),
        );
        assert_eq!(names(&jobs), ["transcribe", "extract"]);
        assert_eq!(jobs[1].with_str("model"), Some("claude-sonnet-5-5"));
        assert_eq!(jobs[1].with_str("account"), Some("claude"));
        assert!(warnings.iter().any(|w| w.contains("digest")));
    }

    #[test]
    fn triggers_match_event_type_and_fields() {
        let claude = ["claude".to_owned()];
        let (jobs, _) = resolve(
            &config("jobs:\n  extract: { with: { account: claude, model: m } }\n"),
            &caps(&claude, true, 1),
        );
        let extract = jobs.iter().find(|j| j.name == "extract").unwrap();
        assert!(extract.matches("source.captured", &json!({ "has_text": true })));
        assert!(!extract.matches(
            "source.captured",
            &json!({ "has_text": false, "has_media": true })
        ));
        assert!(extract.matches("source.transcribed", &json!({})));
        let transcribe = jobs.iter().find(|j| j.name == "transcribe").unwrap();
        assert!(transcribe.matches("source.captured", &json!({ "has_media": true })));
    }
}
