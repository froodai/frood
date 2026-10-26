//! Sign-in with a button in the person's linked Telegram (docs/28 §2). The page asks for the
//! address; Frood checks that it may sign in and has the channel linked, and keeps an ask. The
//! server sends the button through the channel module, which only shows it and passes the
//! press on. The browser that asked waits for the press and then gets the session.

use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::secret::hash;
use crate::{Auth, AuthError, Result};

/// How long the button works.
pub const ASK_MINUTES: i64 = 5;
/// A fuse against someone who knows the address and keeps pressing "send".
const ASKS_PER_HOUR: i64 = 5;

/// A button to send: to whom, where, and what the page shows to compare.
#[derive(Debug, Clone, Serialize)]
pub struct SignInAsk {
    pub id: Uuid,
    pub user_id: Uuid,
    pub channel: String,
    /// The person's account in the channel.
    pub external_id: String,
    pub code: String,
    /// The app the person signs in to, as it registered.
    pub client: String,
    pub expires_at: DateTime<Utc>,
}

/// Where an ask stands, as the waiting page sees it.
pub(crate) enum Asked {
    Waiting { code: String },
    Approved { id: Uuid, user_id: Uuid },
    Denied,
    Expired,
    Used,
}

impl Auth {
    /// Checks that `email` may sign in through `channel` and keeps an ask for the sign-in
    /// request `tx`. A new ask replaces the person's earlier one that still waits.
    pub async fn ask_sign_in(&self, tx: &str, email: &str, channel: &str) -> Result<SignInAsk> {
        let email = email.trim().to_lowercase();
        // Only the listed addresses, not whole domains, and nobody new.
        if !self.settings().allow.contains(&email) {
            return Err(AuthError::Invalid(
                "This address may not sign in here.".into(),
            ));
        }
        let not_linked = || {
            AuthError::Invalid(format!(
                "No {channel} account is linked to this address. Link it first: in the console, \
                 under My settings, or by sending the link code to the bot."
            ))
        };
        let user_id = match self.user_id_by_email(&email, false).await {
            Ok(id) => id,
            Err(AuthError::NotFound(_)) => return Err(not_linked()),
            Err(err) => return Err(err),
        };
        let external_id = self
            .links_of(user_id)
            .await?
            .into_iter()
            .find(|l| l.channel == channel)
            .map(|l| l.external_id)
            .ok_or_else(not_linked)?;
        let recent: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM sign_in_asks WHERE user_id = $1 AND created_at > now() - interval '1 hour'",
        )
        .bind(user_id)
        .fetch_one(self.pool())
        .await?;
        if recent >= ASKS_PER_HOUR {
            return Err(AuthError::Invalid(
                "Too many sign-in requests in the last hour. Try again later.".into(),
            ));
        }
        let client: Option<String> = sqlx::query_scalar(
            "SELECT coalesce(client_name, client_id) FROM auth_requests WHERE id_hash = $1",
        )
        .bind(hash(tx))
        .fetch_optional(self.pool())
        .await?;
        sqlx::query(
            "UPDATE sign_in_asks SET state = 'expired' WHERE user_id = $1 AND state = 'asked'",
        )
        .bind(user_id)
        .execute(self.pool())
        .await?;
        let id = Uuid::now_v7();
        let code = code();
        let expires_at: DateTime<Utc> = sqlx::query_scalar(
            "INSERT INTO sign_in_asks (id, tx_hash, user_id, channel, code, expires_at) \
             VALUES ($1, $2, $3, $4, $5, now() + make_interval(mins => $6)) RETURNING expires_at",
        )
        .bind(id)
        .bind(hash(tx))
        .bind(user_id)
        .bind(channel)
        .bind(&code)
        .bind(ASK_MINUTES as i32)
        .fetch_one(self.pool())
        .await?;
        Ok(SignInAsk {
            id,
            user_id,
            channel: channel.to_owned(),
            external_id,
            code,
            client: client.unwrap_or_else(|| "Frood".to_owned()),
            expires_at,
        })
    }

    /// The button could not be sent: the ask is over.
    pub async fn drop_sign_in_ask(&self, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE sign_in_asks SET state = 'expired' WHERE id = $1 AND state = 'asked'")
            .bind(id)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// The person pressed "Sign in" or "Not me". Only the person asked can answer, and only
    /// while the button works.
    pub async fn decide_sign_in(&self, id: Uuid, user_id: Uuid, approve: bool) -> Result<()> {
        let decided = sqlx::query(
            "UPDATE sign_in_asks SET state = $3 \
             WHERE id = $1 AND user_id = $2 AND state = 'asked' AND expires_at > now()",
        )
        .bind(id)
        .bind(user_id)
        .bind(if approve { "approved" } else { "denied" })
        .execute(self.pool())
        .await?;
        if decided.rows_affected() == 0 {
            return Err(AuthError::Invalid(
                "This sign-in request has expired or was already answered.".into(),
            ));
        }
        Ok(())
    }

    /// The latest ask of the sign-in request `tx`, if any.
    pub(crate) async fn asked(&self, tx: &str) -> Result<Option<Asked>> {
        let row: Option<(Uuid, Uuid, String, String, bool)> = sqlx::query_as(
            "SELECT id, user_id, code, state, expires_at <= now() FROM sign_in_asks \
             WHERE tx_hash = $1 ORDER BY created_at DESC LIMIT 1",
        )
        .bind(hash(tx))
        .fetch_optional(self.pool())
        .await?;
        let Some((id, user_id, code, state, expired)) = row else {
            return Ok(None);
        };
        Ok(Some(match state.as_str() {
            "asked" if expired => {
                self.drop_sign_in_ask(id).await?;
                Asked::Expired
            }
            "asked" => Asked::Waiting { code },
            "approved" => Asked::Approved { id, user_id },
            "denied" => Asked::Denied,
            "used" => Asked::Used,
            _ => Asked::Expired,
        }))
    }

    /// An approved ask gives one session; true when this call took it.
    pub(crate) async fn use_sign_in(&self, id: Uuid) -> Result<bool> {
        let used = sqlx::query(
            "UPDATE sign_in_asks SET state = 'used' WHERE id = $1 AND state = 'approved'",
        )
        .bind(id)
        .execute(self.pool())
        .await?;
        Ok(used.rows_affected() == 1)
    }
}

/// Four digits, to compare the page with the message.
fn code() -> String {
    let mut bytes = [0u8; 2];
    getrandom::fill(&mut bytes).expect("system randomness");
    format!("{:04}", u16::from_le_bytes(bytes) % 10_000)
}

#[cfg(test)]
mod tests {
    #[test]
    fn codes_are_four_digits() {
        for _ in 0..100 {
            let code = super::code();
            assert_eq!(code.len(), 4);
            assert!(code.chars().all(|c| c.is_ascii_digit()));
        }
    }
}
