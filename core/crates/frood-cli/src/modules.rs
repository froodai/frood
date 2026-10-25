//! Deploying container modules (docs/16 §2): service tokens in the secrets file, and one env
//! file per module with only what that module may see.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use frood_config::{Loaded, Secrets, ServerPlacement};

use crate::compose;

/// Where a module's env file is written.
pub fn env_file(deploy_dir: &Path, module: &str) -> PathBuf {
    deploy_dir.join(format!("{}.env", frood_modhost::service_name(module)))
}

/// Container modules: those Compose runs (not the ones reached by `url`).
pub fn containers(loaded: &Loaded) -> impl Iterator<Item = (&String, &frood_config::Module)> {
    loaded
        .config
        .modules
        .iter()
        .filter(|(_, m)| m.url.is_none())
}

/// The secret that seals tokens the console stores (docs/26 §4): an age identity.
pub const VAULT_KEY: &str = "FROOD_VAULT_KEY";

pub fn new_vault_key() -> String {
    use age::secrecy::ExposeSecret;
    age::x25519::Identity::generate()
        .to_string()
        .expose_secret()
        .to_owned()
}

/// Adds the vault key to a secrets file that has none (installations made before 0.1.7).
/// True when it added one.
pub fn ensure_vault_key(secrets_path: &Path) -> anyhow::Result<bool> {
    let secrets = Secrets::from_file(secrets_path)?;
    if secrets.get(VAULT_KEY).is_some() {
        return Ok(false);
    }
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(secrets_path)
        .with_context(|| format!("cannot open {}", secrets_path.display()))?;
    writeln!(file, "{VAULT_KEY}={}", new_vault_key())?;
    Ok(true)
}

/// Adds a service token for every module that has none yet. Returns the modules it added.
pub fn ensure_tokens(loaded: &Loaded, secrets_path: &Path) -> anyhow::Result<Vec<String>> {
    let secrets = Secrets::from_file(secrets_path)?;
    let missing: Vec<&String> = loaded
        .config
        .modules
        .keys()
        .filter(|name| secrets.get(&frood_modhost::token_secret(name)).is_none())
        .collect();
    if missing.is_empty() {
        return Ok(Vec::new());
    }
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(secrets_path)
        .with_context(|| format!("cannot open {}", secrets_path.display()))?;
    for name in &missing {
        writeln!(
            file,
            "{}={}{}",
            frood_modhost::token_secret(name),
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        )?;
    }
    Ok(missing.into_iter().cloned().collect())
}

/// Where a module reaches the Frood API from its container.
fn frood_url(loaded: &Loaded) -> String {
    let port = compose::listen_port(loaded);
    match loaded.config.deploy.server {
        ServerPlacement::Container => format!("http://frood:{port}"),
        ServerPlacement::Host => format!("http://host.docker.internal:{port}"),
    }
}

/// The env file of one module: the Frood URL, its name, token and settings, and the secrets it
/// declares — nothing else from the secrets file.
pub fn render_env(loaded: &Loaded, module: &str, secrets: &Secrets) -> anyhow::Result<String> {
    let settings = &loaded.config.modules[module];
    let token = secrets
        .get(&frood_modhost::token_secret(module))
        .with_context(|| format!("module {module} has no service token"))?;
    let mut lines = vec![
        format!("FROOD_URL={}", frood_url(loaded)),
        format!("FROOD_MODULE={module}"),
        format!("FROOD_TOKEN={token}"),
        format!(
            "FROOD_MODULE_CONFIG={}",
            serde_json::to_string(&settings.config)?
        ),
    ];
    let mut missing = Vec::new();
    for name in &settings.secrets {
        match secrets.get(name) {
            Some(value) if value.contains('\n') => {
                bail!("secret {name} of module {module} spans several lines")
            }
            Some(value) => lines.push(format!("{name}={value}")),
            None => missing.push(name.as_str()),
        }
    }
    if !missing.is_empty() {
        bail!(
            "module {module} needs {} in the secrets file",
            missing.join(", ")
        );
    }
    Ok(lines.join("\n") + "\n")
}

pub fn write_env_files(
    loaded: &Loaded,
    secrets_path: &Path,
    deploy_dir: &Path,
) -> anyhow::Result<()> {
    let secrets = Secrets::from_file(secrets_path)?;
    for (name, _) in containers(loaded) {
        let path = env_file(deploy_dir, name);
        write_private(&path, &render_env(loaded, name, &secrets)?)?;
    }
    Ok(())
}

fn write_private(path: &Path, text: &str) -> anyhow::Result<()> {
    std::fs::write(path, text).with_context(|| format!("cannot write {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loaded(extra: &str) -> Loaded {
        let raw = format!(
            "version: 1\ninstallation: {{name: t}}\nstorage: {{uses: postgres, url: 'postgres://x'}}\n\
             deploy: {{server: container, build: .}}\n{extra}"
        );
        frood_config::parse(&raw, &Secrets::from_pairs(Vec::<(String, String)>::new())).unwrap()
    }

    #[test]
    fn a_module_sees_only_its_secrets() {
        let loaded = loaded(
            "modules:\n  telegram: { build: ../modules/telegram, secrets: [TELEGRAM_BOT_TOKEN], config: { greeting: hi } }\n",
        );
        let secrets = Secrets::from_pairs([
            ("FROOD_MODULE_TOKEN_TELEGRAM", "tok"),
            ("TELEGRAM_BOT_TOKEN", "bot"),
            ("POSTGRES_PASSWORD", "pw"),
        ]);
        let env = render_env(&loaded, "telegram", &secrets).unwrap();
        assert!(env.contains("FROOD_URL=http://frood:8080\n"));
        assert!(env.contains("FROOD_TOKEN=tok\n"));
        assert!(env.contains("TELEGRAM_BOT_TOKEN=bot\n"));
        assert!(env.contains("FROOD_MODULE_CONFIG={\"greeting\":\"hi\"}\n"));
        assert!(!env.contains("pw"), "{env}");

        let err = render_env(
            &loaded,
            "telegram",
            &Secrets::from_pairs([("FROOD_MODULE_TOKEN_TELEGRAM", "t")]),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("TELEGRAM_BOT_TOKEN"), "{err}");
    }

    #[test]
    fn tokens_are_added_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.env");
        std::fs::write(&path, "POSTGRES_PASSWORD=pw\n").unwrap();
        let loaded =
            loaded("modules:\n  stt: { build: ../stt }\n  dev: { url: 'http://127.0.0.1:9' }\n");
        assert_eq!(ensure_tokens(&loaded, &path).unwrap(), ["dev", "stt"]);
        assert!(ensure_tokens(&loaded, &path).unwrap().is_empty());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("FROOD_MODULE_TOKEN_STT="));
        assert_eq!(containers(&loaded).count(), 1);
    }
}
