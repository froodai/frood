use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use frood_config::{ServerPlacement, TemplateOptions};

/// Image used when the config does not build from source.
pub const DEFAULT_IMAGE: &str = concat!("ghcr.io/froodai/frood:", env!("CARGO_PKG_VERSION"));

pub struct Options {
    pub dir: PathBuf,
    pub name: String,
    pub server: ServerPlacement,
    pub build: Option<String>,
    pub postgres_port: u16,
    pub force: bool,
}

pub fn run(options: &Options) -> anyhow::Result<()> {
    std::fs::create_dir_all(&options.dir)
        .with_context(|| format!("cannot create {}", options.dir.display()))?;
    let config_path = options.dir.join("frood.yaml");
    if config_path.exists() && !options.force {
        bail!(
            "{} already exists; pass --force to overwrite it",
            config_path.display()
        );
    }

    let text = frood_config::template(&TemplateOptions {
        name: &options.name,
        server: options.server,
        build: options.build.as_deref(),
        image: DEFAULT_IMAGE,
        postgres_port: options.postgres_port,
    });
    std::fs::write(&config_path, text)
        .with_context(|| format!("cannot write {}", config_path.display()))?;

    let secrets_path = frood_config::default_secrets_path(&config_path);
    let created_secrets = write_secrets_if_missing(&secrets_path)?;
    ensure_gitignored(&options.dir)?;

    // Fail early if the template and the options do not form a valid config.
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
    println!("next: frood --config {} apply", config_path.display());
    Ok(())
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
        "# Secrets for this installation. Never commit this file.\nPOSTGRES_PASSWORD={password}\n"
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
        }
    }

    #[test]
    fn creates_config_secrets_and_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        run(&options(dir.path())).unwrap();
        let loaded = frood_config::load(&dir.path().join("frood.yaml"), None).unwrap();
        assert_eq!(loaded.config.installation.name, "personal");
        let gitignore = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(gitignore.contains(".frood/"));
    }

    #[test]
    fn refuses_to_overwrite_and_keeps_secrets_on_force() {
        let dir = tempfile::tempdir().unwrap();
        run(&options(dir.path())).unwrap();
        let secrets_path = dir.path().join(".frood/secrets.env");
        let secrets = std::fs::read_to_string(&secrets_path).unwrap();

        assert!(run(&options(dir.path())).is_err());

        let mut forced = options(dir.path());
        forced.force = true;
        run(&forced).unwrap();
        assert_eq!(std::fs::read_to_string(&secrets_path).unwrap(), secrets);
    }

    #[cfg(unix)]
    #[test]
    fn secrets_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        run(&options(dir.path())).unwrap();
        let mode = std::fs::metadata(dir.path().join(".frood/secrets.env"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
