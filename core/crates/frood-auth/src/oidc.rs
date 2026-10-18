//! Signing in through an OpenID Connect provider: discovery, authorization code with PKCE,
//! ID token verification against the provider's JWKS.

use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::Deserialize;

use crate::{Auth, AuthError, Result};

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ProviderMetadata {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub jwks_uri: String,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    id_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct IdClaims {
    #[serde(default)]
    nonce: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    email_verified: Option<serde_json::Value>,
    #[serde(default)]
    preferred_username: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

/// The verified identity from an ID token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Identity {
    pub email: String,
    pub name: Option<String>,
}

pub(crate) struct OidcProvider<'a> {
    pub id: &'a str,
    pub issuer: &'a str,
    pub client_id: &'a str,
    pub client_secret: &'a str,
}

impl Auth {
    pub(crate) async fn provider_metadata(
        &self,
        provider: &OidcProvider<'_>,
    ) -> Result<ProviderMetadata> {
        if let Some(meta) = self
            .inner
            .oidc_cache
            .lock()
            .expect("cache lock")
            .get(provider.id)
        {
            return Ok(meta.clone());
        }
        let issuer = provider.issuer.trim_end_matches('/');
        let url = format!("{issuer}/.well-known/openid-configuration");
        let meta: ProviderMetadata = self.get_json(&url).await?;
        if meta.issuer.trim_end_matches('/') != issuer {
            return Err(AuthError::Upstream(format!(
                "{url} names issuer {:?}, expected {issuer:?}",
                meta.issuer
            )));
        }
        self.inner
            .oidc_cache
            .lock()
            .expect("cache lock")
            .insert(provider.id.to_owned(), meta.clone());
        Ok(meta)
    }

    pub(crate) fn authorization_url(
        meta: &ProviderMetadata,
        provider: &OidcProvider<'_>,
        redirect_uri: &str,
        state: &str,
        nonce: &str,
        challenge: &str,
    ) -> Result<String> {
        let mut url = url::Url::parse(&meta.authorization_endpoint)
            .map_err(|_| AuthError::Upstream("authorization_endpoint is not a URL".into()))?;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", provider.client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("scope", "openid email profile")
            .append_pair("state", state)
            .append_pair("nonce", nonce)
            .append_pair("code_challenge", challenge)
            .append_pair("code_challenge_method", "S256");
        Ok(url.into())
    }

    /// Exchanges the provider's code and verifies the returned ID token.
    pub(crate) async fn finish_provider_login(
        &self,
        provider: &OidcProvider<'_>,
        code: &str,
        redirect_uri: &str,
        verifier: &str,
        nonce: &str,
    ) -> Result<Identity> {
        let meta = self.provider_metadata(provider).await?;
        let response = self
            .inner
            .http
            .post(&meta.token_endpoint)
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", redirect_uri),
                ("client_id", provider.client_id),
                ("client_secret", provider.client_secret),
                ("code_verifier", verifier),
            ])
            .send()
            .await
            .map_err(|e| AuthError::Upstream(format!("token request failed: {e}")))?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(AuthError::Upstream(format!(
                "token endpoint answered {status}: {}",
                body.chars().take(300).collect::<String>()
            )));
        }
        let tokens: TokenResponse = response
            .json()
            .await
            .map_err(|e| AuthError::Upstream(format!("bad token response: {e}")))?;
        let id_token = tokens
            .id_token
            .ok_or_else(|| AuthError::Upstream("token response has no id_token".into()))?;
        self.verify_id_token(&meta, provider, &id_token, nonce)
            .await
    }

    async fn verify_id_token(
        &self,
        meta: &ProviderMetadata,
        provider: &OidcProvider<'_>,
        id_token: &str,
        nonce: &str,
    ) -> Result<Identity> {
        let header = jsonwebtoken::decode_header(id_token)
            .map_err(|e| AuthError::Upstream(format!("bad ID token: {e}")))?;
        let allowed = [
            Algorithm::RS256,
            Algorithm::RS384,
            Algorithm::RS512,
            Algorithm::PS256,
            Algorithm::ES256,
            Algorithm::ES384,
            Algorithm::EdDSA,
        ];
        if !allowed.contains(&header.alg) {
            return Err(AuthError::Upstream(format!(
                "ID token algorithm {:?} is not accepted",
                header.alg
            )));
        }
        let jwks: JwkSet = self.get_json(&meta.jwks_uri).await?;
        let jwk = match &header.kid {
            Some(kid) => jwks.find(kid),
            None => jwks.keys.first(),
        }
        .ok_or_else(|| {
            AuthError::Upstream("no matching signing key in the provider's JWKS".into())
        })?;
        let key = DecodingKey::from_jwk(jwk)
            .map_err(|e| AuthError::Upstream(format!("unusable signing key: {e}")))?;

        let mut validation = Validation::new(header.alg);
        validation.set_audience(&[provider.client_id]);
        validation.set_issuer(&[meta.issuer.as_str(), meta.issuer.trim_end_matches('/')]);
        validation.leeway = 60;
        let claims = jsonwebtoken::decode::<IdClaims>(id_token, &key, &validation)
            .map_err(|e| AuthError::Upstream(format!("ID token rejected: {e}")))?
            .claims;

        if claims.nonce.as_deref() != Some(nonce) {
            return Err(AuthError::Upstream("ID token nonce does not match".into()));
        }
        let verified = match &claims.email_verified {
            None => true,
            Some(serde_json::Value::Bool(v)) => *v,
            Some(serde_json::Value::String(v)) => v == "true",
            Some(_) => false,
        };
        if !verified {
            return Err(AuthError::Upstream(
                "the provider says this email is not verified".into(),
            ));
        }
        // Microsoft Entra may put the address only in preferred_username.
        let email = claims
            .email
            .or(claims.preferred_username.filter(|u| u.contains('@')))
            .ok_or_else(|| AuthError::Upstream("ID token has no email".into()))?;
        Ok(Identity {
            email: email.to_lowercase(),
            name: claims.name,
        })
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T> {
        let response = self
            .inner
            .http
            .get(url)
            .send()
            .await
            .map_err(|e| AuthError::Upstream(format!("GET {url} failed: {e}")))?;
        if !response.status().is_success() {
            return Err(AuthError::Upstream(format!(
                "GET {url} answered {}",
                response.status()
            )));
        }
        response
            .json()
            .await
            .map_err(|e| AuthError::Upstream(format!("GET {url}: bad JSON: {e}")))
    }
}
