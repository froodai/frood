//! Who may sign in, and the users table.

use uuid::Uuid;

use crate::{Auth, AuthError, Result};

impl Auth {
    /// Whether an email may sign in: listed in `auth.allow` or in an allowed domain.
    pub fn is_allowed(&self, email: &str) -> bool {
        let email = email.trim().to_lowercase();
        let settings = self.settings();
        if settings.allow.contains(&email) {
            return true;
        }
        email
            .rsplit_once('@')
            .is_some_and(|(_, domain)| settings.allow_domains.iter().any(|allowed| allowed == domain))
    }

    /// Finds or creates the user for a verified email and records the sign-in.
    pub(crate) async fn sign_in(&self, email: &str, display_name: Option<&str>) -> Result<Uuid> {
        let email = email.trim().to_lowercase();
        if !self.is_allowed(&email) {
            return Err(AuthError::Invalid(format!("{email} is not allowed to sign in to this installation")));
        }
        Ok(sqlx::query_scalar(
            "INSERT INTO users (id, email, display_name, last_login_at) VALUES ($1, $2, $3, now()) \
             ON CONFLICT (email) DO UPDATE SET last_login_at = now(), \
             display_name = COALESCE(EXCLUDED.display_name, users.display_name) RETURNING id",
        )
        .bind(Uuid::now_v7())
        .bind(&email)
        .bind(display_name)
        .fetch_one(self.pool())
        .await?)
    }

    /// Finds a user by email, creating it when `create` is set (for administrative tools).
    pub async fn user_id_by_email(&self, email: &str, create: bool) -> Result<Uuid> {
        let email = email.trim().to_lowercase();
        if create {
            return Ok(sqlx::query_scalar(
                "INSERT INTO users (id, email) VALUES ($1, $2) ON CONFLICT (email) DO UPDATE SET email = EXCLUDED.email RETURNING id",
            )
            .bind(Uuid::now_v7())
            .bind(&email)
            .fetch_one(self.pool())
            .await?);
        }
        sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
            .bind(&email)
            .fetch_optional(self.pool())
            .await?
            .ok_or_else(|| AuthError::NotFound(format!("user {email}")))
    }
}
