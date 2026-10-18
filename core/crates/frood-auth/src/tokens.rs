//! Access, refresh and personal tokens.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::Row;
use uuid::Uuid;

use crate::secret::{hash, new_secret};
use crate::{Auth, AuthError, Principal, Resource, Result, scope};

#[derive(Debug, Clone, Serialize)]
pub struct TokenPair {
    pub access_token: String,
    pub token_type: &'static str,
    pub expires_in: u64,
    pub refresh_token: String,
    pub scope: String,
}

/// A run token as issued; the secret goes to the runtime and nowhere else.
#[derive(Debug, Clone)]
pub struct RunToken {
    pub id: Uuid,
    pub token: String,
    pub scopes: Vec<String>,
}

/// A personal token as listed; the secret itself is shown only once, at creation.
#[derive(Debug, Clone, Serialize)]
pub struct PersonalToken {
    pub id: Uuid,
    pub label: Option<String>,
    pub scopes: Vec<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

impl Auth {
    /// Resolves `Authorization: Bearer …` to a principal, checking expiry, revocation and the
    /// token's audience.
    pub async fn authenticate(
        &self,
        authorization: Option<&str>,
        resource: Resource,
    ) -> Result<Principal> {
        let token = authorization
            .and_then(|value| {
                value
                    .strip_prefix("Bearer ")
                    .or_else(|| value.strip_prefix("bearer "))
            })
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .ok_or_else(|| AuthError::Unauthorized {
                invalid: false,
                message: "a bearer token is required".into(),
            })?;
        let invalid = |message: &str| AuthError::Unauthorized {
            invalid: true,
            message: message.to_owned(),
        };

        let row = sqlx::query(
            "SELECT t.id, t.kind, t.user_id, t.module, t.run_id, t.label, t.client_id, t.scopes, t.resource, \
             (t.expires_at IS NOT NULL AND t.expires_at <= now()) AS expired, t.revoked_at IS NOT NULL AS revoked, u.email \
             FROM tokens t LEFT JOIN users u ON u.id = t.user_id WHERE t.token_hash = $1",
        )
        .bind(hash(token))
        .fetch_optional(self.pool())
        .await?
        .ok_or_else(|| invalid("the token is not known"))?;

        let kind: String = row.try_get("kind")?;
        if kind == "refresh" {
            return Err(invalid("a refresh token cannot be used as an access token"));
        }
        if row.try_get::<bool, _>("revoked")? {
            return Err(invalid("the token was revoked"));
        }
        if row.try_get::<bool, _>("expired")? {
            return Err(invalid("the token expired"));
        }
        let audience: Option<String> = row.try_get("resource")?;
        let settings = self.settings();
        if let Some(audience) = &audience
            && audience != &settings.resource_url(resource)
            && audience != &settings.issuer
        {
            return Err(invalid("the token was issued for another resource"));
        }

        let id: Uuid = row.try_get("id")?;
        sqlx::query("UPDATE tokens SET last_used_at = now() WHERE id = $1 AND (last_used_at IS NULL OR last_used_at < now() - interval '1 minute')")
            .bind(id)
            .execute(self.pool())
            .await?;
        let user = match (
            row.try_get::<Option<Uuid>, _>("user_id")?,
            row.try_get::<Option<String>, _>("email")?,
        ) {
            (Some(id), Some(email)) => Some(crate::User { id, email }),
            _ => None,
        };
        let run = match row.try_get::<Option<Uuid>, _>("run_id")? {
            Some(id) => Some(crate::RunRef {
                id,
                job: row
                    .try_get::<Option<String>, _>("label")?
                    .unwrap_or_default(),
            }),
            None => None,
        };
        Ok(Principal {
            user,
            run,
            module: row.try_get("module")?,
            scopes: row.try_get("scopes")?,
            client_id: row.try_get("client_id")?,
            token_id: id,
        })
    }

    /// Issues an access + refresh pair in a rotation family.
    pub(crate) async fn issue_pair(
        &self,
        conn: &mut sqlx::PgConnection,
        user_id: Uuid,
        client_id: &str,
        scopes: &[String],
        resource: Option<&str>,
        family: Uuid,
    ) -> Result<TokenPair> {
        let settings = self.settings();
        let access = new_secret("frood_at_");
        let refresh = new_secret("frood_rt_");
        for (secret, kind, ttl) in [
            (&access, "access", settings.access_ttl),
            (&refresh, "refresh", settings.refresh_ttl),
        ] {
            sqlx::query(
                "INSERT INTO tokens (id, token_hash, kind, user_id, client_id, scopes, resource, family, expires_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, now() + make_interval(secs => $9))",
            )
            .bind(Uuid::now_v7())
            .bind(hash(secret))
            .bind(kind)
            .bind(user_id)
            .bind(client_id)
            .bind(scopes)
            .bind(resource)
            .bind(family)
            .bind(ttl.as_secs_f64())
            .execute(&mut *conn)
            .await?;
        }
        Ok(TokenPair {
            access_token: access,
            token_type: "Bearer",
            expires_in: settings.access_ttl.as_secs(),
            refresh_token: refresh,
            scope: scopes.join(" "),
        })
    }

    /// Refresh-token rotation. Presenting an already rotated refresh token revokes the whole
    /// family: someone else may hold a copy.
    pub(crate) async fn refresh(&self, refresh_token: &str, client_id: &str) -> Result<TokenPair> {
        let mut tx = self.pool().begin().await?;
        let row = sqlx::query(
            "SELECT id, user_id, client_id, scopes, resource, family, revoked_at IS NOT NULL AS revoked, \
             expires_at <= now() AS expired FROM tokens WHERE token_hash = $1 AND kind = 'refresh' FOR UPDATE",
        )
        .bind(hash(refresh_token))
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| AuthError::Invalid("invalid_grant: unknown refresh token".into()))?;

        let family: Uuid = row.try_get("family")?;
        if row.try_get::<bool, _>("revoked")? {
            sqlx::query(
                "UPDATE tokens SET revoked_at = now() WHERE family = $1 AND revoked_at IS NULL",
            )
            .bind(family)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            tracing::warn!(%family, "rotated refresh token reused; family revoked");
            return Err(AuthError::Invalid(
                "invalid_grant: refresh token was already used".into(),
            ));
        }
        if row.try_get::<bool, _>("expired")? {
            return Err(AuthError::Invalid(
                "invalid_grant: refresh token expired".into(),
            ));
        }
        if row.try_get::<Option<String>, _>("client_id")?.as_deref() != Some(client_id) {
            return Err(AuthError::Invalid(
                "invalid_grant: refresh token belongs to another client".into(),
            ));
        }
        let id: Uuid = row.try_get("id")?;
        sqlx::query("UPDATE tokens SET revoked_at = now() WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        let user_id: Uuid = row.try_get("user_id")?;
        let scopes: Vec<String> = row.try_get("scopes")?;
        let resource: Option<String> = row.try_get("resource")?;
        let pair = self
            .issue_pair(
                &mut tx,
                user_id,
                client_id,
                &scopes,
                resource.as_deref(),
                family,
            )
            .await?;
        tx.commit().await?;
        Ok(pair)
    }

    /// Creates a personal token for scripts and bots. Returns the id and the secret.
    pub async fn create_personal_token(
        &self,
        user_id: Uuid,
        label: &str,
        scopes: &[String],
        expires_in_days: Option<u32>,
    ) -> Result<(Uuid, String)> {
        for requested in scopes {
            if !scope::ALL.contains(&requested.as_str()) {
                return Err(AuthError::Invalid(format!("unknown scope {requested:?}")));
            }
        }
        if scopes.is_empty() {
            return Err(AuthError::Invalid(
                "a personal token needs at least one scope".into(),
            ));
        }
        let id = Uuid::now_v7();
        let secret = new_secret("frood_pat_");
        sqlx::query(
            "INSERT INTO tokens (id, token_hash, kind, user_id, scopes, label, expires_at) \
             VALUES ($1, $2, 'personal', $3, $4, $5, CASE WHEN $6::int IS NULL THEN NULL ELSE now() + make_interval(days => $6) END)",
        )
        .bind(id)
        .bind(hash(&secret))
        .bind(user_id)
        .bind(scopes)
        .bind(label)
        .bind(expires_in_days.map(|d| d as i32))
        .execute(self.pool())
        .await?;
        Ok((id, secret))
    }

    pub async fn personal_tokens(&self, user_id: Uuid) -> Result<Vec<PersonalToken>> {
        let rows = sqlx::query(
            "SELECT id, label, scopes, expires_at, created_at, last_used_at FROM tokens \
             WHERE user_id = $1 AND kind = 'personal' AND revoked_at IS NULL ORDER BY created_at",
        )
        .bind(user_id)
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(|row| {
                Ok(PersonalToken {
                    id: row.try_get("id")?,
                    label: row.try_get("label")?,
                    scopes: row.try_get("scopes")?,
                    expires_at: row.try_get("expires_at")?,
                    created_at: row.try_get("created_at")?,
                    last_used_at: row.try_get("last_used_at")?,
                })
            })
            .collect()
    }

    /// Revokes one of the user's tokens; for access or refresh tokens, the whole family.
    pub async fn revoke_token(&self, user_id: Uuid, token_id: Uuid) -> Result<()> {
        let done = sqlx::query(
            "UPDATE tokens SET revoked_at = now() WHERE revoked_at IS NULL AND user_id = $1 AND \
             (id = $2 OR family = (SELECT family FROM tokens WHERE id = $2 AND family IS NOT NULL))",
        )
        .bind(user_id)
        .bind(token_id)
        .execute(self.pool())
        .await?;
        if done.rows_affected() == 0 {
            return Err(AuthError::NotFound(format!("token {token_id}")));
        }
        Ok(())
    }

    /// Registers the service token a module was given at deploy time (only its hash is
    /// stored) with the scopes its manifest asks for; earlier tokens of the module stop working.
    pub async fn register_service_token(
        &self,
        module: &str,
        secret: &str,
        scopes: &[String],
    ) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        sqlx::query(
            "UPDATE tokens SET revoked_at = now() WHERE kind = 'service' AND module = $1 \
             AND revoked_at IS NULL AND token_hash <> $2",
        )
        .bind(module)
        .bind(hash(secret))
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO tokens (id, token_hash, kind, module, scopes, label) \
             VALUES ($1, $2, 'service', $3, $4, $5) \
             ON CONFLICT (token_hash) DO UPDATE SET scopes = EXCLUDED.scopes, revoked_at = NULL",
        )
        .bind(Uuid::now_v7())
        .bind(hash(secret))
        .bind(module)
        .bind(scopes)
        .bind(format!("module {module}"))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// The credentials of one agent run: rights of the job within what agents may hold, for
    /// the person the run acts for, valid until the run ends (or `ttl`).
    pub async fn issue_run_token(
        &self,
        run_id: Uuid,
        job: &str,
        on_behalf_of: Option<Uuid>,
        requested: &[String],
        ttl: std::time::Duration,
    ) -> Result<RunToken> {
        let scopes: Vec<String> = requested
            .iter()
            .filter(|s| scope::AGENT_ALLOWED.contains(&s.as_str()))
            .cloned()
            .collect();
        let secret = new_secret("frood_run_");
        let id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO tokens (id, token_hash, kind, user_id, run_id, scopes, label, expires_at) \
             VALUES ($1, $2, 'run', $3, $4, $5, $6, now() + make_interval(secs => $7))",
        )
        .bind(id)
        .bind(hash(&secret))
        .bind(on_behalf_of)
        .bind(run_id)
        .bind(&scopes)
        .bind(job)
        .bind(ttl.as_secs_f64())
        .execute(self.pool())
        .await?;
        Ok(RunToken {
            id,
            token: secret,
            scopes,
        })
    }

    /// Ends every token of a run (when it finishes, fails or is cancelled).
    pub async fn revoke_run_tokens(&self, run_id: Uuid) -> Result<()> {
        sqlx::query(
            "UPDATE tokens SET revoked_at = now() WHERE kind = 'run' AND run_id = $1 AND revoked_at IS NULL",
        )
        .bind(run_id)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// RFC 7009: revoke by the secret itself. Unknown tokens are not an error.
    pub(crate) async fn revoke_secret(&self, secret: &str) -> Result<()> {
        sqlx::query(
            "UPDATE tokens SET revoked_at = now() WHERE revoked_at IS NULL AND \
             (token_hash = $1 OR family = (SELECT family FROM tokens WHERE token_hash = $1 AND family IS NOT NULL))",
        )
        .bind(hash(secret))
        .execute(self.pool())
        .await?;
        Ok(())
    }
}
