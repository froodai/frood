//! HTTP surface of the Frood core (docs/12_api_mcp_auth.md): health, `/v1` over the Ledger,
//! the OAuth endpoints and `/mcp`, all on one origin.

mod account;
mod admin;
mod attachments;
mod attention;
mod auth;
mod capture;
mod channels;
mod connections;
mod console;
mod error;
mod inbox;
mod ledger;
mod links;
pub mod logs;
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
    /// The installation's spheres (docs/26 §3); empty when they are off.
    pub spheres: std::sync::Arc<Vec<frood_config::Sphere>>,
    /// Names of the accounts in the config, for the console's choice of who pays.
    pub accounts: std::sync::Arc<Vec<String>>,
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
    let (objects, max_mb, objects_warning) = files_store(config, base_dir);
    let ledger = Ledger::new(
        pool.clone(),
        frood_ledger::PolicyConfig {
            auto_accept_own_facts: policies.auto_accept_own_facts,
            entity_merge_threshold: policies.entity_merge_threshold,
        },
    )
    .with_spheres(config.spheres.iter().map(|s| s.id.clone()).collect())
    .with_objects(objects, max_mb);
    let auth = Auth::new(pool, frood_auth::AuthSettings::from_config(config));
    let gateway = frood_llm::Gateway::new(&config.accounts);
    let searcher = frood_search::Searcher::new(ledger.clone(), gateway.clone(), &config.search);
    let vault_key = token_of("FROOD_VAULT_KEY");
    let modules = frood_modhost::Modules::new(config, token_of);
    let api_accounts = gateway.api_accounts();
    let caps = frood_dispatch::Capabilities {
        api_accounts: &api_accounts,
        embeddings: searcher.embedding_model().is_some(),
        modules: config.modules.len(),
        base_dir,
    };
    let (jobs, mut warnings) = frood_dispatch::resolve(config, &caps);
    let job_base = frood_dispatch::connections::JobBase {
        config: config.clone(),
        api_accounts: api_accounts.clone(),
        embeddings: caps.embeddings,
        modules: caps.modules,
        base_dir: base_dir.to_owned(),
    };
    warnings.extend(objects_warning);
    let mut dispatch = frood_dispatch::Dispatch::new(
        ledger.clone(),
        auth.clone(),
        searcher.clone(),
        gateway,
        modules.clone(),
        jobs,
    )
    .with_sources(config.sources.clone())
    .with_vault(vault_key)
    .with_attention(config.attention.clone())
    .with_spheres(config.spheres.clone())
    .with_job_base(job_base);
    if let Some(backup) = &config.backup {
        dispatch = dispatch.with_backup(frood_dispatch::BackupSetup {
            settings: backup.clone(),
            installation: config.installation.name.clone(),
            database_url: frood_modhost::database_url(config),
            config_yaml: None,
            secrets_env: None,
            objects: matches!(config.storage.objects, frood_config::Objects::Disk { .. })
                .then(|| OBJECTS_FOR_BACKUP.to_owned()),
        });
    }
    Assembly {
        state: AppState {
            ledger,
            auth,
            searcher,
            dispatch,
            installation: config.installation.name.clone(),
            spheres: std::sync::Arc::new(config.spheres.clone()),
            accounts: std::sync::Arc::new(config.accounts.keys().cloned().collect()),
            config_sha256: None,
        },
        modules,
        warnings,
    }
}

/// Inside the server's container, the files folder `frood apply` mounts.
pub const OBJECTS_IN_CONTAINER: &str = "/var/lib/frood/objects";
/// Where the backup module sees the files folder `frood apply` mounts into it.
pub const OBJECTS_FOR_BACKUP: &str = "/frood/objects";
/// What `frood apply` leaves in a files folder it made, so the server knows the folder lasts.
pub const OBJECTS_MARK: &str = ".frood-objects";

/// The files store as the machine with `frood.yaml` reaches it — the folder itself, not the
/// server container's mount — for `frood export` and `frood restore`.
pub fn files_on_host(
    config: &frood_config::Config,
    base_dir: &std::path::Path,
) -> Result<frood_store::objects::Objects, String> {
    use frood_store::objects::{Objects, S3};
    match &config.storage.objects {
        frood_config::Objects::Database { .. } => Ok(Objects::Database),
        frood_config::Objects::Disk { path, .. } => Ok(Objects::Disk {
            root: base_dir.join(path.as_deref().unwrap_or(".frood/objects")),
        }),
        frood_config::Objects::S3 {
            endpoint,
            bucket,
            region,
            prefix,
            access_key_id,
            secret_access_key,
            path_style,
            ..
        } => S3::new(
            endpoint,
            bucket,
            region.as_deref(),
            prefix.as_deref(),
            access_key_id,
            secret_access_key,
            *path_style,
        )
        .map(Objects::S3)
        .map_err(|e| e.to_string()),
    }
}

/// The files store the config names (docs/26 §2), its size limit, and why the database keeps
/// files instead when the store cannot be used.
fn files_store(
    config: &frood_config::Config,
    base_dir: &std::path::Path,
) -> (frood_store::objects::Objects, u32, Option<String>) {
    use frood_store::objects::{Objects, S3};
    let max_mb = config.storage.objects.max_mb();
    let in_database = |why: String| (Objects::Database, max_mb.min(25), Some(why));
    match &config.storage.objects {
        frood_config::Objects::Database { .. } => (Objects::Database, max_mb, None),
        frood_config::Objects::Disk { path, .. } => {
            let root = match config.deploy.server {
                frood_config::ServerPlacement::Container => {
                    std::path::PathBuf::from(OBJECTS_IN_CONTAINER)
                }
                frood_config::ServerPlacement::Host => {
                    base_dir.join(path.as_deref().unwrap_or(".frood/objects"))
                }
            };
            // A folder the deployment did not mount would vanish with the container.
            if !root.join(OBJECTS_MARK).exists() {
                if config.deploy.server == frood_config::ServerPlacement::Container {
                    return in_database(format!(
                        "files stay in the database: {} is not the folder `frood apply` mounts; run `frood apply`",
                        root.display()
                    ));
                }
                if let Err(err) = std::fs::create_dir_all(&root)
                    .and_then(|()| std::fs::write(root.join(OBJECTS_MARK), b""))
                {
                    return in_database(format!(
                        "files stay in the database: cannot make {}: {err}",
                        root.display()
                    ));
                }
            }
            (Objects::Disk { root }, max_mb, None)
        }
        frood_config::Objects::S3 {
            endpoint,
            bucket,
            region,
            prefix,
            access_key_id,
            secret_access_key,
            path_style,
            ..
        } => match S3::new(
            endpoint,
            bucket,
            region.as_deref(),
            prefix.as_deref(),
            access_key_id,
            secret_access_key,
            *path_style,
        ) {
            Ok(s3) => (Objects::S3(s3), max_mb, None),
            Err(err) => in_database(format!("files stay in the database: {err}")),
        },
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
        .merge(attention::routes())
        .merge(capture::routes(state.ledger.max_file_bytes()))
        .merge(attachments::routes(state.ledger.max_file_bytes()))
        .merge(channels::routes())
        .merge(connections::routes())
        .merge(work::routes())
        .merge(inbox::routes())
        .merge(admin::routes())
        .merge(console::routes())
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
        // For links from channels to the console; only with a public address.
        "console": state.auth.settings().console_url(),
        // The largest file a capture or an attachment may carry.
        "max_file_mb": state.ledger.max_file_bytes() / 1_048_576,
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
            "version: 1\ninstallation: {name: test}\nstorage: {uses: postgres, url: 'postgres://x', objects: {uses: database}}\ndeploy: {server: host}\n",
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
        assert_eq!(
            body["max_file_mb"], 25,
            "the test keeps files in the database"
        );
    }
}
