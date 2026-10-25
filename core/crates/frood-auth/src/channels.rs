//! Channel accounts (Telegram, …) linked to people by one-time codes (docs/15 §4): a person
//! asks for a code in the web or CLI and sends it to the bot; from then on the bot's
//! messages act as that person.

use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::secret::hash;
use crate::{Auth, AuthError, Result};

/// Letters and digits that cannot be confused when typed from a screen.
const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
const CODE_LENGTH: usize = 8;
const CODE_MINUTES: i32 = 15;

#[derive(Debug, Clone, Serialize)]
pub struct LinkCode {
    pub code: String,
    pub channel: String,
    pub expires_at: DateTime<Utc>,
}

/// A person's account in a channel, as they see it.
#[derive(Debug, Clone, Serialize)]
pub struct ChannelLink {
    pub channel: String,
    pub external_id: String,
    /// E.g. the Telegram username.
    pub label: Option<String>,
    pub linked_at: DateTime<Utc>,
}

fn new_code() -> String {
    let mut bytes = [0u8; CODE_LENGTH];
    getrandom::fill(&mut bytes).expect("system randomness");
    // 256 is not a multiple of the alphabet size; the bias is negligible for a 15-minute code.
    bytes
        .iter()
        .map(|b| CODE_ALPHABET[usize::from(*b) % CODE_ALPHABET.len()] as char)
        .collect()
}

fn normalize(code: &str) -> String {
    code.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

impl Auth {
    /// A one-time code that links an account of `channel` to `user_id`.
    pub async fn create_link_code(&self, user_id: Uuid, channel: &str) -> Result<LinkCode> {
        let code = new_code();
        let expires_at: DateTime<Utc> = sqlx::query_scalar(
            "INSERT INTO link_codes (code_hash, channel, user_id, expires_at) \
             VALUES ($1, $2, $3, now() + make_interval(mins => $4)) RETURNING expires_at",
        )
        .bind(hash(&code))
        .bind(channel)
        .bind(user_id)
        .bind(CODE_MINUTES)
        .fetch_one(self.pool())
        .await?;
        sqlx::query("DELETE FROM link_codes WHERE expires_at < now() - interval '1 day'")
            .execute(self.pool())
            .await?;
        Ok(LinkCode {
            code,
            channel: channel.to_owned(),
            expires_at,
        })
    }

    /// Redeems a code sent to the channel by `external_id`; returns the linked user. A
    /// channel account linked before moves to the new user.
    pub async fn redeem_link_code(
        &self,
        channel: &str,
        code: &str,
        external_id: &str,
        label: Option<&str>,
    ) -> Result<Uuid> {
        let mut tx = self.pool().begin().await?;
        let user_id: Uuid = sqlx::query_scalar(
            "UPDATE link_codes SET used_at = now() \
             WHERE code_hash = $1 AND channel = $2 AND used_at IS NULL AND expires_at > now() \
             RETURNING user_id",
        )
        .bind(hash(&normalize(code)))
        .bind(channel)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| AuthError::Invalid("the code is unknown, used or expired".into()))?;
        sqlx::query(
            "INSERT INTO channel_links (channel, external_id, user_id, label) VALUES ($1, $2, $3, $4) \
             ON CONFLICT (channel, external_id) DO UPDATE SET user_id = EXCLUDED.user_id, \
             label = EXCLUDED.label, created_at = now()",
        )
        .bind(channel)
        .bind(external_id)
        .bind(user_id)
        .bind(label)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(user_id)
    }

    pub async fn linked_user(&self, channel: &str, external_id: &str) -> Result<Option<Uuid>> {
        Ok(sqlx::query_scalar(
            "SELECT user_id FROM channel_links WHERE channel = $1 AND external_id = $2",
        )
        .bind(channel)
        .bind(external_id)
        .fetch_optional(self.pool())
        .await?)
    }

    /// The person's linked channel accounts, oldest first.
    pub async fn links_of(&self, user_id: Uuid) -> Result<Vec<ChannelLink>> {
        let rows: Vec<(String, String, Option<String>, DateTime<Utc>)> = sqlx::query_as(
            "SELECT channel, external_id, label, created_at FROM channel_links \
             WHERE user_id = $1 ORDER BY created_at",
        )
        .bind(user_id)
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(|(channel, external_id, label, linked_at)| ChannelLink {
                channel,
                external_id,
                label,
                linked_at,
            })
            .collect())
    }

    /// The person unlinks one of their own channel accounts.
    pub async fn unlink_own(
        &self,
        user_id: Uuid,
        channel: &str,
        external_id: &str,
    ) -> Result<bool> {
        let done = sqlx::query(
            "DELETE FROM channel_links WHERE channel = $1 AND external_id = $2 AND user_id = $3",
        )
        .bind(channel)
        .bind(external_id)
        .bind(user_id)
        .execute(self.pool())
        .await?;
        Ok(done.rows_affected() > 0)
    }

    pub async fn unlink(&self, channel: &str, external_id: &str) -> Result<bool> {
        let done = sqlx::query("DELETE FROM channel_links WHERE channel = $1 AND external_id = $2")
            .bind(channel)
            .bind(external_id)
            .execute(self.pool())
            .await?;
        Ok(done.rows_affected() > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_unambiguous_and_typed_loosely() {
        let code = new_code();
        assert_eq!(code.len(), CODE_LENGTH);
        assert!(code.bytes().all(|b| CODE_ALPHABET.contains(&b)));
        assert_eq!(normalize(" k7m2-qx9p "), "K7M2QX9P");
    }
}
