//! HTTP surface of the Frood core (docs/12_api_mcp_auth.md): health, `/v1` over the Ledger,
//! the OAuth endpoints and `/mcp`, all on one origin.

mod account;
mod auth;
mod capture;
mod channels;
mod error;
mod ledger;
mod work;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router, middleware};
use frood_auth::Auth;
use frood_ledger::Ledger;
use serde_json::{Value, json};

pub use error::ApiError;

#[derive(Clone)]
pub struct AppState {
    pub ledger: Ledger,
    pub auth: Auth,
    pub searcher: frood_search::Searcher,
    pub dispatch: frood_dispatch::Dispatch,
    pub installation: String,
    /// SHA-256 of the config file the server started with; `frood apply` restarts the server
    /// when the file differs.
    pub config_sha256: Option<String>,
}

/// Everything the server runs, assembled from the config.
pub struct Assembly {
    pub state: AppState,
    pub modules: frood_modhost::Modules,
    /// Jobs that cannot run and why.
    pub warnings: Vec<String>,
}

/// Builds the server's parts: the Ledger with its policies, sign-in, the LLM gateway, search,
/// the modules (`token_of` reads their service tokens from the secrets) and the dispatcher
/// with the jobs the installation can run.
pub fn assemble(
    config: &frood_config::Config,
    base_dir: &std::path::Path,
    pool: frood_store::sqlx::PgPool,
    token_of: impl Fn(&str) -> Option<String>,
) -> Assembly {
    let policies = &config.policies;
    let ledger = Ledger::new(
        pool.clone(),
        frood_ledger::PolicyConfig {
            auto_accept_own_facts: policies.auto_accept_own_facts,
            entity_merge_threshold: policies.entity_merge_threshold,
        },
    );
    let auth = Auth::new(pool, frood_auth::AuthSettings::from_config(config));
    let gateway = frood_llm::Gateway::new(&config.accounts);
    let searcher = frood_search::Searcher::new(ledger.clone(), gateway.clone(), &config.search);
    let modules = frood_modhost::Modules::new(config, token_of);
    let api_accounts = gateway.api_accounts();
    let caps = frood_dispatch::Capabilities {
        api_accounts: &api_accounts,
        embeddings: searcher.embedding_model().is_some(),
        modules: config.modules.len(),
        base_dir,
    };
    let (jobs, warnings) = frood_dispatch::resolve(config, &caps);
    let dispatch = frood_dispatch::Dispatch::new(
        ledger.clone(),
        auth.clone(),
        searcher.clone(),
        gateway,
        modules.clone(),
        jobs,
    )
    .with_sources(
        config
            .sources
            .as_ref()
            .and_then(|sources| sources.as_object())
            .map(|sources| sources.keys().cloned().collect())
            .unwrap_or_default(),
    );
    Assembly {
        state: AppState {
            ledger,
            auth,
            searcher,
            dispatch,
            installation: config.installation.name.clone(),
            config_sha256: None,
        },
        modules,
        warnings,
    }
}

pub fn router(state: AppState) -> Router {
    let mcp = Router::new()
        .nest_service(
            "/mcp",
            frood_mcp::service(
                state.ledger.clone(),
                state.searcher.clone(),
                state.dispatch.clone(),
                allowed_hosts(&state.auth),
            ),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::mcp_bearer,
        ));
    let oauth = frood_auth::routes(state.auth.clone());
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/v1/version", get(version))
        .merge(ledger::routes())
        .merge(account::routes())
        .merge(capture::routes())
        .merge(channels::routes())
        .merge(work::routes())
        .with_state(state)
        .merge(mcp)
        .merge(oauth)
}

/// Hosts `/mcp` answers to (DNS-rebinding protection): the issuer's host and loopback.
fn allowed_hosts(auth: &Auth) -> Vec<String> {
    // Modules and agent containers reach the server as `frood` inside the Compose network, or
    // as `host.docker.internal` when the server runs on the host.
    let mut hosts = vec![
        "localhost".to_owned(),
        "127.0.0.1".to_owned(),
        "::1".to_owned(),
        "frood".to_owned(),
        "host.docker.internal".to_owned(),
    ];
    if let Some(host) = url::Url::parse(&auth.settings().issuer)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        && !hosts.contains(&host)
    {
        hosts.push(host);
    }
    hosts
}

/// Liveness: the process is up.
async fn healthz() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

/// Readiness: the database answers.
async fn readyz(State(state): State<AppState>) -> (StatusCode, Json<Value>) {
    match frood_store::ping(state.ledger.pool()).await {
        Ok(()) => (StatusCode::OK, Json(json!({ "status": "ready" }))),
        Err(err) => {
            tracing::warn!(error = %err, "readiness check failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "status": "not_ready", "reason": "database is not reachable" })),
            )
        }
    }
}

async fn version(State(state): State<AppState>) -> Json<Value> {
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "api": "v1",
        "installation": state.installation,
        "config_sha256": state.config_sha256,
    }))
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;

    fn app() -> Router {
        // Nothing listens on port 1, so readiness fails fast without a real database.
        let pool = frood_store::connect_lazy("postgres://frood:x@127.0.0.1:1/frood").unwrap();
        let config = frood_config::parse(
            "version: 1\ninstallation: {name: test}\nstorage: {uses: postgres, url: 'postgres://x'}\ndeploy: {server: host}\n",
            &frood_config::Secrets::from_pairs(Vec::<(String, String)>::new()),
        )
        .unwrap()
        .config;
        router(assemble(&config, std::path::Path::new("."), pool, |_| None).state)
    }

    async fn get_json(path: &str) -> (StatusCode, Value) {
        let response = app()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn healthz_is_ok_without_database() {
        assert_eq!(
            get_json("/healthz").await,
            (StatusCode::OK, json!({ "status": "ok" }))
        );
    }

    #[tokio::test]
    async fn readyz_reports_unreachable_database() {
        let (status, body) = get_json("/readyz").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["status"], "not_ready");
    }

    #[tokio::test]
    async fn v1_needs_a_bearer_token() {
        let response = app()
            .oneshot(Request::get("/v1/assertions").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let challenge = response.headers()["www-authenticate"].to_str().unwrap();
        assert_eq!(
            challenge,
            "Bearer resource_metadata=\"http://127.0.0.1:8080/.well-known/oauth-protected-resource/v1\""
        );
    }

    #[tokio::test]
    async fn authorization_server_metadata_is_public() {
        let (status, body) = get_json("/.well-known/oauth-authorization-server").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["issuer"], "http://127.0.0.1:8080");
        assert_eq!(body["client_id_metadata_document_supported"], true);
        assert_eq!(body["code_challenge_methods_supported"], json!(["S256"]));
    }

    #[tokio::test]
    async fn version_names_installation() {
        let (status, body) = get_json("/v1/version").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["installation"], "test");
        assert_eq!(body["api"], "v1");
    }
}
