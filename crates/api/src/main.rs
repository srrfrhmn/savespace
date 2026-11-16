//! The `api` binary. See the library crate for the router.

use anyhow::Context;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // `api openapi <path>` writes the OpenAPI document and exits (pnpm gen:api).
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("openapi") {
        let path = args.next().context("usage: api openapi <path>")?;
        let json = api::openapi().to_pretty_json().context("serialize")? + "\n";
        std::fs::write(&path, json).with_context(|| format!("write {path}"))?;
        return Ok(());
    }

    platform_observability::init("api");
    let config = api::Config::from_env()?;
    let db = platform_db::connect_lazy(&config.database_url, &platform_db::PoolConfig::new("api"))
        .context("parse DATABASE_URL")?;
    let mut state =
        api::AppState::new(db.clone(), config.public_origin.clone(), config.setup_code)?;
    if let Some((sync, plaid)) =
        aggregator_plaid::sync_service_from_env(db.clone()).map_err(anyhow::Error::msg)?
    {
        tracing::info!(env = ?plaid.env(), "bank connections on");
        state = state.with_sync(sync, plaid);
    } else {
        tracing::warn!("PLAID_CLIENT_ID or PLAID_SECRET not set; bank connections off");
    }
    let state = state.with_worker(api::worker_kick::WorkerKick::from_env());
    let router = api::build_router(state, config.web_dir.as_deref());

    let listener = tokio::net::TcpListener::bind(config.addr)
        .await
        .with_context(|| format!("bind {}", config.addr))?;
    tracing::info!(
        addr = %config.addr,
        origin = %config.public_origin,
        web = config.web_dir.is_some(),
        "api listening"
    );

    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("serve")?;

    db.close().await;
    tracing::info!("api stopped");
    Ok(())
}

/// Resolves on Ctrl-C, or on SIGTERM on Unix (Cloud Run sends SIGTERM).
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %e, "cannot listen for Ctrl-C");
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(e) => {
                tracing::error!(error = %e, "cannot listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    tracing::info!("shutdown signal received");
}
