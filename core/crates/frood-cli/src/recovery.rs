//! `frood backup key` and `frood restore` (docs/16 §7, C4): the recovery key that opens the
//! secrets in backups, and a new installation from a backup on a clean machine.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use age::secrecy::ExposeSecret;
use anyhow::{Context, bail};

use crate::lifecycle::{self, Paths, absolute};

/// Makes a recovery key: its secret half to keep off the server, its public half for
/// `backup.recovery_key`.
pub fn key(out: Option<&Path>) -> anyhow::Result<()> {
    let identity = age::x25519::Identity::generate();
    let public = identity.to_public().to_string();
    let secret = identity.to_string();
    let text = format!(
        "# Frood recovery key, made {}.\n# Keep it off the server (a password manager, paper): restoring a backup needs it.\n# public key: {public}\n{}\n",
        chrono::Utc::now().format("%Y-%m-%d"),
        secret.expose_secret()
    );
    match out {
        Some(path) => {
            if path.exists() {
                bail!("{} exists; choose another file", path.display());
            }
            write_private(path, text.as_bytes())?;
            println!("wrote the recovery key to {}", path.display());
        }
        None => print!("{text}"),
    }
    println!("\nIn frood.yaml:\n\nbackup:\n  recovery_key: {public}");
    Ok(())
}

/// A file only its owner can read.
fn write_private(path: &Path, data: &[u8]) -> anyhow::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("cannot create {}", path.display()))?;
    file.write_all(data)?;
    Ok(())
}

pub struct RestoreOptions {
    /// The restic repository: a folder on this machine, `s3:…`, `b2:…`, `sftp:…`, `rest:…`.
    pub repository: String,
    /// The secret half of the recovery key: a file as `frood backup key` wrote it.
    pub identity: PathBuf,
    /// The image of the backup module, which has restic.
    pub image: String,
    /// `KEY=VALUE` for the repository, e.g. S3 credentials.
    pub env: Vec<String>,
    /// A restic snapshot id; the latest when omitted.
    pub snapshot: Option<String>,
    /// Ports on this machine instead of the backup's, when those are taken here.
    pub postgres_port: Option<u16>,
    pub listen: Option<String>,
}

/// The restored config with this machine's ports: `deploy.postgres_port` (and the database
/// URL's default port that follows it), and `server.listen`.
fn with_ports(text: &str, postgres_port: Option<u16>, listen: Option<&str>) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let indent = &line[..line.len() - trimmed.len()];
        let ending = if line.ends_with('\n') { "\n" } else { "" };
        if let Some(port) = postgres_port
            && trimmed.starts_with("postgres_port:")
        {
            out.push_str(&format!("{indent}postgres_port: {port}{ending}"));
        } else if let Some(listen) = listen
            && trimmed.starts_with("listen:")
        {
            out.push_str(&format!("{indent}listen: \"{listen}\"{ending}"));
        } else if let Some(port) = postgres_port
            && let Some(at) = line.find("FROOD_DB_PORT:-")
        {
            let start = at + "FROOD_DB_PORT:-".len();
            let digits = line[start..]
                .chars()
                .take_while(char::is_ascii_digit)
                .count();
            out.push_str(&line[..start]);
            out.push_str(&port.to_string());
            out.push_str(&line[start + digits..]);
        } else {
            out.push_str(line);
        }
    }
    out
}

/// What already holds a port on this machine: a program, or a Docker container that publishes
/// it (Docker can hold a port that no listening socket shows).
fn port_taken(port: u16) -> Option<String> {
    // Only "in use" counts: a port below 1024 may just need root to bind.
    if let Err(err) = std::net::TcpListener::bind(("127.0.0.1", port))
        && err.kind() == std::io::ErrorKind::AddrInUse
    {
        return Some("a program on this machine".to_owned());
    }
    let listed = std::process::Command::new("docker")
        .args(["ps", "--format", "{{.Names}}\t{{.Ports}}"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&listed.stdout)
        .lines()
        .find(|line| line.contains(&format!(":{port}->")))
        .map(|line| format!("container {}", line.split('\t').next().unwrap_or("?")))
}

pub async fn restore(paths: &Paths, options: &RestoreOptions) -> anyhow::Result<()> {
    let config_path = absolute(&paths.config)?;
    let secrets_path = absolute(&paths.secrets())?;
    for existing in [&config_path, &secrets_path] {
        if existing.exists() {
            bail!(
                "{} exists: restore into a new folder (--config <folder>/frood.yaml)",
                existing.display()
            );
        }
    }
    let password = std::env::var("RESTIC_PASSWORD")
        .context("set RESTIC_PASSWORD to the repository's password")?;
    let identity_text = std::fs::read_to_string(&options.identity)
        .with_context(|| format!("cannot read {}", options.identity.display()))?;
    let identity: age::x25519::Identity = identity_text
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("AGE-SECRET-KEY-"))
        .context("the identity file has no AGE-SECRET-KEY line")?
        .parse()
        .map_err(|e| anyhow::anyhow!("the recovery key does not parse: {e}"))?;

    let base = config_path
        .parent()
        .context("the config has no folder")?
        .to_owned();
    let work = base.join(".frood").join("restore");
    if work.exists() {
        std::fs::remove_dir_all(&work)?;
    }
    std::fs::create_dir_all(&work)?;
    // restic may run as another user than this one (below); it only writes the restored files.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&work, std::fs::Permissions::from_mode(0o777))?;
    }

    // restic from the backup module's image; a repository folder on this machine is mounted.
    let mut command = tokio::process::Command::new("docker");
    command.args(["run", "--rm"]);
    let local = Path::new(&options.repository);
    // As whoever wrote a repository folder — the backup module's user, whose files only it may
    // read — or else as this user.
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let owner = match std::fs::metadata(local.join("config")) {
            Ok(meta) if local.is_absolute() => format!("{}:{}", meta.uid(), meta.gid()),
            _ => {
                let uid = std::process::Command::new("id").arg("-u").output()?;
                let gid = std::process::Command::new("id").arg("-g").output()?;
                format!(
                    "{}:{}",
                    String::from_utf8_lossy(&uid.stdout).trim(),
                    String::from_utf8_lossy(&gid.stdout).trim()
                )
            }
        };
        command.args(["--user", &owner]);
    }
    let repository = if local.is_absolute() && local.exists() {
        command
            .arg("-v")
            .arg(format!("{}:/repo:ro", local.display()));
        "/repo".to_owned()
    } else {
        options.repository.clone()
    };
    command
        .arg("-v")
        .arg(format!("{}:/restore", work.display()))
        .args(["-e", &format!("RESTIC_REPOSITORY={repository}")])
        .args(["-e", "RESTIC_PASSWORD"])
        .env("RESTIC_PASSWORD", &password);
    for pair in &options.env {
        let (key, value) = pair
            .split_once('=')
            .with_context(|| format!("--env {pair:?} must be KEY=VALUE"))?;
        command.args(["-e", key]).env(key, value);
    }
    let snapshot = options.snapshot.as_deref().unwrap_or("latest");
    command
        .args(["--entrypoint", "sh", &options.image, "-c"])
        // The whole snapshot: the dump and the config under backup/, the files folder, when
        // there is one, under frood/objects/. Read only: no lock in the repository, which may
        // be mounted read-only. Then open to this user, who moves and removes what came back.
        .arg(
            "restic --no-cache --no-lock restore \"$0\" --tag frood --target /restore \
             && find /restore -mindepth 1 -exec chmod a+rwX {} +",
        )
        .arg(snapshot);
    println!("restoring {snapshot} from {}…", options.repository);
    let status = command
        .status()
        .await
        .context("cannot run docker; is it installed?")?;
    if !status.success() {
        bail!("restic could not restore from the repository ({status})");
    }

    let objects = work.join("frood").join("objects");
    let work = work.join("backup");
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(work.join("manifest.json")).context("the backup has no manifest.json")?,
    )?;
    println!(
        "backup of {} made {} (frood {})",
        manifest["installation"].as_str().unwrap_or("?"),
        manifest["made_at"].as_str().unwrap_or("?"),
        manifest["about"]["frood_version"].as_str().unwrap_or("?"),
    );
    let sealed =
        std::fs::read(work.join("secrets.env.age")).context("the backup has no secrets.env.age")?;
    let secrets = age::decrypt(&identity, &sealed)
        .map_err(|_| anyhow::anyhow!("this recovery key does not open the backup's secrets"))?;
    let config_text = with_ports(
        &std::fs::read_to_string(work.join("frood.yaml"))
            .context("the backup has no frood.yaml")?,
        options.postgres_port,
        options.listen.as_deref(),
    );

    // The ports it will use on this machine, checked before anything is written.
    let pairs = String::from_utf8_lossy(&secrets)
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .collect::<Vec<_>>();
    let config = frood_config::parse(&config_text, &frood_config::Secrets::from_pairs(pairs))
        .context("the restored frood.yaml does not load")?
        .config;
    let mut ports = vec![(
        config.deploy.postgres_port,
        "the database",
        "--postgres-port",
    )];
    if config.deploy.server == frood_config::ServerPlacement::Container
        && let Ok(addr) = config.server.listen.parse::<std::net::SocketAddr>()
    {
        ports.push((addr.port(), "the server", "--listen 0.0.0.0:<port>"));
    }
    // Caddy's ports matter only when the installation starts (`frood apply`): said, not refused.
    let later: Vec<String> = if config.server.public_url.is_some() {
        [80, 443]
            .into_iter()
            .filter_map(|port| {
                port_taken(port).map(|by| format!("port {port} for HTTPS is taken by {by}"))
            })
            .collect()
    } else {
        Vec::new()
    };
    let taken: Vec<String> = ports
        .iter()
        .filter_map(|(port, what, how)| {
            port_taken(*port)
                .map(|by| format!("port {port} for {what} is taken by {by}: use {how}"))
        })
        .collect();
    if !taken.is_empty() {
        std::fs::remove_dir_all(work.parent().unwrap_or(&work))?;
        bail!("nothing was written:\n  {}", taken.join("\n  "));
    }

    std::fs::write(&config_path, &config_text)
        .with_context(|| format!("cannot write {}", config_path.display()))?;
    std::fs::create_dir_all(secrets_path.parent().context("no secrets folder")?)?;
    write_private(&secrets_path, &secrets)?;
    println!(
        "wrote {} and {}",
        config_path.display(),
        secrets_path.display()
    );

    // The files, where the restored config keeps them.
    if objects.is_dir() {
        let loaded = paths.load()?;
        match frood_api::files_on_host(&loaded.config, &loaded.base_dir) {
            Ok(frood_store::objects::Objects::Disk { root }) => {
                if root.exists() && std::fs::read_dir(&root)?.next().is_some() {
                    bail!("{} is not empty: restore into a new folder", root.display());
                }
                if let Some(parent) = root.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let _ = std::fs::remove_dir(&root);
                std::fs::rename(&objects, &root)
                    .with_context(|| format!("cannot move the files to {}", root.display()))?;
                println!("restored the files to {}", root.display());
            }
            _ => println!(
                "note: the backup has a files folder, but frood.yaml keeps files elsewhere; they are in {}",
                objects.display()
            ),
        }
    }

    // The database: start PostgreSQL alone and load the dump into it while it is empty.
    let loaded = paths.load()?;
    let compose_path = lifecycle::write_deploy(paths, &loaded)?;
    lifecycle::docker_compose(&compose_path, &["up", "-d", "--wait", "postgres"]).await?;
    let pool = frood_store::connect(&loaded.config.storage.url, Duration::from_secs(60))
        .await
        .context("the database did not become reachable")?;
    let (_, existing) = frood_store::pending_migrations(&pool).await?;
    pool.close().await;
    if existing {
        bail!("the database already holds an installation; restore needs an empty one");
    }
    let dump = std::fs::File::open(work.join("db.dump")).context("the backup has no db.dump")?;
    let status = tokio::process::Command::new("docker")
        .args(["compose", "-f"])
        .arg(&compose_path)
        .args([
            "exec",
            "-T",
            "postgres",
            "pg_restore",
            "-U",
            "frood",
            "-d",
            "frood",
            "--no-owner",
            "--exit-on-error",
        ])
        .stdin(dump)
        .status()
        .await
        .context("cannot run pg_restore in the postgres container")?;
    if !status.success() {
        bail!("loading the database failed ({status})");
    }
    std::fs::remove_dir_all(work.parent().unwrap_or(&work))?;
    for note in &later {
        println!("note: {note}; free it, or remove server.public_url, before `frood apply`");
    }
    println!(
        "\nRestored. Check frood.yaml — build paths and module volumes may differ on this machine — then run `frood apply`."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const RESTORED: &str = "server:\n  listen: \"0.0.0.0:8080\"\nstorage:\n  uses: postgres\n  url: postgres://frood:${secret:POSTGRES_PASSWORD}@${env:FROOD_DB_HOST:-127.0.0.1}:${env:FROOD_DB_PORT:-5432}/frood\ndeploy:\n  postgres_port: 5432 # host port\n";

    #[test]
    fn ports_of_this_machine_replace_the_backups() {
        let out = with_ports(RESTORED, Some(54398), Some("0.0.0.0:18098"));
        assert!(out.contains("  listen: \"0.0.0.0:18098\"\n"));
        assert!(out.contains("FROOD_DB_PORT:-54398}/frood"));
        assert!(out.contains("  postgres_port: 54398\n"));
        assert_eq!(with_ports(RESTORED, None, None), RESTORED);
    }

    #[test]
    fn a_held_port_is_seen() {
        let held = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = held.local_addr().unwrap().port();
        assert!(port_taken(port).is_some());
        drop(held);
        assert!(port_taken(port).is_none());
    }
}
