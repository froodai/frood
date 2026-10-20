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

    // restic from the backup module's image; a repository folder on this machine is mounted.
    let mut command = tokio::process::Command::new("docker");
    command.args(["run", "--rm"]);
    #[cfg(unix)]
    {
        let uid = std::process::Command::new("id").arg("-u").output()?;
        let gid = std::process::Command::new("id").arg("-g").output()?;
        command.args([
            "--user",
            &format!(
                "{}:{}",
                String::from_utf8_lossy(&uid.stdout).trim(),
                String::from_utf8_lossy(&gid.stdout).trim()
            ),
        ]);
    }
    let local = Path::new(&options.repository);
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
        .args(["--entrypoint", "restic", &options.image])
        // Read only: no lock in the repository, which may be mounted read-only.
        .args([
            "--no-cache",
            "--no-lock",
            "restore",
            &format!("{snapshot}:/backup"),
        ])
        .args(["--tag", "frood", "--target", "/restore"]);
    println!("restoring {snapshot} from {}…", options.repository);
    let status = command
        .status()
        .await
        .context("cannot run docker; is it installed?")?;
    if !status.success() {
        bail!("restic could not restore from the repository ({status})");
    }

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
    std::fs::copy(work.join("frood.yaml"), &config_path).context("the backup has no frood.yaml")?;
    std::fs::create_dir_all(secrets_path.parent().context("no secrets folder")?)?;
    write_private(&secrets_path, &secrets)?;
    println!(
        "wrote {} and {}",
        config_path.display(),
        secrets_path.display()
    );

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
    std::fs::remove_dir_all(&work)?;
    println!(
        "\nRestored. Check frood.yaml — build paths and module volumes may differ on this machine — then run `frood apply`."
    );
    Ok(())
}
