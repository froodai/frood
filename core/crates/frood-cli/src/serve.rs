//! `frood serve`: the HTTP server.

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::Context;
use tracing_subscriber::EnvFilter;

use crate::lifecycle::Paths;

pub async fn run(paths: &Paths) -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .json()
        .with_writer(frood_api::logs::Tee)
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

    let config = &loaded.config;
    let secrets = frood_config::Secrets::from_file(&paths.secrets())?;
    let frood_api::Assembly {
        mut state,
        modules,
        warnings,
    } = frood_api::assemble(config, &loaded.base_dir, pool, |name| secrets.get(name));
    state.config_sha256 = Some(loaded.sha256.clone());
    for warning in &warnings {
        tracing::warn!("{warning}");
    }
    let auth = state.auth.clone();
    let dispatch = state.dispatch.clone();
    tracing::info!(issuer = %auth.settings().issuer, providers = auth.settings().providers.len(), "sign-in configured");

    // Modules start in their own containers; find them and register their service tokens.
    {
        let modules = modules.clone();
        let auth = auth.clone();
        tokio::spawn(async move {
            modules
                .discover_all(Duration::from_secs(600), |name, manifest| {
                    let auth = auth.clone();
                    let token = modules.token(&name).map(str::to_owned);
                    async move {
                        match token {
                            Some(token) => {
                                if let Err(err) = auth
                                    .register_service_token(&name, &token, &manifest.frood_api)
                                    .await
                                {
                                    tracing::error!(module = %name, error = %err, "cannot register the module's token");
                                }
                            }
                            None => tracing::warn!(module = %name, secret = %frood_modhost::token_secret(&name), "no service token in the secrets; the module cannot call Frood"),
                        }
                    }
                })
                .await;
        });
    }

    let app = frood_api::router(state);
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let worker = tokio::spawn(dispatch.start(2, stopped));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("cannot listen on {addr}"))?;
    tracing::info!(%addr, installation = %config.installation.name, "frood is listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    let _ = stop.send(true);
    let _ = worker.await;
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
