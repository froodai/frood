//! Dispatcher (docs/13 §2): reads the journal with its own checkpoint, applies the rules of
//! the jobs, queues runs (one per job and event) and executes them with retries.
//!
//! M3 has event rules and the standard jobs `transcribe`, `extract` and `embed`; schedules,
//! agents and questions come with M4.

mod agents;
mod jobs;
pub mod questions;
pub mod runs;

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

pub use jobs::{AgentSettings, Capabilities, JobError, JobSpec, Task, resolve};
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
}

#[derive(Clone)]
pub struct Dispatch {
    ctx: Ctx,
    jobs: Arc<Vec<JobSpec>>,
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
            },
            jobs: Arc::new(jobs),
        }
    }

    pub fn jobs(&self) -> &[JobSpec] {
        &self.jobs
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
            for job in self.jobs.iter().filter(|j| j.matches(&event_type, &data)) {
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
        let Some(spec) = self.jobs.iter().find(|j| j.name == run.job) else {
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
        let names: Vec<&str> = self.jobs.iter().map(|j| j.name.as_str()).collect();
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
