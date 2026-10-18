//! Bearer authentication of `/v1` and `/mcp` (docs/12_api_mcp_auth.md §3).

use std::marker::PhantomData;

use axum::extract::{FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use frood_auth::{Auth, AuthError, Principal, Resource, scope};
use frood_ledger::model::Actor;

use crate::AppState;
use crate::error::ApiError;

/// The scope a route needs; `None` means any valid token.
pub trait Permission {
    const SCOPE: Option<&'static str>;
}

macro_rules! permissions {
    ($($(#[$doc:meta])* $name:ident => $scope:expr;)*) => {
        $(
            $(#[$doc])*
            pub struct $name;
            impl Permission for $name {
                const SCOPE: Option<&'static str> = $scope;
            }
        )*
    };
}

permissions! {
    /// Any valid token.
    Signed => None;
    Read => Some(scope::READ);
    Capture => Some(scope::CAPTURE);
    Propose => Some(scope::PROPOSE);
    Decide => Some(scope::DECIDE);
    Journal => Some(scope::JOURNAL);
    Admin => Some(scope::ADMIN);
    /// Channel modules: link accounts, act for linked people.
    ChannelsAct => Some(scope::CHANNELS_ACT);
}

/// The authenticated caller of a `/v1` route, holding the permission `P`.
pub struct Caller<P> {
    pub principal: Principal,
    permission: PhantomData<P>,
}

impl<P> Caller<P> {
    pub fn actor(&self) -> Actor {
        Actor::parse(&self.principal.actor()).expect("principal actors are well-formed")
    }

    /// The person behind the token; module service tokens have none.
    pub fn user_id(&self) -> Result<uuid::Uuid, ApiError> {
        self.principal.user_id().ok_or_else(|| {
            ApiError::new(
                StatusCode::FORBIDDEN,
                "person-required",
                "this needs a person's token, not a module's",
            )
        })
    }
}

impl<P: Permission> FromRequestParts<AppState> for Caller<P> {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let authorization = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok());
        let principal = state
            .auth
            .authenticate(authorization, Resource::Api)
            .await
            .map_err(|err| auth_error(&state.auth, Resource::Api, err))?;
        if let Some(needed) = P::SCOPE
            && !principal.has(needed)
        {
            return Err(auth_error(
                &state.auth,
                Resource::Api,
                AuthError::InsufficientScope(needed.to_owned()),
            ));
        }
        Ok(Self {
            principal,
            permission: PhantomData,
        })
    }
}

/// Maps an authentication failure to a problem response; 401 and 403 carry an RFC 6750
/// challenge that points at the resource metadata (RFC 9728), which is how MCP clients
/// discover where to sign in.
pub fn auth_error(auth: &Auth, resource: Resource, err: AuthError) -> ApiError {
    let metadata = auth.settings().resource_metadata_url(resource);
    match err {
        AuthError::Unauthorized { invalid, message } => {
            let challenge = if invalid {
                format!(
                    "Bearer error=\"invalid_token\", error_description=\"{}\", resource_metadata=\"{metadata}\"",
                    message.replace('"', "'")
                )
            } else {
                format!("Bearer resource_metadata=\"{metadata}\"")
            };
            ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized", message).with_challenge(challenge)
        }
        AuthError::InsufficientScope(needed) => ApiError::new(
            StatusCode::FORBIDDEN,
            "insufficient-scope",
            format!("this token lacks the {needed} permission"),
        )
        .with_challenge(format!(
            "Bearer error=\"insufficient_scope\", scope=\"{needed}\", resource_metadata=\"{metadata}\""
        )),
        AuthError::Invalid(message) => ApiError::bad_request(message),
        AuthError::NotFound(what) => {
            ApiError::new(StatusCode::NOT_FOUND, "not-found", format!("{what} not found"))
        }
        AuthError::Upstream(message) => {
            ApiError::new(StatusCode::BAD_GATEWAY, "upstream", message)
        }
        AuthError::Database(err) => {
            tracing::error!(error = %err, "database error during authentication");
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "internal error; see server logs",
            )
        }
    }
}

/// Middleware of `/mcp`: authenticates the bearer token for the MCP resource and hands the
/// principal to the MCP service through the request extensions.
pub async fn mcp_bearer(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let authorization = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    match state
        .auth
        .authenticate(authorization.as_deref(), Resource::Mcp)
        .await
    {
        Ok(principal) => {
            request.extensions_mut().insert(principal);
            next.run(request).await
        }
        Err(err) => auth_error(&state.auth, Resource::Mcp, err).into_response(),
    }
}
