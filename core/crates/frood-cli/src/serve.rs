//! `frood serve`: the HTTP server.

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::Context;
use tracing_subscriber::EnvFilter;

use crate::lifecycle::Paths;

pub async fn run(paths: &Paths) -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let loaded = paths.load()?;
    for warning in &loaded.warnings {
        tracing::warn!("{warning}");
    }
    let addr: SocketAddr = loaded
        .config
        .server
        .listen
        .parse()
        .context("server.listen is not an address")?;

    let pool = frood_store::connect(&loaded.config.storage.url, Duration::from_secs(60))
        .await
        .context("database did not become reachable within 60 s")?;
    frood_store::migrate(&pool)
        .await
        .context("migrations failed")?;

    let policies = &loaded.config.policies;
    let auth = frood_auth::Auth::new(
        pool.clone(),
        frood_auth::AuthSettings::from_config(&loaded.config),
    );
    tracing::info!(issuer = %auth.settings().issuer, providers = auth.settings().providers.len(), "sign-in configured");
    let ledger = frood_ledger::Ledger::new(
        pool,
        frood_ledger::PolicyConfig {
            auto_accept_own_facts: policies.auto_accept_own_facts,
            entity_merge_threshold: policies.entity_merge_threshold,
        },
    );
    let app = frood_api::router(frood_api::AppState {
        ledger,
        auth,
        installation: loaded.config.installation.name.clone(),
    });
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("cannot listen on {addr}"))?;
    tracing::info!(%addr, installation = %loaded.config.installation.name, "frood is listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    tracing::info!("frood stopped");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}
