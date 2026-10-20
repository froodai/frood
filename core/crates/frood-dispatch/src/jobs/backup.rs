//! `backup` and `backup-verify` (docs/16 §7, C4): through the module that gives
//! `backup.target@1`, a consistent dump of the database with `frood.yaml` and the secrets file —
//! encrypted here, to the recovery key, so the module and the storages never see it in the
//! clear — into every target; and a restore of each target's latest backup into a temporary
//! database, compared with what was backed up.

use std::path::PathBuf;
use std::time::Duration;

use base64::Engine;
use serde_json::{Value, json};

use super::JobError;
use crate::Ctx;

/// A backup or a check may take long on a big database or a slow storage.
const CALL_LIMIT: Duration = Duration::from_secs(6 * 3600);

/// What backups need from the installation.
#[derive(Debug, Clone)]
pub struct BackupSetup {
    pub settings: frood_config::Backup,
    pub installation: String,
    /// The database as the backup module reaches it.
    pub database_url: String,
    /// `frood.yaml` as written, and where the secrets file is; the server sets them at start.
    pub config_yaml: Option<String>,
    pub secrets_path: Option<PathBuf>,
}

fn file(name: &str, data: &[u8]) -> Value {
    json!({ "name": name, "data_base64": base64::engine::general_purpose::STANDARD.encode(data) })
}

/// The secrets file, encrypted to the installation's recovery key.
fn encrypt_secrets(recovery_key: &str, plain: &[u8]) -> Result<Vec<u8>, JobError> {
    let recipient: age::x25519::Recipient = recovery_key.trim().parse().map_err(|_| {
        JobError::permanent("backup.recovery_key is not the public half of an age key")
    })?;
    age::encrypt(&recipient, plain)
        .map_err(|e| JobError::permanent(format!("cannot encrypt the secrets: {e}")))
}

pub(crate) async fn run(ctx: &Ctx, verify: bool) -> Result<Value, JobError> {
    let setup = ctx.backup.as_ref().as_ref().ok_or_else(|| {
        JobError::permanent("backups are not configured: frood.yaml has no `backup` section")
    })?;
    let settings = &setup.settings;
    let module = ctx
        .modules
        .provider("backup.target", settings.module.as_deref())?;
    let targets: Vec<Value> = settings
        .targets
        .iter()
        .map(|(name, t)| {
            json!({ "name": name, "repository": t.repository, "password": t.password, "env": t.env })
        })
        .collect();
    let mut input = json!({
        "op": if verify { "verify" } else { "backup" },
        "installation": setup.installation,
        "database": { "url": setup.database_url },
        "targets": targets,
    });
    let mut warnings = Vec::new();
    if !verify {
        let mut files = Vec::new();
        match &setup.config_yaml {
            Some(yaml) => files.push(file("frood.yaml", yaml.as_bytes())),
            None => warnings.push("frood.yaml is not in the backup: the server was not given it"),
        }
        let secrets = match &setup.secrets_path {
            Some(path) => tokio::fs::read(path).await.ok(),
            None => None,
        };
        match secrets {
            Some(plain) => files.push(file(
                "secrets.env.age",
                &encrypt_secrets(&settings.recovery_key, &plain)?,
            )),
            None => warnings.push(
                "the secrets file is not in the backup: the server cannot read it; keep a copy yourself",
            ),
        }
        let schema: Option<i64> =
            sqlx::query_scalar("SELECT max(version) FROM _sqlx_migrations WHERE success")
                .fetch_one(&ctx.pool)
                .await
                .map_err(|e| JobError::transient(format!("database: {e}")))?;
        input["files"] = json!(files);
        input["keep"] = json!(
            settings
                .keep
                .rules()
                .into_iter()
                .collect::<std::collections::BTreeMap<_, _>>()
        );
        input["about"] = json!({
            "frood_version": env!("CARGO_PKG_VERSION"),
            "schema_version": schema,
        });
    }
    let mut out = ctx
        .modules
        .call_within(&module, "backup.target", &input, Some(CALL_LIMIT))
        .await?;
    let failed: Vec<String> = out["targets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|t| t["ok"] != true)
        .map(|t| {
            format!(
                "{}: {}",
                t["name"].as_str().unwrap_or("?"),
                t["error"].as_str().unwrap_or("no details")
            )
        })
        .collect();
    if !failed.is_empty() {
        let what = if verify {
            "Restoring from a backup failed"
        } else {
            "The backup failed"
        };
        return Err(JobError::permanent(format!(
            "{what} in {}",
            failed.join("; ")
        )));
    }
    if let Some(map) = out.as_object_mut() {
        map.insert("module".into(), json!(module));
        if !warnings.is_empty() {
            map.insert("warnings".into(), json!(warnings));
        }
    }
    for warning in &warnings {
        tracing::warn!("{warning}");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_readable_with_the_recovery_key_only() {
        let identity = age::x25519::Identity::generate();
        let public = identity.to_public().to_string();
        let sealed = encrypt_secrets(&public, b"POSTGRES_PASSWORD=pw\n").unwrap();
        assert!(!sealed.windows(8).any(|w| w == b"POSTGRES"));
        assert_eq!(
            age::decrypt(&identity, &sealed).unwrap(),
            b"POSTGRES_PASSWORD=pw\n"
        );
        let other = age::x25519::Identity::generate();
        assert!(age::decrypt(&other, &sealed).is_err());
        assert!(encrypt_secrets("not-a-key", b"x").is_err());
    }
}
