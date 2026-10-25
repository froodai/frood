use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use frood_config::{ServerPlacement, TemplateOptions};

use crate::lifecycle::{self, Paths};
use crate::wizard::{self, Answers};

/// Image used when the config does not build from source.
pub const DEFAULT_IMAGE: &str = concat!("ghcr.io/froodai/frood:", env!("CARGO_PKG_VERSION"));

pub struct Options {
    pub dir: PathBuf,
    pub name: String,
    pub server: ServerPlacement,
    pub build: Option<String>,
    pub postgres_port: u16,
    pub force: bool,
    pub email: Option<String>,
    pub public_url: Option<String>,
    pub timezone: Option<String>,
    /// Ask nothing.
    pub yes: bool,
}

pub async fn run(options: &Options) -> anyhow::Result<()> {
    std::fs::create_dir_all(&options.dir)
        .with_context(|| format!("cannot create {}", options.dir.display()))?;
    let config_path = options.dir.join("frood.yaml");
    if config_path.exists() && !options.force {
        bail!(
            "{} already exists; pass --force to overwrite it",
            config_path.display()
        );
    }

    let interactive = wizard::interactive(options.yes);
    let mut answers = Answers::defaults(
        options.email.clone(),
        options.public_url.clone(),
        options.timezone.clone(),
    );
    if interactive {
        answers.email = answers.email.or_else(wizard::git_email);
        answers = wizard::ask(answers).await?;
    }
    let recovery = match &answers.backups {
        Some(_) => Some(recovery_key(&options.dir, &options.name)?),
        None => None,
    };

    let template = frood_config::template(&TemplateOptions {
        name: &options.name,
        server: options.server,
        build: options.build.as_deref(),
        image: DEFAULT_IMAGE,
        postgres_port: options.postgres_port,
    });
    let text = wizard::config_text(
        &template,
        &options.name,
        &answers,
        recovery.as_ref().map(|(public, _)| public.as_str()),
    );
    std::fs::write(&config_path, text)
        .with_context(|| format!("cannot write {}", config_path.display()))?;

    let secrets_path = frood_config::default_secrets_path(&config_path);
    let created_secrets = write_secrets_if_missing(&secrets_path)?;
    add_secrets(&secrets_path, &wizard::secrets(&answers))?;
    ensure_gitignored(&options.dir)?;

    // Fail early if the template and the answers do not form a valid config.
    frood_config::load(&config_path, Some(&secrets_path)).context("generated config is invalid")?;

    println!("created {}", config_path.display());
    if created_secrets {
        println!(
            "created {} (keep it private; it is git-ignored)",
            secrets_path.display()
        );
    } else {
        println!("kept existing {}", secrets_path.display());
    }
    if let Some((_, file)) = &recovery {
        println!(
            "wrote the recovery key to {}: keep it off this computer (a password manager, paper); restoring a backup needs it",
            file.display()
        );
    }
    let paths = Paths::new(config_path.clone(), None);
    if interactive && wizard_confirm_start()? {
        lifecycle::apply(&paths, false).await?;
        welcome(&paths, &answers).await?;
    } else {
        println!("next: frood --config {} apply", config_path.display());
    }
    Ok(())
}

fn wizard_confirm_start() -> anyhow::Result<bool> {
    use std::io::Write;
    print!("\nStart Frood now? [Y/n]: ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(!line.trim().to_lowercase().starts_with('n'))
}

/// A recovery key for the backups: its secret half in a private file next to the person's
/// home, not in the installation's folder.
fn recovery_key(dir: &Path, name: &str) -> anyhow::Result<(String, PathBuf)> {
    use age::secrecy::ExposeSecret;
    let identity = age::x25519::Identity::generate();
    let public = identity.to_public().to_string();
    let home = std::env::var("HOME").map_or_else(|_| dir.to_owned(), PathBuf::from);
    let mut file = home.join(format!("frood-{name}-recovery-key.txt"));
    let mut n = 2;
    while file.exists() {
        file = home.join(format!("frood-{name}-recovery-key-{n}.txt"));
        n += 1;
    }
    let text = format!(
        "# Frood recovery key for {name}, made {}.\n# Keep it off this computer: restoring a backup needs it.\n# public key: {public}\n{}\n",
        chrono::Utc::now().format("%Y-%m-%d"),
        identity.to_string().expose_secret()
    );
    crate::recovery::write_private(&file, text.as_bytes())?;
    Ok((public, file))
}

/// What to do next, with what was just made: the console, the clients, the bot.
async fn welcome(paths: &Paths, answers: &Answers) -> anyhow::Result<()> {
    let loaded = paths.load()?;
    let base = match &loaded.config.server.public_url {
        Some(url) => url.trim_end_matches('/').to_owned(),
        None => {
            let port = loaded
                .config
                .server
                .listen
                .rsplit(':')
                .next()
                .unwrap_or("8080")
                .to_owned();
            format!("http://127.0.0.1:{port}")
        }
    };
    println!("\nFrood is running.\n");
    println!("Console:      {base}/admin/");
    println!("Claude Code:  claude mcp add --transport http frood {base}/mcp");
    println!("              then in Claude Code: /mcp → frood → Authenticate");
    let Some(email) = &answers.email else {
        return Ok(());
    };
    let auth = crate::token::auth(paths).await?;
    let user = auth.user_id_by_email(email, true).await?;
    let scopes = crate::token::default_scopes();
    let (_, token) = auth
        .create_personal_token(user, "codex", &scopes, None)
        .await?;
    println!("Codex:        add to ~/.codex/config.toml");
    println!("                [mcp_servers.frood]");
    println!("                url = \"{base}/mcp\"");
    println!("                bearer_token_env_var = \"FROOD_TOKEN\"");
    println!("              and set FROOD_TOKEN={token}");
    println!("              (shown once; `frood token` makes more)");
    if let Some((_, bot)) = &answers.telegram {
        let code = auth.create_link_code(user, "telegram").await?;
        println!(
            "Telegram:     open https://t.me/{bot}?start={} within 15 minutes",
            code.code
        );
    }
    if answers.model.is_none() {
        println!(
            "\nNotes are kept but not read into knowledge until a model is named: add a key in the console or in frood.yaml."
        );
    }
    Ok(())
}

/// Adds `KEY=value` lines the file does not have yet.
fn add_secrets(path: &Path, pairs: &[(String, String)]) -> anyhow::Result<()> {
    if pairs.is_empty() {
        return Ok(());
    }
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let mut text = existing.clone();
    for (key, value) in pairs {
        if existing.lines().any(|l| l.starts_with(&format!("{key}="))) {
            continue;
        }
        if !text.ends_with('\n') && !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&format!("{key}={value}\n"));
    }
    std::fs::write(path, text).with_context(|| format!("cannot write {}", path.display()))
}

fn write_secrets_if_missing(path: &Path) -> anyhow::Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    let password = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let text = format!(
        "# Secrets for this installation. Never commit this file.\nPOSTGRES_PASSWORD={password}\n{}={}\n",
        crate::modules::VAULT_KEY,
        crate::modules::new_vault_key()
    );
    std::fs::write(path, text).with_context(|| format!("cannot write {}", path.display()))?;
    restrict_permissions(path)?;
    Ok(true)
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("cannot restrict permissions of {}", path.display()))
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

fn ensure_gitignored(dir: &Path) -> anyhow::Result<()> {
    let path = dir.join(".gitignore");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    if existing
        .lines()
        .any(|line| line.trim() == ".frood/" || line.trim() == ".frood")
    {
        return Ok(());
    }
    let separator = if existing.is_empty() || existing.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    std::fs::write(&path, format!("{existing}{separator}.frood/\n"))
        .with_context(|| format!("cannot write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(dir: &Path) -> Options {
        Options {
            dir: dir.to_owned(),
            name: "personal".to_owned(),
            server: ServerPlacement::Host,
            build: None,
            postgres_port: 5432,
            force: false,
            email: None,
            public_url: None,
            timezone: None,
            yes: true,
        }
    }

    #[tokio::test]
    async fn creates_config_secrets_and_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        run(&options(dir.path())).await.unwrap();
        let loaded = frood_config::load(&dir.path().join("frood.yaml"), None).unwrap();
        assert_eq!(loaded.config.installation.name, "personal");
        let gitignore = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(gitignore.contains(".frood/"));
    }

    #[tokio::test]
    async fn refuses_to_overwrite_and_keeps_secrets_on_force() {
        let dir = tempfile::tempdir().unwrap();
        run(&options(dir.path())).await.unwrap();
        let secrets_path = dir.path().join(".frood/secrets.env");
        let secrets = std::fs::read_to_string(&secrets_path).unwrap();

        assert!(run(&options(dir.path())).await.is_err());

        let mut forced = options(dir.path());
        forced.force = true;
        run(&forced).await.unwrap();
        assert_eq!(std::fs::read_to_string(&secrets_path).unwrap(), secrets);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn secrets_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        run(&options(dir.path())).await.unwrap();
        let mode = std::fs::metadata(dir.path().join(".frood/secrets.env"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
