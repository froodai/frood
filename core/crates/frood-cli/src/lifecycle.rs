//! `plan`, `apply`, `status`, `probe`.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, bail};
use frood_config::{Loaded, ServerPlacement};
use frood_store::RecordOutcome;

use crate::compose;

/// Resolved locations of the config and its secrets file.
pub struct Paths {
    pub config: PathBuf,
    secrets: Option<PathBuf>,
}

impl Paths {
    pub fn new(config: PathBuf, secrets: Option<PathBuf>) -> Self {
        Self { config, secrets }
    }

    pub fn load(&self) -> anyhow::Result<Loaded> {
        frood_config::load(&self.config, self.secrets.as_deref())
            .with_context(|| format!("cannot load {}", self.config.display()))
    }

    pub fn secrets(&self) -> PathBuf {
        self.secrets
            .clone()
            .unwrap_or_else(|| frood_config::default_secrets_path(&self.config))
    }
}

pub async fn plan(paths: &Paths) -> anyhow::Result<()> {
    let loaded = paths.load()?;
    print_summary(&loaded);
    match frood_store::connect(&loaded.config.storage.url, Duration::from_secs(2)).await {
        Ok(pool) => match frood_store::last_config_revision(&pool).await {
            Ok(Some(last)) if last.sha256 == loaded.sha256 => {
                println!(
                    "config: unchanged since revision #{} ({})",
                    last.id, last.applied_at
                );
            }
            Ok(Some(last)) => println!(
                "config: changed; apply records a new revision (last: #{})",
                last.id
            ),
            Ok(None) => println!("config: never applied; apply records revision #1"),
            Err(_) => println!("config: database is up but not migrated yet; apply migrates it"),
        },
        Err(_) => println!("config: database not reachable; apply starts it"),
    }
    Ok(())
}

pub async fn apply(paths: &Paths, render_only: bool) -> anyhow::Result<()> {
    let loaded = paths.load()?;
    print_summary(&loaded);

    let config_abs = absolute(&paths.config)?;
    let secrets_abs = absolute(&paths.secrets())?;
    if !secrets_abs.exists() {
        bail!(
            "secrets file {} does not exist; run `frood init` or create it",
            secrets_abs.display()
        );
    }
    let deploy_dir = loaded.base_dir.join(".frood").join("deploy");
    std::fs::create_dir_all(&deploy_dir)
        .with_context(|| format!("cannot create {}", deploy_dir.display()))?;
    let compose_path = deploy_dir.join("compose.yaml");
    std::fs::write(
        &compose_path,
        compose::render(&loaded, &config_abs, &secrets_abs),
    )
    .with_context(|| format!("cannot write {}", compose_path.display()))?;
    println!("wrote {}", compose_path.display());
    if render_only {
        return Ok(());
    }

    docker_compose_up(&compose_path, loaded.config.deploy.build.is_some()).await?;

    let pool = frood_store::connect(&loaded.config.storage.url, Duration::from_secs(60))
        .await
        .context("database did not become reachable")?;
    frood_store::migrate(&pool)
        .await
        .context("migrations failed")?;
    match frood_store::record_config_revision(&pool, &loaded.sha256, &loaded.raw, "file").await? {
        RecordOutcome::Recorded(revision) => println!("recorded config revision #{}", revision.id),
        RecordOutcome::Unchanged(revision) => {
            println!("config unchanged since revision #{}", revision.id)
        }
    }

    match loaded.config.deploy.server {
        ServerPlacement::Container => {
            let url = format!("http://127.0.0.1:{}/readyz", compose::listen_port(&loaded));
            if wait_ready(&url, Duration::from_secs(60)).await {
                println!("server is ready: {url}");
            } else {
                bail!(
                    "server did not become ready at {url}; see `docker compose -p {} logs frood`",
                    compose::project_name(&loaded)
                );
            }
        }
        ServerPlacement::Host => {
            println!(
                "database is ready; start the server with: frood --config {} serve",
                paths.config.display()
            );
        }
    }
    Ok(())
}

pub async fn status(paths: &Paths) -> anyhow::Result<()> {
    let loaded = paths.load()?;
    let url = format!("http://{}/readyz", local_address(&loaded));
    match http_status(&url).await {
        Some(200) => println!("server: ready ({url})"),
        Some(code) => println!("server: answering but not ready, HTTP {code} ({url})"),
        None => println!("server: not reachable ({url})"),
    }
    match frood_store::connect(&loaded.config.storage.url, Duration::from_secs(2)).await {
        Ok(pool) => match frood_store::last_config_revision(&pool).await {
            Ok(Some(last)) => {
                let state = if last.sha256 == loaded.sha256 {
                    "matches the file"
                } else {
                    "differs from the file; run apply"
                };
                println!(
                    "config: revision #{} applied {} ({state})",
                    last.id, last.applied_at
                );
            }
            Ok(None) => println!("config: never applied"),
            Err(err) => println!("config: cannot read revisions: {err}"),
        },
        Err(_) => println!("database: not reachable"),
    }
    Ok(())
}

pub async fn probe(url: &str) -> anyhow::Result<()> {
    match http_status(url).await {
        Some(200) => Ok(()),
        Some(code) => bail!("{url} answered HTTP {code}"),
        None => bail!("{url} is not reachable"),
    }
}

fn print_summary(loaded: &Loaded) {
    let config = &loaded.config;
    println!("installation: {}", config.installation.name);
    println!("storage: {}", config.storage.uses);
    let source = match (&config.deploy.build, &config.deploy.image) {
        (_, _) if config.deploy.server == ServerPlacement::Host => {
            "you run `frood serve`".to_owned()
        }
        (Some(build), _) => format!("built from {build}"),
        (None, Some(image)) => image.clone(),
        (None, None) => "?".to_owned(),
    };
    println!(
        "deploy: {} (server: {}, {source})",
        config.deploy.target,
        config.deploy.server.as_str()
    );
    println!("listen: {}", config.server.listen);
    if let Some(url) = &config.server.public_url {
        println!("public url: {url}");
    }
    if !loaded.secrets_used.is_empty() {
        let names: Vec<&str> = loaded.secrets_used.iter().map(String::as_str).collect();
        println!("secrets: {} (resolved)", names.join(", "));
    }
    for warning in &loaded.warnings {
        println!("warning: {warning}");
    }
}

async fn docker_compose_up(compose_path: &Path, build: bool) -> anyhow::Result<()> {
    let mut command = tokio::process::Command::new("docker");
    command
        .args(["compose", "-f"])
        .arg(compose_path)
        .args(["up", "-d", "--wait"]);
    if build {
        command.arg("--build");
    }
    let status = command
        .status()
        .await
        .context("cannot run `docker compose`; is Docker installed?")?;
    if !status.success() {
        bail!("`docker compose up` failed with {status}");
    }
    Ok(())
}

/// Where this machine reaches the server: unspecified bind addresses map to loopback.
fn local_address(loaded: &Loaded) -> SocketAddr {
    let mut addr = loaded
        .config
        .server
        .listen
        .parse::<SocketAddr>()
        .unwrap_or_else(|_| ([127, 0, 0, 1], 8080).into());
    if addr.ip().is_unspecified() {
        addr.set_ip([127, 0, 0, 1].into());
    }
    addr
}

async fn wait_ready(url: &str, wait: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + wait;
    while tokio::time::Instant::now() < deadline {
        if http_status(url).await == Some(200) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    false
}

async fn http_status(url: &str) -> Option<u16> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .ok()?;
    client
        .get(url)
        .send()
        .await
        .ok()
        .map(|response| response.status().as_u16())
}

fn absolute(path: &Path) -> anyhow::Result<PathBuf> {
    std::path::absolute(path).with_context(|| format!("cannot resolve {}", path.display()))
}
