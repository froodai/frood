//! Jobs: the built-in standard set and how `frood.yaml` changes it (docs/13 §3, §10).

pub(crate) mod embed;
pub(crate) mod extract;
pub(crate) mod transcribe;

use std::collections::BTreeMap;

use frood_config::{Config, Trigger};
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
}

#[derive(Debug, Clone)]
pub struct JobSpec {
    pub name: String,
    pub uses: String,
    pub task: Task,
    pub triggers: Vec<Trigger>,
    pub with: Map<String, Value>,
}

impl JobSpec {
    pub(crate) fn matches(&self, event_type: &str, data: &Value) -> bool {
        self.triggers.iter().any(|trigger| {
            trigger.event == event_type
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
        event: event.to_owned(),
        filter: filter
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect::<BTreeMap<_, _>>(),
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
        },
    ]
}

/// What the installation can run, decided at startup.
pub struct Capabilities<'a> {
    pub default_account: Option<&'a str>,
    pub embeddings: bool,
    pub modules: usize,
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
        }
        match job.task {
            Task::Extract => match job.with_str("account").or(caps.default_account) {
                Some(account) => {
                    let account = account.to_owned();
                    job.with.insert("account".into(), json!(account));
                }
                None => {
                    warnings.push(
                        "job extract is off: add an account (accounts: …) so it can call a model"
                            .to_owned(),
                    );
                    continue;
                }
            },
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
    for name in config.jobs.keys() {
        if !matches!(name.as_str(), "transcribe" | "extract" | "embed") {
            warnings.push(format!(
                "job {name}: jobs beyond the standard set arrive in M4; ignored for now"
            ));
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

    #[test]
    fn standard_jobs_follow_what_the_installation_has() {
        let caps = Capabilities {
            default_account: Some("claude"),
            embeddings: false,
            modules: 0,
        };
        let (jobs, warnings) = resolve(&config(""), &caps);
        assert_eq!(names(&jobs), ["extract"]);
        assert_eq!(jobs[0].with_str("account"), Some("claude"));
        assert!(warnings.iter().any(|w| w.contains("transcribe is off")));

        let caps = Capabilities {
            default_account: None,
            embeddings: true,
            modules: 1,
        };
        let (jobs, warnings) = resolve(&config(""), &caps);
        assert_eq!(names(&jobs), ["transcribe", "embed"]);
        assert!(warnings.iter().any(|w| w.contains("extract is off")));
    }

    #[test]
    fn config_changes_and_turns_off_standard_jobs() {
        let caps = Capabilities {
            default_account: Some("claude"),
            embeddings: true,
            modules: 1,
        };
        let (jobs, warnings) = resolve(
            &config(
                "jobs:\n  extract: { with: { model: claude-sonnet-5-5 } }\n  embed: { enabled: false }\n  digest: { uses: llm.complete }\n",
            ),
            &caps,
        );
        assert_eq!(names(&jobs), ["transcribe", "extract"]);
        assert_eq!(jobs[1].with_str("model"), Some("claude-sonnet-5-5"));
        assert_eq!(jobs[1].with_str("account"), Some("claude"));
        assert!(warnings.iter().any(|w| w.contains("digest")));
    }

    #[test]
    fn triggers_match_event_type_and_fields() {
        let caps = Capabilities {
            default_account: Some("claude"),
            embeddings: true,
            modules: 1,
        };
        let (jobs, _) = resolve(&config(""), &caps);
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
