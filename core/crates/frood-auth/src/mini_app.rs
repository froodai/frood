//! Sign-in from the Telegram Mini App (docs/32 M2). Telegram signs the launch data
//! (`initData`) with its own Ed25519 key over the bot's id, so the core checks it without the
//! bot's token, which stays in the module. The person is whoever the Telegram account is
//! linked to. The session is short, has no refresh token and cannot administer the
//! installation (QM2): read, write and decide only.

use std::collections::HashSet;

use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, routing::post};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::secret::{hash, new_secret};
use crate::{Auth, AuthError, Resource, Result, scope};

/// Telegram's key for third-party validation of Mini App launch data (production).
pub const TELEGRAM_PUBLIC_KEY: &str =
    "e7bf03a2fa4602af4580703d88dda5bb59f32ed8b02a56c187fe7d34caed242d";
/// The client the Mini App's tokens carry, for the audit (`user:<id>/via/frood-telegram`).
pub const MINI_APP_CLIENT: &str = "frood-telegram";
/// What a Mini App session may do: everything for one's own knowledge, nothing of the
/// installation's settings.
pub const MINI_APP_SCOPES: [&str; 4] = [scope::READ, scope::CAPTURE, scope::PROPOSE, scope::DECIDE];
/// Launch data older than this is not accepted; the Mini App is opened again from the chat.
const MAX_AGE_SECS: i64 = 24 * 3600;

/// A Mini App session as issued.
#[derive(Debug, Clone, Serialize)]
pub struct MiniAppSession {
    pub access_token: String,
    pub token_type: &'static str,
    pub expires_in: u64,
    pub scope: String,
}

/// What the checked launch data says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Launch {
    pub telegram_id: i64,
}

/// Checks the launch data of a Mini App opened from the bot `bot_id`, at `now` (Unix seconds).
pub(crate) fn verify(
    init_data: &str,
    bot_id: i64,
    key: &[u8; 32],
    now: i64,
) -> std::result::Result<Launch, &'static str> {
    let mut seen = HashSet::new();
    let mut signature = None;
    let mut pairs = Vec::new();
    for (k, v) in url::form_urlencoded::parse(init_data.as_bytes()) {
        if !seen.insert(k.clone()) {
            return Err("a field of the launch data repeats");
        }
        match k.as_ref() {
            "signature" => signature = Some(v.into_owned()),
            // The bot-token HMAC; the core has no token to check it with.
            "hash" => {}
            _ => pairs.push((k.into_owned(), v.into_owned())),
        }
    }
    let signature = signature.ok_or("the launch data is not signed")?;
    let signature = URL_SAFE_NO_PAD
        .decode(signature.trim_end_matches('='))
        .map_err(|_| "the signature is not base64url")?;
    pairs.sort();
    let mut check = format!("{bot_id}:WebAppData");
    for (k, v) in &pairs {
        check.push('\n');
        check.push_str(k);
        check.push('=');
        check.push_str(v);
    }
    let verified = openssl::pkey::PKey::public_key_from_raw_bytes(key, openssl::pkey::Id::ED25519)
        .and_then(|key| {
            openssl::sign::Verifier::new_without_digest(&key)?
                .verify_oneshot(&signature, check.as_bytes())
        })
        .unwrap_or(false);
    if !verified {
        return Err("the launch data is not signed by Telegram for this bot");
    }
    let field = |name: &str| {
        pairs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    };
    let auth_date: i64 = field("auth_date")
        .and_then(|v| v.parse().ok())
        .ok_or("the launch data has no date")?;
    if now - auth_date > MAX_AGE_SECS || auth_date > now + 300 {
        return Err("the launch data is too old; open the Mini App from the chat again");
    }
    let user: serde_json::Value = field("user")
        .and_then(|v| serde_json::from_str(v).ok())
        .ok_or("the launch data names no person")?;
    let telegram_id = user["id"]
        .as_i64()
        .ok_or("the launch data names no person")?;
    Ok(Launch { telegram_id })
}

/// The production key as bytes.
pub(crate) fn telegram_key() -> [u8; 32] {
    hex::decode(TELEGRAM_PUBLIC_KEY)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .expect("the Telegram key is 32 bytes of hex")
}

impl Auth {
    /// A short session for the person whose Telegram account opened the Mini App.
    pub async fn mini_app_session(&self, init_data: &str) -> Result<MiniAppSession> {
        let refused = |message: &str| AuthError::Unauthorized {
            invalid: true,
            message: message.to_owned(),
        };
        let bots: Vec<(String, i64)> = sqlx::query_as(
            "SELECT channel, telegram_bot_id FROM channel_about WHERE telegram_bot_id IS NOT NULL",
        )
        .fetch_all(self.pool())
        .await?;
        if bots.is_empty() {
            return Err(refused("no Telegram bot is connected to this installation"));
        }
        let now = chrono::Utc::now().timestamp();
        let key = &self.settings().telegram_key;
        let mut why = "";
        let mut found = None;
        for (channel, bot_id) in bots {
            match verify(init_data, bot_id, key, now) {
                Ok(launch) => {
                    found = Some((channel, launch));
                    break;
                }
                Err(reason) => why = reason,
            }
        }
        let (channel, launch) = found.ok_or_else(|| refused(why))?;
        let user_id = self
            .linked_user(&channel, &launch.telegram_id.to_string())
            .await?
            .ok_or_else(|| refused("this Telegram account is not linked to Frood"))?;
        let email = self.user_email(user_id).await?;
        if !self.is_allowed(&email) {
            return Err(refused(&format!(
                "{email} is not allowed to sign in to this installation"
            )));
        }
        let settings = self.settings();
        let scopes: Vec<String> = MINI_APP_SCOPES.iter().map(|s| (*s).to_owned()).collect();
        let access = new_secret("frood_at_");
        sqlx::query(
            "INSERT INTO tokens (id, token_hash, kind, user_id, client_id, scopes, resource, family, expires_at) \
             VALUES ($1, $2, 'access', $3, $4, $5, $6, $7, now() + make_interval(secs => $8))",
        )
        .bind(Uuid::now_v7())
        .bind(hash(&access))
        .bind(user_id)
        .bind(MINI_APP_CLIENT)
        .bind(&scopes)
        .bind(settings.resource_url(Resource::Api))
        .bind(Uuid::now_v7())
        .bind(settings.access_ttl.as_secs_f64())
        .execute(self.pool())
        .await?;
        sqlx::query("UPDATE users SET last_login_at = now() WHERE id = $1")
            .bind(user_id)
            .execute(self.pool())
            .await?;
        Ok(MiniAppSession {
            access_token: access,
            token_type: "Bearer",
            expires_in: settings.access_ttl.as_secs(),
            scope: scopes.join(" "),
        })
    }

    /// The channel's bot says its id, so launch data signed for it can be checked.
    pub async fn set_channel_bot_id(&self, channel: &str, bot_id: Option<i64>) -> Result<()> {
        sqlx::query(
            "INSERT INTO channel_about (channel, telegram_bot_id) VALUES ($1, $2) \
             ON CONFLICT (channel) DO UPDATE SET telegram_bot_id = EXCLUDED.telegram_bot_id, updated_at = now()",
        )
        .bind(channel)
        .bind(bot_id)
        .execute(self.pool())
        .await?;
        Ok(())
    }
}

#[derive(Deserialize)]
struct Launched {
    init_data: String,
}

pub(crate) fn routes() -> Router<Auth> {
    Router::new().route("/oauth/telegram", post(sign_in))
}

/// `POST /oauth/telegram {init_data}` → a short session, or 401 with the reason.
async fn sign_in(State(auth): State<Auth>, Json(launched): Json<Launched>) -> Response {
    let mut response = match auth.mini_app_session(&launched.init_data).await {
        Ok(session) => Json(session).into_response(),
        Err(AuthError::Unauthorized { message, .. }) => (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "type": "not-signed-in",
                "title": "Not signed in",
                "detail": message,
            })),
        )
            .into_response(),
        Err(err) => {
            tracing::warn!(error = %err, "Mini App sign-in failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "type": "internal", "title": "Sign-in failed" })),
            )
                .into_response()
        }
    };
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Signs launch data the way Telegram does, with a key of the test's own.
    fn signed(
        fields: &[(&str, &str)],
        bot_id: i64,
        key: &openssl::pkey::PKey<openssl::pkey::Private>,
    ) -> String {
        let mut pairs: Vec<(String, String)> = fields
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        pairs.sort();
        let check = std::iter::once(format!("{bot_id}:WebAppData"))
            .chain(pairs.iter().map(|(k, v)| format!("{k}={v}")))
            .collect::<Vec<_>>()
            .join("\n");
        let signature = openssl::sign::Signer::new_without_digest(key)
            .unwrap()
            .sign_oneshot_to_vec(check.as_bytes())
            .unwrap();
        let mut out = url::form_urlencoded::Serializer::new(String::new());
        for (k, v) in &pairs {
            out.append_pair(k, v);
        }
        out.append_pair("hash", "ignored");
        out.append_pair("signature", &URL_SAFE_NO_PAD.encode(signature));
        out.finish()
    }

    fn keys() -> (openssl::pkey::PKey<openssl::pkey::Private>, [u8; 32]) {
        let private = openssl::pkey::PKey::generate_ed25519().unwrap();
        let public: [u8; 32] = private.raw_public_key().unwrap().try_into().unwrap();
        (private, public)
    }

    #[test]
    fn launch_data_signed_by_telegram_names_the_person() {
        let (private, public) = keys();
        let now = 1_800_000_000;
        let data = signed(
            &[
                ("auth_date", &(now - 60).to_string()),
                ("user", r#"{"id":42,"first_name":"Alice"}"#),
                ("query_id", "AAE"),
            ],
            7,
            &private,
        );
        assert_eq!(
            verify(&data, 7, &public, now),
            Ok(Launch { telegram_id: 42 })
        );
        // Signed for another bot, or checked with another key: refused.
        assert!(verify(&data, 8, &public, now).is_err());
        assert!(verify(&data, 7, &keys().1, now).is_err());
        // Too old.
        assert!(verify(&data, 7, &public, now + MAX_AGE_SECS + 120).is_err());
        // A changed field breaks the signature.
        let forged = data.replace("%22id%22%3A42", "%22id%22%3A43");
        assert_ne!(forged, data);
        assert!(verify(&forged, 7, &public, now).is_err());
        // A repeated field is refused rather than guessed.
        assert!(verify(&format!("{data}&user=x"), 7, &public, now).is_err());
        // Unsigned.
        assert!(verify("auth_date=1&user=%7B%22id%22%3A42%7D", 7, &public, now).is_err());
    }

    #[test]
    fn the_production_key_reads() {
        assert_eq!(telegram_key().len(), 32);
    }
}
