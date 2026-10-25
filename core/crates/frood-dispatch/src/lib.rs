//! Dispatcher (docs/13 §2): reads the journal with its own checkpoint, applies the rules of
//! the jobs, queues runs (one per job and event) and executes them with retries.
//!
//! Jobs react to journal events or to schedules (a schedule's tick is a journal event too);
//! agent jobs run in a runtime module and may wait for people's answers.

mod agents;
pub mod connections;
mod detectors;
mod digest;
pub mod health;
pub mod inbox;
mod jobs;
mod mcp;
pub mod oauth;
pub mod questions;
pub mod runs;
mod schedules;
mod sources;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use frood_ledger::Ledger;
use frood_ledger::model::Proposal;
use frood_llm::Gateway;
use frood_modhost::Modules;
use frood_search::Searcher;
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use tokio::sync::watch;
use uuid::Uuid;

pub use inbox::{Act, ActOutcome, InboxItem, Shown};
pub use jobs::backup::BackupSetup;
pub use jobs::{
    AgentSettings, Capabilities, JobError, JobSpec, Task, resolve, standard_jobs,
    standard_jobs_triggers,
};
pub use questions::Question;
pub use runs::Run;

const READER: &str = "dispatcher";
const EVENTS_PER_PASS: i64 = 200;

/// What jobs work with.
#[derive(Clone)]
pub(crate) struct Ctx {
    pub pool: PgPool,
    pub auth: frood_auth::Auth,
    pub ledger: Ledger,
    pub searcher: Searcher,
    pub gateway: Gateway,
    pub modules: Modules,
    /// The sources agents read: frood.yaml's and those connected from the console.
    pub sources: connections::Sources,
    /// Sources of frood.yaml alone; they win over the console's of the same name.
    pub config_sources: Arc<BTreeMap<String, frood_config::Source>>,
    /// Opens tokens the console stored.
    pub vault: Arc<connections::Vault>,
    pub attention: Arc<frood_config::Attention>,
    pub backup: Arc<Option<jobs::backup::BackupSetup>>,
    /// The installation's spheres (docs/26 §3); empty when they are off.
    pub spheres: Arc<Vec<frood_config::Sphere>>,
}

#[derive(Clone)]
pub struct Dispatch {
    ctx: Ctx,
    /// The jobs as they are now: frood.yaml's and the console's changes (docs/26 §4).
    jobs: Arc<std::sync::RwLock<Arc<Vec<JobSpec>>>>,
    /// What the jobs are made from again when the console changes one.
    job_base: Option<Arc<connections::JobBase>>,
    /// Since when each module has not answered (detectors).
    down_since: Arc<std::sync::Mutex<BTreeMap<String, chrono::DateTime<chrono::Utc>>>>,
}

/// Processing of one source: its runs and the proposals that cite it.
#[derive(Debug, Serialize)]
pub struct SourceStatus {
    pub source_id: Uuid,
    /// `queued`, `working`, `done`, `failed` or `idle` (nothing to do).
    pub state: &'static str,
    pub runs: Vec<Run>,
    pub proposals: Vec<Proposal>,
}

impl Dispatch {
    pub fn new(
        ledger: Ledger,
        auth: frood_auth::Auth,
        searcher: Searcher,
        gateway: Gateway,
        modules: Modules,
        jobs: Vec<JobSpec>,
    ) -> Self {
        Self {
            ctx: Ctx {
                pool: ledger.pool().clone(),
                auth,
                ledger,
                searcher,
                gateway,
                modules,
                sources: connections::Sources::default(),
                config_sources: Arc::new(BTreeMap::new()),
                vault: Arc::new(connections::Vault::new(None)),
                attention: Arc::new(frood_config::Attention::default()),
                backup: Arc::new(None),
                spheres: Arc::new(Vec::new()),
            },
            jobs: Arc::new(std::sync::RwLock::new(Arc::new(jobs))),
            job_base: None,
            down_since: Arc::default(),
        }
    }

    /// Who is told what, when and where.
    pub fn with_attention(mut self, attention: frood_config::Attention) -> Self {
        self.ctx.attention = Arc::new(attention);
        self
    }

    /// The spheres extraction sorts knowledge into.
    pub fn with_spheres(mut self, spheres: Vec<frood_config::Sphere>) -> Self {
        self.ctx.spheres = Arc::new(spheres);
        self
    }

    /// Where backups go (C4).
    pub fn with_backup(mut self, setup: BackupSetup) -> Self {
        self.ctx.backup = Arc::new(Some(setup));
        self
    }

    /// What a backup takes besides the database: the config as written and the secrets as
    /// `KEY=value` lines.
    pub fn with_backup_files(mut self, config_yaml: String, secrets_env: String) -> Self {
        if let Some(setup) = Arc::make_mut(&mut self.ctx.backup) {
            setup.config_yaml = Some(config_yaml);
            setup.secrets_env = Some(secrets_env);
        }
        self
    }

    /// The sources agents read; schedules with `for_each: source` run once for each.
    pub fn with_sources(mut self, sources: BTreeMap<String, frood_config::Source>) -> Self {
        self.ctx.sources.set(sources.clone());
        self.ctx.config_sources = Arc::new(sources);
        self
    }

    /// The key that seals tokens the console stores (`FROOD_VAULT_KEY`, an age identity).
    pub fn with_vault(mut self, key: Option<String>) -> Self {
        self.ctx.vault = Arc::new(connections::Vault::new(key.as_deref()));
        self
    }

    /// The jobs this installation runs now.
    pub fn jobs(&self) -> Arc<Vec<JobSpec>> {
        self.jobs.read().expect("jobs lock").clone()
    }

    /// What jobs are made from: the config and what the installation can run. With it, the
    /// console can change jobs.
    pub fn with_job_base(mut self, base: connections::JobBase) -> Self {
        self.job_base = Some(Arc::new(base));
        self
    }

    /// Replaces the jobs, as when the console changes one.
    pub fn set_jobs(&self, jobs: Vec<JobSpec>) {
        *self.jobs.write().expect("jobs lock") = Arc::new(jobs);
    }

    /// Turns new journal events into runs. Returns how many events it read.
    pub async fn dispatch_pending(&self) -> sqlx::Result<usize> {
        let mut tx = self.ctx.pool.begin().await?;
        sqlx::query(
            "INSERT INTO journal_checkpoints (reader, position) VALUES ($1, 0) ON CONFLICT DO NOTHING",
        )
        .bind(READER)
        .execute(&mut *tx)
        .await?;
        let checkpoint: i64 = sqlx::query_scalar(
            "SELECT position FROM journal_checkpoints WHERE reader = $1 FOR UPDATE",
        )
        .bind(READER)
        .fetch_one(&mut *tx)
        .await?;
        let events = sqlx::query(
            "SELECT position, id, type, subject, actor, data, occurred_at FROM journal \
             WHERE position > $1 ORDER BY position LIMIT $2",
        )
        .bind(checkpoint)
        .bind(EVENTS_PER_PASS)
        .fetch_all(&mut *tx)
        .await?;
        let Some(last) = events.last() else {
            return Ok(0);
        };
        let last: i64 = last.try_get("position")?;
        for event in &events {
            let event_type: String = event.try_get("type")?;
            let data: Value = event.try_get("data")?;
            let id: Uuid = event.try_get("id")?;
            let position: i64 = event.try_get("position")?;
            let subject: Option<String> = event.try_get("subject")?;
            for job in self.jobs().iter().filter(|j| j.matches(&event_type, &data)) {
                let input = json!({ "event": {
                    "position": position, "id": id, "type": event_type, "subject": subject,
                    "actor": event.try_get::<String, _>("actor")?, "data": data,
                } });
                sqlx::query(
                    "INSERT INTO runs (id, job, idempotency_key, subject, event_position, input, status) \
                     VALUES ($1, $2, $3, $4, $5, $6, 'queued') ON CONFLICT (idempotency_key) DO NOTHING",
                )
                .bind(Uuid::now_v7())
                .bind(&job.name)
                .bind(format!("{}:{id}", job.name))
                .bind(&subject)
                .bind(position)
                .bind(&input)
                .execute(&mut *tx)
                .await?;
            }
        }
        sqlx::query(
            "UPDATE journal_checkpoints SET position = $2, updated_at = now() WHERE reader = $1",
        )
        .bind(READER)
        .bind(last)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(events.len())
    }

    /// Executes the next due run, if any. Returns its id.
    pub async fn work_one(&self) -> sqlx::Result<Option<Uuid>> {
        let Some(run) = runs::claim(&self.ctx.pool).await? else {
            return Ok(None);
        };
        let jobs = self.jobs();
        let Some(spec) = jobs.iter().find(|j| j.name == run.job) else {
            runs::cancel(&self.ctx.pool, run.id, "the job is turned off").await?;
            return Ok(Some(run.id));
        };
        let started = std::time::Instant::now();
        if spec.task == Task::Agent {
            // The agent runs in its runtime; the watcher sees it end.
            let settings = spec.agent.as_ref().expect("agent jobs have settings");
            if let Err(err) = agents::start_or_resume(&self.ctx, spec, settings, &run).await {
                let retry = (err.transient && run.attempts < runs::MAX_ATTEMPTS)
                    .then(|| runs::backoff(run.attempts));
                tracing::warn!(run = %run.id, job = %run.job, error = %err.message, "agent did not start");
                runs::fail(&self.ctx.pool, run.id, &err.message, retry).await?;
            } else {
                tracing::info!(run = %run.id, job = %run.job, runtime = %settings.runtime, "agent started");
            }
            return Ok(Some(run.id));
        }
        let result = match spec.task {
            Task::Extract => jobs::extract::run(&self.ctx, spec, &run).await,
            Task::Transcribe => jobs::transcribe::run(&self.ctx, spec, &run).await,
            Task::Embed => jobs::embed::run(&self.ctx).await,
            Task::Digest => self.run_digest(&run).await,
            Task::Backup => jobs::backup::run(&self.ctx, false).await,
            Task::BackupVerify => jobs::backup::run(&self.ctx, true).await,
            Task::Agent => unreachable!("handled above"),
        };
        match result {
            Ok(output) => {
                tracing::info!(run = %run.id, job = %run.job, ms = started.elapsed().as_millis() as u64, "run succeeded");
                runs::succeed(&self.ctx.pool, run.id, &output).await?;
            }
            Err(err) => {
                let retry = (err.transient && run.attempts < runs::MAX_ATTEMPTS)
                    .then(|| runs::backoff(run.attempts));
                tracing::warn!(run = %run.id, job = %run.job, attempt = run.attempts, retry_in = ?retry, error = %err.message, "run failed");
                runs::fail(&self.ctx.pool, run.id, &err.message, retry).await?;
            }
        }
        Ok(Some(run.id))
    }

    /// Dispatches and works until nothing is left (tests and one-off catch-up).
    pub async fn drain(&self) -> sqlx::Result<usize> {
        let mut done = 0;
        loop {
            self.dispatch_pending().await?;
            match self.work_one().await? {
                Some(_) => done += 1,
                None => {
                    if self.dispatch_pending().await? == 0 {
                        return Ok(done);
                    }
                }
            }
        }
    }

    /// Runs the dispatcher and `workers` workers until `shutdown` turns true.
    pub async fn start(self, workers: usize, shutdown: watch::Receiver<bool>) {
        match runs::requeue_interrupted(&self.ctx.pool).await {
            Ok(n) if n > 0 => tracing::info!(runs = n, "requeued runs interrupted by a restart"),
            Ok(_) => {}
            Err(err) => tracing::error!(error = %err, "cannot requeue interrupted runs"),
        }
        let jobs = self.jobs();
        let names: Vec<&str> = jobs.iter().map(|j| j.name.as_str()).collect();
        tracing::info!(jobs = ?names, workers, "dispatcher started");
        let mut tasks = Vec::new();
        {
            let this = self.clone();
            let mut shutdown = shutdown.clone();
            tasks.push(tokio::spawn(async move {
                loop {
                    let read = match this.dispatch_pending().await {
                        Ok(n) => n,
                        Err(err) => {
                            tracing::error!(error = %err, "dispatch failed");
                            0
                        }
                    };
                    if read == 0
                        && tokio::time::timeout(Duration::from_millis(500), shutdown.changed())
                            .await
                            .is_ok()
                    {
                        break;
                    }
                    if *shutdown.borrow() {
                        break;
                    }
                }
            }));
        }
        {
            let this = self.clone();
            let mut shutdown = shutdown.clone();
            tasks.push(tokio::spawn(async move {
                loop {
                    if let Err(err) = this.tick_schedules(chrono::Utc::now()).await {
                        tracing::error!(error = %err, "schedules failed");
                    }
                    if tokio::time::timeout(Duration::from_secs(15), shutdown.changed())
                        .await
                        .is_ok()
                        || *shutdown.borrow()
                    {
                        break;
                    }
                }
            }));
        }
        {
            let this = self.clone();
            let mut shutdown = shutdown.clone();
            tasks.push(tokio::spawn(async move {
                let mut reminded: Option<std::time::Instant> = None;
                loop {
                    if let Err(err) = this.inbox_pending().await {
                        tracing::error!(error = %err, "Inbox failed to read the journal");
                    }
                    if reminded.is_none_or(|at| at.elapsed() > Duration::from_secs(300)) {
                        reminded = Some(std::time::Instant::now());
                        if let Err(err) = this.remind_once(chrono::Utc::now()).await {
                            tracing::error!(error = %err, "reminders failed");
                        }
                        if let Err(err) = this.detect_once(chrono::Utc::now()).await {
                            tracing::error!(error = %err, "detectors failed");
                        }
                    }
                    if let Err(err) = this.deliver_once().await {
                        tracing::error!(error = %err, "Inbox delivery failed");
                    }
                    if tokio::time::timeout(Duration::from_secs(2), shutdown.changed())
                        .await
                        .is_ok()
                        || *shutdown.borrow()
                    {
                        break;
                    }
                }
            }));
        }
        {
            let this = self.clone();
            let mut shutdown = shutdown.clone();
            tasks.push(tokio::spawn(async move {
                loop {
                    if let Err(err) = this.watch_once().await {
                        tracing::error!(error = %err, "watching agent runs failed");
                    }
                    if tokio::time::timeout(Duration::from_secs(2), shutdown.changed())
                        .await
                        .is_ok()
                        || *shutdown.borrow()
                    {
                        break;
                    }
                }
            }));
        }
        for _ in 0..workers.max(1) {
            let this = self.clone();
            let mut shutdown = shutdown.clone();
            tasks.push(tokio::spawn(async move {
                loop {
                    let worked = match this.work_one().await {
                        Ok(run) => run.is_some(),
                        Err(err) => {
                            tracing::error!(error = %err, "worker failed");
                            false
                        }
                    };
                    if !worked
                        && tokio::time::timeout(Duration::from_millis(500), shutdown.changed())
                            .await
                            .is_ok()
                    {
                        break;
                    }
                    if *shutdown.borrow() {
                        break;
                    }
                }
            }));
        }
        for task in tasks {
            let _ = task.await;
        }
    }

    /// The jobs this installation runs, as the console shows them: name, what they use, when.
    pub fn jobs_shown(&self) -> Vec<Value> {
        self.jobs()
            .iter()
            .map(|j| json!({ "name": j.name, "uses": j.uses, "triggers": j.triggers }))
            .collect()
    }

    /// A person takes back a note they sent (docs/25 §4): only their own.
    pub async fn withdraw_source(
        &self,
        source_id: Uuid,
        user: Uuid,
        actor: &str,
    ) -> Result<frood_ledger::model::Withdrawn, String> {
        let source = self
            .ctx
            .ledger
            .source(source_id)
            .await
            .map_err(|e| e.to_string())?;
        let own = format!("user:{user}");
        if source.captured_by != own && !source.captured_by.starts_with(&format!("{own}/")) {
            return Err("only the person who sent a note can take it back".to_owned());
        }
        let actor = frood_ledger::model::Actor::parse(actor).map_err(|e| e.to_string())?;
        self.ctx
            .ledger
            .withdraw_source(&actor, source_id)
            .await
            .map_err(|e| e.to_string())
    }

    /// How processing of a source goes.
    pub async fn source_status(
        &self,
        source_id: Uuid,
    ) -> Result<SourceStatus, frood_ledger::LedgerError> {
        self.ctx.ledger.source(source_id).await?;
        let subject = format!("source/{source_id}");
        let runs = runs::for_subject(&self.ctx.pool, &subject).await?;
        let proposals = self.ctx.ledger.proposals_citing_source(source_id).await?;
        // Events about the source the dispatcher has not read yet still make runs: between
        // `transcribe` finishing and `extract` being queued, nothing is running, yet the
        // source is not done.
        let undispatched: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM journal WHERE subject = $1 AND position > \
             coalesce((SELECT position FROM journal_checkpoints WHERE reader = $2), 0))",
        )
        .bind(&subject)
        .bind(READER)
        .fetch_one(&self.ctx.pool)
        .await?;
        let state = if runs.iter().any(|r| r.status == "running") {
            "working"
        } else if undispatched || runs.iter().any(|r| r.status == "queued") {
            // A failed attempt waiting for its retry counts as queued.
            "queued"
        } else if runs.iter().any(|r| r.status == "failed") {
            "failed"
        } else if runs.is_empty() {
            "idle"
        } else {
            "done"
        };
        Ok(SourceStatus {
            source_id,
            state,
            runs,
            proposals,
        })
    }

    pub async fn run(&self, id: Uuid) -> sqlx::Result<Option<Run>> {
        runs::get(&self.ctx.pool, id).await
    }
}
