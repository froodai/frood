//! `frood token …`: personal tokens for scripts and bots, created by the operator directly in
//! the database (the first token of an installation has to come from somewhere).

use std::time::Duration;

use anyhow::Context;
use frood_auth::{Auth, AuthSettings, scope};
use uuid::Uuid;

use crate::lifecycle::Paths;

pub(crate) async fn auth(paths: &Paths) -> anyhow::Result<Auth> {
    let loaded = paths.load()?;
    let pool = frood_store::connect(&loaded.config.storage.url, Duration::from_secs(5))
        .await
        .context("the database is not reachable; run `frood apply` first")?;
    Ok(Auth::new(pool, AuthSettings::from_config(&loaded.config)))
}

pub async fn create(
    paths: &Paths,
    email: &str,
    label: &str,
    scopes: &[String],
    expires_in_days: Option<u32>,
) -> anyhow::Result<()> {
    let auth = auth(paths).await?;
    if !auth.is_allowed(email) {
        eprintln!(
            "note: {email} is not in auth.allow / auth.allow_domains, so it cannot sign in through a browser; the token works regardless"
        );
    }
    let user = auth.user_id_by_email(email, true).await?;
    let (id, token) = auth
        .create_personal_token(user, label, scopes, expires_in_days)
        .await?;
    eprintln!("created token {id} for {email} with {}", scopes.join(" "));
    eprintln!("it is shown once; the server keeps only its hash");
    println!("{token}");
    Ok(())
}

/// A one-time link that adds a passkey for a person in auth.allow (docs/28 §2).
pub async fn passkey_link(paths: &Paths, email: Option<&str>) -> anyhow::Result<()> {
    let auth = auth(paths).await?;
    let email = match (email, auth.settings().allow.as_slice()) {
        (Some(email), _) => email.trim().to_lowercase(),
        (None, [only]) => only.clone(),
        _ => anyhow::bail!("whose passkey? --email you@example.com"),
    };
    let user = auth.user_id_by_email(&email, true).await?;
    let (url, _) = auth.passkey_link(user).await?;
    eprintln!(
        "open it within {} minutes on the device that will sign in as {email}",
        frood_auth::PASSKEY_LINK_MINUTES
    );
    println!("{url}");
    Ok(())
}

pub async fn list(paths: &Paths, email: &str) -> anyhow::Result<()> {
    let auth = auth(paths).await?;
    let user = auth.user_id_by_email(email, false).await?;
    for token in auth.personal_tokens(user).await? {
        let expires = token
            .expires_at
            .map_or_else(|| "never".to_owned(), |at| at.to_rfc3339());
        let used = token
            .last_used_at
            .map_or_else(|| "never".to_owned(), |at| at.to_rfc3339());
        println!(
            "{}  {}  [{}]  expires: {expires}  last used: {used}",
            token.id,
            token.label.as_deref().unwrap_or("-"),
            token.scopes.join(" "),
        );
    }
    Ok(())
}

pub async fn revoke(paths: &Paths, email: &str, id: Uuid) -> anyhow::Result<()> {
    let auth = auth(paths).await?;
    let user = auth.user_id_by_email(email, false).await?;
    auth.revoke_token(user, id).await?;
    println!("revoked {id}");
    Ok(())
}

/// A one-time code that links the person's account in `channel` (e.g. Telegram).
pub async fn link(paths: &Paths, channel: &str, email: &str) -> anyhow::Result<()> {
    let auth = auth(paths).await?;
    let loaded = paths.load()?;
    if !loaded.config.modules.contains_key(channel) {
        eprintln!(
            "note: there is no module {channel:?} in modules; the code works once one is added"
        );
    }
    let user = auth.user_id_by_email(email, true).await?;
    let code = auth.create_link_code(user, channel).await?;
    eprintln!(
        "send this to the {channel} bot within 15 minutes (until {}):",
        code.expires_at.format("%H:%M UTC")
    );
    println!("/link {}", code.code);
    Ok(())
}

/// Default scopes of a token made on the command line: everything but `admin`.
pub fn default_scopes() -> Vec<String> {
    scope::ALL
        .iter()
        .filter(|s| **s != scope::ADMIN)
        .map(|s| (*s).to_owned())
        .collect()
}
