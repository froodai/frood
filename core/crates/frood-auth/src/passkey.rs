//! Passkeys (docs/28 §2): sign-in with a fingerprint, Face ID or a security key, for the people
//! in `auth.allow`. The first passkey comes through a one-time link that `frood passkey` prints
//! or the console makes after another sign-in; the browser registers it there and signs in
//! with it afterwards. The challenge between the two steps of each stays in memory: a restart
//! in between only means pressing the button again.

use std::time::{Duration, Instant};

use axum::extract::{Path, Query, RawForm, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use url::Url;
use uuid::Uuid;
use webauthn_rs::prelude::{
    Passkey, PasskeyAuthentication, PasskeyRegistration, PublicKeyCredential,
    RegisterPublicKeyCredential, Webauthn, WebauthnBuilder,
};

use crate::pages::{escape, page};
use crate::secret::{hash, new_secret};
use crate::web::{error_page, finish_sign_in, form_value, form_values};
use crate::{Auth, AuthError, Result};

/// How long a link to add a passkey works.
pub const LINK_MINUTES: i64 = 15;
/// How long the browser has between asking for the challenge and answering it.
const STEP: Duration = Duration::from_secs(300);

/// What waits between the two steps.
pub(crate) enum Step {
    Enrol {
        user_id: Uuid,
        state: PasskeyRegistration,
    },
    SignIn {
        user_id: Uuid,
        state: PasskeyAuthentication,
    },
}

/// A person's passkey, as the console lists it.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct PasskeyInfo {
    pub id: Uuid,
    pub label: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

pub(crate) fn routes() -> Router<Auth> {
    Router::new()
        .route("/oauth/passkey", get(enrol_page).post(enrol_finish))
        .route("/oauth/passkey/options", post(enrol_options))
        .route("/oauth/login/{provider}/passkey", post(sign_in_finish))
        .route(
            "/oauth/login/{provider}/passkey/options",
            post(sign_in_options),
        )
}

impl Auth {
    /// Passkeys are on when `auth.providers` has one.
    pub fn passkeys_on(&self) -> bool {
        self.settings()
            .providers
            .iter()
            .any(|p| matches!(p, frood_config::Provider::Passkey { .. }))
    }

    /// The relying party: this installation's host, its origin.
    fn webauthn(&self) -> Result<Webauthn> {
        let issuer = &self.settings().issuer;
        let cannot =
            |why: String| AuthError::Invalid(format!("passkeys cannot work at {issuer}: {why}"));
        let url = Url::parse(issuer).map_err(|e| cannot(e.to_string()))?;
        let host = url.host_str().ok_or_else(|| cannot("no host".into()))?;
        let origin =
            Url::parse(&url.origin().ascii_serialization()).map_err(|e| cannot(e.to_string()))?;
        WebauthnBuilder::new(host, &origin)
            .and_then(|b| b.rp_name("Frood").build())
            .map_err(|e| cannot(e.to_string()))
    }

    /// A one-time link that adds a passkey for this person, and until when it works.
    pub async fn passkey_link(&self, user_id: Uuid) -> Result<(String, DateTime<Utc>)> {
        if !self.passkeys_on() {
            return Err(AuthError::Invalid(
                "Passkeys are off here: add `kind: passkey` to auth.providers in frood.yaml."
                    .into(),
            ));
        }
        let email = self.user_email(user_id).await?;
        if !self.settings().allow.contains(&email) {
            return Err(AuthError::Invalid(format!(
                "{email} is not in auth.allow, so it cannot sign in with a passkey."
            )));
        }
        let code = new_secret("pk_");
        let expires_at: DateTime<Utc> = sqlx::query_scalar(
            "INSERT INTO passkey_links (code_hash, user_id, expires_at) \
             VALUES ($1, $2, now() + make_interval(mins => $3)) RETURNING expires_at",
        )
        .bind(hash(&code))
        .bind(user_id)
        .bind(LINK_MINUTES as i32)
        .fetch_one(self.pool())
        .await?;
        Ok((
            format!("{}/oauth/passkey?code={code}", self.settings().issuer),
            expires_at,
        ))
    }

    pub async fn passkeys_of(&self, user_id: Uuid) -> Result<Vec<PasskeyInfo>> {
        Ok(sqlx::query_as(
            "SELECT id, label, created_at, last_used_at FROM passkeys \
             WHERE user_id = $1 ORDER BY created_at",
        )
        .bind(user_id)
        .fetch_all(self.pool())
        .await?)
    }

    /// The person removes one of their passkeys; true when there was one.
    pub async fn remove_passkey(&self, user_id: Uuid, id: Uuid) -> Result<bool> {
        let removed = sqlx::query("DELETE FROM passkeys WHERE id = $1 AND user_id = $2")
            .bind(id)
            .bind(user_id)
            .execute(self.pool())
            .await?;
        Ok(removed.rows_affected() == 1)
    }

    /// The person a working link is for, with their address.
    async fn link_user(&self, code: &str) -> Result<(Uuid, String)> {
        let row: Option<(Uuid, String)> = sqlx::query_as(
            "SELECT l.user_id, u.email FROM passkey_links l JOIN users u ON u.id = l.user_id \
             WHERE l.code_hash = $1 AND l.used_at IS NULL AND l.expires_at > now()",
        )
        .bind(hash(code))
        .fetch_optional(self.pool())
        .await?;
        match row {
            Some((user_id, email)) if self.settings().allow.contains(&email) => Ok((user_id, email)),
            _ => Err(AuthError::Invalid(
                "This link has expired or was used. Ask for a new one: `frood passkey`, or the console."
                    .into(),
            )),
        }
    }

    async fn stored_passkeys(&self, user_id: Uuid) -> Result<Vec<Passkey>> {
        let rows: Vec<serde_json::Value> =
            sqlx::query_scalar("SELECT passkey FROM passkeys WHERE user_id = $1")
                .bind(user_id)
                .fetch_all(self.pool())
                .await?;
        rows.into_iter()
            .map(|v| {
                serde_json::from_value(v)
                    .map_err(|e| AuthError::Invalid(format!("a stored passkey is unreadable: {e}")))
            })
            .collect()
    }

    fn keep_step(&self, key: String, step: Step) {
        let mut steps = self
            .inner
            .passkey_steps
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        steps.retain(|_, (_, at)| at.elapsed() < STEP);
        steps.insert(key, (step, Instant::now()));
    }

    fn take_step(&self, key: &str) -> Option<Step> {
        let mut steps = self
            .inner
            .passkey_steps
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        steps
            .remove(key)
            .filter(|(_, at)| at.elapsed() < STEP)
            .map(|(step, _)| step)
    }

    /// The challenge to create a passkey with, for the link `code`.
    async fn enrol_options(&self, code: &str) -> Result<serde_json::Value> {
        let (user_id, email) = self.link_user(code).await?;
        let known = self
            .stored_passkeys(user_id)
            .await?
            .iter()
            .map(|p| p.cred_id().clone())
            .collect();
        let (challenge, state) = self
            .webauthn()?
            .start_passkey_registration(user_id, &email, &email, Some(known))
            .map_err(|e| AuthError::Invalid(format!("cannot start: {e}")))?;
        self.keep_step(
            format!("enrol:{}", hash(code)),
            Step::Enrol { user_id, state },
        );
        serde_json::to_value(challenge).map_err(|e| AuthError::Invalid(e.to_string()))
    }

    /// Keeps the passkey the browser created; the link is used up. Returns the address.
    async fn enrol(
        &self,
        code: &str,
        credential: &RegisterPublicKeyCredential,
        label: Option<&str>,
    ) -> Result<String> {
        let (user_id, email) = self.link_user(code).await?;
        let Some(Step::Enrol {
            user_id: asked,
            state,
        }) = self.take_step(&format!("enrol:{}", hash(code)))
        else {
            return Err(AuthError::Invalid(
                "The page waited too long. Open the link again and create the passkey.".into(),
            ));
        };
        if asked != user_id {
            return Err(AuthError::Invalid(
                "This answer is for another link.".into(),
            ));
        }
        let passkey = self
            .webauthn()?
            .finish_passkey_registration(credential, &state)
            .map_err(|e| AuthError::Invalid(format!("The passkey was not accepted: {e}")))?;
        let credential_id = serde_json::to_value(passkey.cred_id())
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default();
        let stored = sqlx::query(
            "INSERT INTO passkeys (id, user_id, credential_id, passkey, label) VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (credential_id) DO NOTHING",
        )
        .bind(Uuid::now_v7())
        .bind(user_id)
        .bind(&credential_id)
        .bind(serde_json::to_value(&passkey).map_err(|e| AuthError::Invalid(e.to_string()))?)
        .bind(label.map(str::trim).filter(|l| !l.is_empty()))
        .execute(self.pool())
        .await?;
        if stored.rows_affected() == 0 {
            return Err(AuthError::Invalid("This passkey is already added.".into()));
        }
        sqlx::query("UPDATE passkey_links SET used_at = now() WHERE code_hash = $1")
            .bind(hash(code))
            .execute(self.pool())
            .await?;
        Ok(email)
    }

    /// The challenge to sign in with, for the sign-in request `tx` and the person's address.
    async fn sign_in_options(&self, tx: &str, email: &str) -> Result<serde_json::Value> {
        let email = email.trim().to_lowercase();
        if !self.settings().allow.contains(&email) {
            return Err(AuthError::Invalid(
                "This address may not sign in here.".into(),
            ));
        }
        let none_yet = || {
            AuthError::Invalid(
                "No passkey for this address yet. Add one first: `frood passkey --email …` on the \
                 server, or in the console after signing in another way."
                    .into(),
            )
        };
        let user_id = match self.user_id_by_email(&email, false).await {
            Ok(id) => id,
            Err(AuthError::NotFound(_)) => return Err(none_yet()),
            Err(err) => return Err(err),
        };
        let passkeys = self.stored_passkeys(user_id).await?;
        if passkeys.is_empty() {
            return Err(none_yet());
        }
        let (challenge, state) = self
            .webauthn()?
            .start_passkey_authentication(&passkeys)
            .map_err(|e| AuthError::Invalid(format!("cannot start: {e}")))?;
        self.keep_step(
            format!("sign_in:{}", hash(tx)),
            Step::SignIn { user_id, state },
        );
        serde_json::to_value(challenge).map_err(|e| AuthError::Invalid(e.to_string()))
    }

    /// Checks the browser's answer; returns the address of the person it signs in.
    async fn sign_in_with(&self, tx: &str, credential: &PublicKeyCredential) -> Result<String> {
        let Some(Step::SignIn { user_id, state }) =
            self.take_step(&format!("sign_in:{}", hash(tx)))
        else {
            return Err(AuthError::Invalid(
                "The page waited too long. Press the button again.".into(),
            ));
        };
        let result = self
            .webauthn()?
            .finish_passkey_authentication(credential, &state)
            .map_err(|_| AuthError::Invalid("The passkey did not match.".into()))?;
        // The counter moves on, so a copied passkey shows itself.
        let rows: Vec<(Uuid, serde_json::Value)> =
            sqlx::query_as("SELECT id, passkey FROM passkeys WHERE user_id = $1")
                .bind(user_id)
                .fetch_all(self.pool())
                .await?;
        for (id, value) in rows {
            let Ok(mut passkey) = serde_json::from_value::<Passkey>(value) else {
                continue;
            };
            if passkey.update_credential(&result).is_none() {
                continue;
            }
            sqlx::query("UPDATE passkeys SET passkey = $2, last_used_at = now() WHERE id = $1")
                .bind(id)
                .bind(
                    serde_json::to_value(&passkey)
                        .map_err(|e| AuthError::Invalid(e.to_string()))?,
                )
                .execute(self.pool())
                .await?;
        }
        self.user_email(user_id).await
    }
}

fn json_error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "detail": message }))).into_response()
}

fn refusal(err: AuthError) -> (StatusCode, String) {
    match err {
        AuthError::Invalid(message) => (StatusCode::FORBIDDEN, message),
        err => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    }
}

#[derive(Deserialize)]
struct CodeQuery {
    code: String,
}

async fn enrol_page(
    State(auth): State<Auth>,
    Query(CodeQuery { code }): Query<CodeQuery>,
) -> Response {
    match auth.link_user(&code).await {
        Ok((_, email)) => {
            let settings = auth.settings();
            enrol_form(&settings.installation, &settings.base_path, &code, &email).into_response()
        }
        Err(err) => {
            let (status, message) = refusal(err);
            error_page(status, &message)
        }
    }
}

async fn enrol_options(
    State(auth): State<Auth>,
    headers: HeaderMap,
    Json(body): Json<CodeQuery>,
) -> Response {
    if !auth.same_origin(&headers) {
        return json_error(StatusCode::FORBIDDEN, "Cross-site request refused.");
    }
    match auth.enrol_options(&body.code).await {
        Ok(options) => Json(options).into_response(),
        Err(err) => {
            let (status, message) = refusal(err);
            json_error(status, &message)
        }
    }
}

async fn enrol_finish(
    State(auth): State<Auth>,
    headers: HeaderMap,
    RawForm(form): RawForm,
) -> Response {
    if !auth.same_origin(&headers) {
        return error_page(StatusCode::FORBIDDEN, "Cross-site request refused.");
    }
    let values = form_values(&form);
    let (Some(code), Some(credential)) = (
        form_value(&values, "code"),
        form_value(&values, "credential"),
    ) else {
        return error_page(StatusCode::BAD_REQUEST, "The form is incomplete.");
    };
    let Ok(credential) = serde_json::from_str::<RegisterPublicKeyCredential>(credential) else {
        return error_page(
            StatusCode::BAD_REQUEST,
            "The browser's answer cannot be read.",
        );
    };
    match auth
        .enrol(code, &credential, form_value(&values, "label"))
        .await
    {
        Ok(email) => added(&auth.settings().installation, &email).into_response(),
        Err(err) => {
            let (status, message) = refusal(err);
            error_page(status, &message)
        }
    }
}

#[derive(Deserialize)]
struct SignInBody {
    tx: String,
    email: String,
}

fn is_passkey(auth: &Auth, provider_id: &str) -> bool {
    matches!(
        auth.settings()
            .providers
            .iter()
            .find(|p| p.id() == provider_id),
        Some(frood_config::Provider::Passkey { .. })
    )
}

async fn sign_in_options(
    State(auth): State<Auth>,
    Path(provider_id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<SignInBody>,
) -> Response {
    if !is_passkey(&auth, &provider_id) {
        return json_error(StatusCode::NOT_FOUND, "Unknown sign-in provider.");
    }
    if !auth.same_origin(&headers) {
        return json_error(StatusCode::FORBIDDEN, "Cross-site request refused.");
    }
    if let Err(err) = auth.pending("tx", &body.tx).await {
        return json_error(StatusCode::BAD_REQUEST, &err.to_string());
    }
    match auth.sign_in_options(&body.tx, &body.email).await {
        Ok(options) => Json(options).into_response(),
        Err(err) => {
            let (status, message) = refusal(err);
            json_error(status, &message)
        }
    }
}

async fn sign_in_finish(
    State(auth): State<Auth>,
    Path(provider_id): Path<String>,
    headers: HeaderMap,
    RawForm(form): RawForm,
) -> Response {
    if !is_passkey(&auth, &provider_id) {
        return error_page(StatusCode::NOT_FOUND, "Unknown sign-in provider.");
    }
    if !auth.same_origin(&headers) {
        return error_page(StatusCode::FORBIDDEN, "Cross-site request refused.");
    }
    let values = form_values(&form);
    let (Some(tx), Some(credential)) =
        (form_value(&values, "tx"), form_value(&values, "credential"))
    else {
        return error_page(StatusCode::BAD_REQUEST, "The form is incomplete.");
    };
    if let Err(err) = auth.pending("tx", tx).await {
        return error_page(StatusCode::BAD_REQUEST, &err.to_string());
    }
    let Ok(credential) = serde_json::from_str::<PublicKeyCredential>(credential) else {
        return error_page(
            StatusCode::BAD_REQUEST,
            "The browser's answer cannot be read.",
        );
    };
    match auth.sign_in_with(tx, &credential).await {
        Ok(email) => finish_sign_in(&auth, tx, &email, None).await,
        Err(err) => {
            let (status, message) = refusal(err);
            error_page(status, &message)
        }
    }
}

/// Asks the server for the challenge, hands it to the browser, and submits the form with the
/// browser's answer as JSON. Forms with `data-options` get it; `data-create` registers.
const SCRIPT: &str = r#"<p id="note" class="muted" role="status"></p><script>
const dec = s => Uint8Array.from(atob(s.replace(/-/g, '+').replace(/_/g, '/') + '='.repeat((4 - s.length % 4) % 4)), c => c.charCodeAt(0));
const enc = b => btoa(String.fromCharCode(...new Uint8Array(b))).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
async function passkey(form) {
  const note = document.getElementById('note');
  note.textContent = '';
  if (!window.PublicKeyCredential) { note.textContent = 'This browser cannot use passkeys.'; return; }
  try {
    const body = Object.fromEntries(new FormData(form));
    const answer = await fetch(form.dataset.options, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) });
    const json = await answer.json();
    if (!answer.ok) { note.textContent = json.detail || 'Something went wrong.'; return; }
    const o = json.publicKey;
    o.challenge = dec(o.challenge);
    let c;
    if (form.dataset.create) {
      o.user.id = dec(o.user.id);
      (o.excludeCredentials || []).forEach(x => { x.id = dec(x.id); });
      c = await navigator.credentials.create({ publicKey: o });
      form.credential.value = JSON.stringify({ id: c.id, rawId: enc(c.rawId), type: c.type,
        extensions: c.getClientExtensionResults(),
        response: { attestationObject: enc(c.response.attestationObject), clientDataJSON: enc(c.response.clientDataJSON),
          transports: c.response.getTransports ? c.response.getTransports() : undefined } });
    } else {
      (o.allowCredentials || []).forEach(x => { x.id = dec(x.id); });
      c = await navigator.credentials.get({ publicKey: o });
      form.credential.value = JSON.stringify({ id: c.id, rawId: enc(c.rawId), type: c.type,
        extensions: c.getClientExtensionResults(),
        response: { authenticatorData: enc(c.response.authenticatorData), clientDataJSON: enc(c.response.clientDataJSON),
          signature: enc(c.response.signature), userHandle: c.response.userHandle ? enc(c.response.userHandle) : null } });
    }
    form.submit();
  } catch (e) {
    note.textContent = e.name === 'NotAllowedError' ? 'Cancelled, or the passkey did not answer. Try again.' : String(e);
  }
}
document.querySelectorAll('form[data-options]').forEach(form => form.addEventListener('submit', e => {
  if (!form.credential.value) { e.preventDefault(); passkey(form); }
}));
</script>"#;

fn enrol_form(
    installation: &str,
    base: &str,
    code: &str,
    email: &str,
) -> axum::response::Html<String> {
    page(
        "Add a passkey",
        &format!(
            "<h1>Add a passkey</h1><p>For <b>{}</b>, installation <code>{}</code>. Your device asks for a fingerprint, \
             Face ID or its screen lock; afterwards you sign in with it.</p>\
             <form method=\"post\" action=\"{base}/oauth/passkey\" data-options=\"{base}/oauth/passkey/options\" data-create=\"1\">\
             <input type=\"hidden\" name=\"code\" value=\"{}\"><input type=\"hidden\" name=\"credential\">\
             <input type=\"text\" name=\"label\" placeholder=\"This device, e.g. iPhone\" maxlength=\"60\">\
             <button class=\"btn\" type=\"submit\">Create a passkey</button></form>{SCRIPT}",
            escape(email),
            escape(installation),
            escape(code),
            base = escape(base),
        ),
    )
}

fn added(installation: &str, email: &str) -> axum::response::Html<String> {
    page(
        "Passkey added",
        &format!(
            "<h1>Passkey added</h1><p><b>{}</b> can now sign in to <code>{}</code> with it: choose \
             <b>Passkey</b> on the sign-in page.</p><p class=\"muted\">You can close this page.</p>",
            escape(email),
            escape(installation),
        ),
    )
}

/// The sign-in page: the address, then the device's prompt.
pub(crate) fn sign_in_form(
    installation: &str,
    base: &str,
    provider: &str,
    tx: &str,
    email: &str,
) -> axum::response::Html<String> {
    page(
        "Sign in with a passkey",
        &format!(
            "<h1>Sign in with a passkey</h1><p class=\"muted\">Installation <code>{}</code>.</p>\
             <form method=\"post\" action=\"{base}/oauth/login/{provider}/passkey\" data-options=\"{base}/oauth/login/{provider}/passkey/options\">\
             <input type=\"hidden\" name=\"tx\" value=\"{}\"><input type=\"hidden\" name=\"credential\">\
             <input type=\"email\" name=\"email\" value=\"{}\" placeholder=\"you@example.com\" required autofocus>\
             <button class=\"btn\" type=\"submit\">Use my passkey</button></form>{SCRIPT}\
             <p class=\"muted\">No passkey yet? Run <code>frood passkey --email you@…</code> where Frood is installed, \
             or add one in the console after signing in another way.</p>",
            escape(installation),
            escape(tx),
            escape(email),
            base = escape(base),
            provider = escape(provider),
        ),
    )
}
