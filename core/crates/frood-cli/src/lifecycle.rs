//! `plan`, `apply`, `status`, `probe`.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, bail};
use frood_config::{Loaded, ServerPlacement};
use frood_store::RecordOutcome;

use crate::{compose, modules};

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
    let secrets = frood_config::Secrets::from_file(&paths.secrets())?;
    for (name, module) in &loaded.config.modules {
        let source = match (&module.url, &module.build, &module.image) {
            (Some(url), _, _) => format!("runs at {url}"),
            (None, Some(build), _) => format!("built from {build}"),
            (None, None, Some(image)) => image.clone(),
            _ => "?".to_owned(),
        };
        let missing: Vec<&str> = module
            .secrets
            .iter()
            .filter(|s| secrets.get(s).is_none())
            .map(String::as_str)
            .collect();
        if module.host_docker {
            println!(
                "module {name}: gets the host's Docker, which is root on this machine (host_docker: true)"
            );
        }
        if missing.is_empty() {
            println!("module {name}: {source}");
        } else {
            println!(
                "module {name}: {source}; missing in the secrets file: {}",
                missing.join(", ")
            );
        }
    }
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

/// Makes the files folder (docs/26 §2) with the mark the server looks for. The server and the
/// backup module run as uid 10001: when this runs as root, the folder becomes theirs.
fn prepare_objects(loaded: &Loaded) -> anyhow::Result<()> {
    let frood_config::Objects::Disk { path, .. } = &loaded.config.storage.objects else {
        return Ok(());
    };
    let folder = loaded
        .base_dir
        .join(path.as_deref().unwrap_or(".frood/objects"));
    std::fs::create_dir_all(&folder)
        .with_context(|| format!("cannot create {}", folder.display()))?;
    let mark = folder.join(frood_api::OBJECTS_MARK);
    if !mark.exists() {
        std::fs::write(&mark, b"").with_context(|| format!("cannot write {}", mark.display()))?;
    }
    #[cfg(unix)]
    if loaded.config.deploy.server == frood_config::ServerPlacement::Container && is_root() {
        give_to_server(&folder)?;
    }
    Ok(())
}

/// Hands a folder and everything in it (files a restore put there) to the server's uid 10001,
/// readable by others and writable only by it.
#[cfg(unix)]
fn give_to_server(path: &std::path::Path) -> anyhow::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    const SERVER: u32 = 10001;
    let meta = std::fs::symlink_metadata(path)?;
    if meta.uid() != SERVER {
        std::os::unix::fs::chown(path, Some(SERVER), Some(SERVER))
            .with_context(|| format!("cannot give {} to the server", path.display()))?;
    }
    let mode = if meta.is_dir() { 0o755 } else { 0o644 };
    if meta.mode() & 0o777 != mode {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    }
    if meta.is_dir() {
        for entry in std::fs::read_dir(path)? {
            give_to_server(&entry?.path())?;
        }
    }
    Ok(())
}

/// Only root may hand files to another user.
#[cfg(unix)]
fn is_root() -> bool {
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).trim() == "0")
}

/// Writes the deployment files (module env files, compose.yaml); returns compose.yaml's path.
pub(crate) fn write_deploy(paths: &Paths, loaded: &Loaded) -> anyhow::Result<PathBuf> {
    let config_abs = absolute(&paths.config)?;
    let secrets_abs = absolute(&paths.secrets())?;
    if !secrets_abs.exists() {
        bail!(
            "secrets file {} does not exist; run `frood init` or create it",
            secrets_abs.display()
        );
    }
    let deploy_dir = absolute(&loaded.base_dir.join(".frood").join("deploy"))?;
    std::fs::create_dir_all(&deploy_dir)
        .with_context(|| format!("cannot create {}", deploy_dir.display()))?;
    if modules::ensure_vault_key(&secrets_abs)? {
        println!(
            "added {} to the secrets file: it seals tokens the console stores; keep it with the other secrets",
            modules::VAULT_KEY
        );
    }
    for module in modules::ensure_tokens(loaded, &secrets_abs)? {
        println!("added a service token for module {module} to the secrets file");
    }
    modules::write_env_files(loaded, &secrets_abs, &deploy_dir)?;
    prepare_objects(loaded)?;
    let compose_path = deploy_dir.join("compose.yaml");
    std::fs::write(
        &compose_path,
        compose::render(loaded, &config_abs, &secrets_abs, &deploy_dir),
    )
    .with_context(|| format!("cannot write {}", compose_path.display()))?;
    println!("wrote {}", compose_path.display());
    if let Some(caddyfile) = compose::caddyfile(loaded) {
        if let Some(routes) = compose::proxy_routes(loaded)
            && !routes.is_file()
        {
            bail!("deploy.proxy.routes: {} does not exist", routes.display());
        }
        let dir = deploy_dir.join("caddy");
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("cannot create {}", dir.display()))?;
        std::fs::write(dir.join("Caddyfile"), caddyfile)
            .with_context(|| format!("cannot write {}", dir.display()))?;
    }
    if let (Some(file), Some(routes)) = (
        compose::proxy_external(loaded),
        compose::external_routes(loaded),
    ) {
        write_external_routes(&file, &routes)?;
    }
    Ok(compose_path)
}

/// Frood's file in the server's own proxy (docs/29): written whole, so a proxy that watches
/// its folder never reads half of it, and only when it changed. The proxy picks it up itself.
fn write_external_routes(file: &Path, routes: &str) -> anyhow::Result<()> {
    let dir = file.parent().filter(|d| d.is_dir()).with_context(|| {
        format!(
            "deploy.proxy.external: the proxy's folder for {} does not exist",
            file.display()
        )
    })?;
    if std::fs::read_to_string(file).is_ok_and(|now| now == routes) {
        return Ok(());
    }
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{name}.tmp"));
    std::fs::write(&tmp, routes).with_context(|| format!("cannot write {}", tmp.display()))?;
    std::fs::rename(&tmp, file).with_context(|| format!("cannot write {}", file.display()))?;
    println!("wrote {}; the proxy in front picks it up", file.display());
    Ok(())
}

pub async fn apply(paths: &Paths, render_only: bool) -> anyhow::Result<()> {
    let loaded = paths.load()?;
    print_summary(&loaded);
    let compose_path = write_deploy(paths, &loaded)?;
    if render_only {
        return Ok(());
    }

    let build = loaded.config.deploy.build.is_some()
        || modules::containers(&loaded).any(|(_, m)| m.build.is_some());
    // The database first: a new server migrates it as it starts, so a snapshot comes before.
    docker_compose(&compose_path, &["up", "-d", "--wait", "postgres"]).await?;
    snapshot_before_migrations(&loaded, &compose_path).await?;
    let caddy = compose::caddyfile(&loaded).is_some();
    if caddy {
        // A mistake in deploy.proxy.routes must not take the site down: check first.
        let check = [
            "run",
            "--rm",
            "--no-deps",
            "-T",
            "caddy",
            "caddy",
            "validate",
            "--config",
            compose::CADDYFILE,
            "--adapter",
            "caddyfile",
        ];
        docker_compose(&compose_path, &check)
            .await
            .context("Caddy refused the routes; nothing was changed")?;
    }
    docker_compose_up(&compose_path, build).await?;
    if caddy {
        // Compose recreates Caddy only when its service changed, not for a new Caddyfile.
        let reload = [
            "exec",
            "-T",
            "caddy",
            "caddy",
            "reload",
            "--config",
            compose::CADDYFILE,
            "--adapter",
            "caddyfile",
        ];
        if docker_compose(&compose_path, &reload).await.is_err() {
            println!(
                "warning: Caddy did not reload its routes; `docker compose -p {} restart caddy` does it",
                compose::project_name(&loaded)
            );
        }
    }

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
            let port = compose::listen_port(&loaded);
            let url = format!("http://127.0.0.1:{port}/readyz");
            if !wait_ready(&url, Duration::from_secs(60)).await {
                bail!(
                    "server did not become ready at {url}; see `docker compose -p {} logs frood`",
                    compose::project_name(&loaded)
                );
            }
            // The server reads frood.yaml at start. Compose recreates it for a new image, but
            // not when only the mounted config changed: restart it then.
            let running = loaded_config_sha(port).await;
            if running.as_deref() != Some(loaded.sha256.as_str()) {
                docker_compose(&compose_path, &["restart", "frood"]).await?;
                if !wait_ready(&url, Duration::from_secs(60)).await {
                    bail!(
                        "server did not come back after the restart; see `docker compose -p {} logs frood`",
                        compose::project_name(&loaded)
                    );
                }
                println!("restarted the server to load the new config");
            }
            println!("server is ready: {url}");
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

pub async fn export(paths: &Paths, dir: &std::path::Path) -> anyhow::Result<()> {
    let loaded = paths.load()?;
    let pool = frood_store::connect(&loaded.config.storage.url, Duration::from_secs(5))
        .await
        .context("the database is not reachable; run `frood apply` first")?;
    let objects = frood_api::files_on_host(&loaded.config, &loaded.base_dir)
        .map_err(|e| anyhow::anyhow!("the files store: {e}"))?;
    let exported =
        crate::export::export(&pool, &objects, &loaded.config.installation.name, dir).await?;
    for (file, count) in &exported.counts {
        println!("{file}: {count}");
    }
    println!("files: {}", exported.files);
    println!("entity pages: {}", exported.entities);
    println!("wrote {}", dir.display());
    Ok(())
}

/// Logs of the deployment's containers: the server, PostgreSQL, a module (`module-<name>`).
pub async fn logs(
    paths: &Paths,
    service: Option<&str>,
    follow: bool,
    lines: u32,
) -> anyhow::Result<()> {
    let loaded = paths.load()?;
    let compose_path = absolute(
        &loaded
            .base_dir
            .join(".frood")
            .join("deploy")
            .join("compose.yaml"),
    )?;
    if !compose_path.exists() {
        bail!(
            "{} does not exist; run `frood apply` first",
            compose_path.display()
        );
    }
    let service = service.map(|name| {
        if name == "frood" || name == "postgres" || name.starts_with("module-") {
            name.to_owned()
        } else {
            frood_modhost::service_name(name)
        }
    });
    let tail = lines.to_string();
    let mut args = vec!["logs", "--tail", tail.as_str(), "--timestamps"];
    if follow {
        args.push("--follow");
    }
    if let Some(service) = &service {
        args.push(service);
    }
    docker_compose(&compose_path, &args).await
}

pub async fn status(paths: &Paths) -> anyhow::Result<()> {
    let loaded = paths.load()?;
    let url = format!("http://{}/readyz", local_address(&loaded));
    match http_status(&url).await {
        Ok(200) => println!("server: ready ({url})"),
        Ok(code) => println!("server: answering but not ready, HTTP {code} ({url})"),
        Err(err) => println!("server: not reachable ({url}): {err}"),
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
        Ok(200) => Ok(()),
        Ok(code) => bail!("{url} answered HTTP {code}"),
        Err(err) => bail!("{url} is not reachable: {err}"),
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
        match compose::proxy_external(loaded) {
            Some(file) => println!(
                "proxy: the server's own, in front; Frood writes its routes to {}",
                file.display()
            ),
            None => println!("proxy: Frood's Caddy on ports 80 and 443"),
        }
    }
    if !loaded.secrets_used.is_empty() {
        let names: Vec<&str> = loaded.secrets_used.iter().map(String::as_str).collect();
        println!("secrets: {} (resolved)", names.join(", "));
    }
    for warning in &loaded.warnings {
        println!("warning: {warning}");
    }
}

/// The config hash the running server reports (`/v1/version`).
async fn loaded_config_sha(port: u16) -> Option<String> {
    let response = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/v1/version"))
        .timeout(Duration::from_secs(3))
        .send()
        .await
        .ok()?;
    let body: serde_json::Value = response.json().await.ok()?;
    body.get("config_sha256")?.as_str().map(str::to_owned)
}

/// Snapshots kept in `.frood/snapshots`.
const SNAPSHOTS_KEPT: usize = 5;

/// Before new migrations change an existing database: a local dump to roll back to (C5).
async fn snapshot_before_migrations(loaded: &Loaded, compose_path: &Path) -> anyhow::Result<()> {
    let pool = frood_store::connect(&loaded.config.storage.url, Duration::from_secs(60))
        .await
        .context("database did not become reachable")?;
    let (pending, existing) = frood_store::pending_migrations(&pool).await?;
    pool.close().await;
    if pending.is_empty() || !existing {
        return Ok(());
    }
    let dir = absolute(&loaded.base_dir.join(".frood").join("snapshots"))?;
    std::fs::create_dir_all(&dir)?;
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let path = dir.join(format!("{stamp}-before-{}.dump", pending[0]));
    let file = std::fs::File::create(&path)
        .with_context(|| format!("cannot create {}", path.display()))?;
    let status = tokio::process::Command::new("docker")
        .args(["compose", "-f"])
        .arg(compose_path)
        .args([
            "exec", "-T", "postgres", "pg_dump", "-U", "frood", "-d", "frood", "-Fc",
        ])
        .stdout(file)
        .status()
        .await
        .context("cannot run pg_dump in the postgres container")?;
    if !status.success() {
        let _ = std::fs::remove_file(&path);
        bail!("the snapshot before migrations failed ({status}); nothing was changed");
    }
    println!(
        "snapshot before migrations {:?}: {}",
        pending,
        path.display()
    );
    let mut dumps: Vec<_> = std::fs::read_dir(&dir)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "dump"))
        .collect();
    dumps.sort();
    let extra = dumps.len().saturating_sub(SNAPSHOTS_KEPT);
    for old in dumps.into_iter().take(extra) {
        let _ = std::fs::remove_file(old);
    }
    Ok(())
}

pub(crate) async fn docker_compose(compose_path: &Path, args: &[&str]) -> anyhow::Result<()> {
    let status = tokio::process::Command::new("docker")
        .args(["compose", "-f"])
        .arg(compose_path)
        .args(args)
        .status()
        .await
        .context("cannot run `docker compose`")?;
    if !status.success() {
        bail!("`docker compose {}` failed with {status}", args.join(" "));
    }
    Ok(())
}

async fn docker_compose_up(compose_path: &Path, build: bool) -> anyhow::Result<()> {
    let mut command = tokio::process::Command::new("docker");
    command
        .args(["compose", "-f"])
        .arg(compose_path)
        // What the config no longer has goes: a removed module, or Caddy once a proxy in
        // front serves the address.
        .args(["up", "-d", "--wait", "--remove-orphans"]);
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
        if matches!(http_status(url).await, Ok(200)) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    false
}

async fn http_status(url: &str) -> anyhow::Result<u16> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .context("cannot create an HTTP client")?;
    let response = client.get(url).send().await.map_err(|err| {
        // reqwest's Display omits the cause (connection refused, timeout, ...).
        let mut message = err.to_string();
        let mut source = std::error::Error::source(&err);
        while let Some(cause) = source {
            message = format!("{message}: {cause}");
            source = cause.source();
        }
        anyhow::anyhow!(message)
    })?;
    Ok(response.status().as_u16())
}

pub(crate) fn absolute(path: &Path) -> anyhow::Result<PathBuf> {
    std::path::absolute(path).with_context(|| format!("cannot resolve {}", path.display()))
}

/// The console's sources as `frood.yaml` lines (docs/26 §4). A sealed token becomes a
/// `${secret:SOURCE_<NAME>_TOKEN}` reference; its value is not printed.
pub async fn config_export(paths: &Paths) -> anyhow::Result<()> {
    let loaded = paths.load()?;
    let pool = frood_store::connect(&loaded.config.storage.url, Duration::from_secs(5))
        .await
        .context("the database is not reachable; run `frood apply` first")?;
    let data: Option<serde_json::Value> = frood_store::sqlx::query_scalar(
        "SELECT data FROM config_overlay ORDER BY revision DESC LIMIT 1",
    )
    .fetch_optional(&pool)
    .await?;
    let data = data.unwrap_or_default();
    let sources: std::collections::BTreeMap<String, frood_config::Source> =
        serde_json::from_value(data["sources"].clone()).unwrap_or_default();
    let jobs: std::collections::BTreeMap<String, frood_config::Job> =
        serde_json::from_value(data["jobs"].clone()).unwrap_or_default();
    let attention: Option<frood_config::Attention> =
        serde_json::from_value(data["attention"].clone()).ok();
    if sources.is_empty() && jobs.is_empty() && attention.is_none() {
        println!("# Nothing was changed from the console.");
        return Ok(());
    }
    let (sources, secrets) = exported_sources(sources);
    println!(
        "# Changes made from the console; add them to frood.yaml (what frood.yaml sets already wins)."
    );
    if !secrets.is_empty() {
        println!(
            "# Their tokens stay in the database: put them in the secrets file as {}.",
            secrets.join(", ")
        );
    }
    let mut out = serde_json::Map::new();
    if !sources.is_empty() {
        out.insert("sources".into(), serde_json::to_value(sources)?);
    }
    if !jobs.is_empty() {
        out.insert("jobs".into(), serde_json::to_value(jobs)?);
    }
    if let Some(attention) = attention {
        out.insert("attention".into(), serde_json::to_value(attention)?);
    }
    print!("{}", serde_yaml_ng::to_string(&out)?);
    Ok(())
}

/// Sources with each sealed token replaced by a secret reference, and the secrets named.
fn exported_sources(
    mut sources: std::collections::BTreeMap<String, frood_config::Source>,
) -> (
    std::collections::BTreeMap<String, frood_config::Source>,
    Vec<String>,
) {
    let mut secrets = Vec::new();
    for (name, source) in &mut sources {
        for value in source.headers.values_mut() {
            if let Some(start) = value.find("${vault:")
                && let Some(end) = value[start..].find('}')
            {
                let secret = format!(
                    "SOURCE_{}_TOKEN",
                    name.to_ascii_uppercase().replace('-', "_")
                );
                value.replace_range(start..=start + end, &format!("${{secret:{secret}}}"));
                secrets.push(secret);
            }
        }
    }
    (sources, secrets)
}

#[cfg(test)]
mod export_tests {
    use super::*;

    #[test]
    fn sealed_tokens_become_secret_references() {
        let source: frood_config::Source = serde_json::from_value(serde_json::json!({
            "uses": "mcp", "server": "https://mcp.linear.app/mcp",
            "headers": { "Authorization": "Bearer ${vault:source.team-tracker}" }
        }))
        .unwrap();
        let (sources, secrets) = exported_sources([("team-tracker".to_owned(), source)].into());
        assert_eq!(
            sources["team-tracker"].headers["Authorization"],
            "Bearer ${secret:SOURCE_TEAM_TRACKER_TOKEN}"
        );
        assert_eq!(secrets, ["SOURCE_TEAM_TRACKER_TOKEN"]);
    }
}

#[cfg(test)]
mod proxy_tests {
    use super::*;

    #[test]
    fn the_routes_file_is_written_whole_and_only_when_it_changes() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("frood.caddy");
        write_external_routes(&file, "handle_path /frood/* {}\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "handle_path /frood/* {}\n"
        );
        let written = std::fs::metadata(&file).unwrap().modified().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        write_external_routes(&file, "handle_path /frood/* {}\n").unwrap();
        assert_eq!(
            std::fs::metadata(&file).unwrap().modified().unwrap(),
            written
        );
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(names, ["frood.caddy"], "no temporary file is left");
        let missing = write_external_routes(&dir.path().join("no/frood.caddy"), "x").unwrap_err();
        assert!(missing.to_string().contains("does not exist"), "{missing}");
    }
}
