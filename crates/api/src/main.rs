//! The `api` binary. See the library crate for the router.

use anyhow::Context;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    platform_observability::init("api");
    let config = api::Config::from_env()?;

    let listener = tokio::net::TcpListener::bind(config.addr)
        .await
        .with_context(|| format!("bind {}", config.addr))?;
    tracing::info!(addr = %config.addr, "api listening");

    axum::serve(listener, api::build_router())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("serve")?;

    tracing::info!("api stopped");
    Ok(())
}

/// Resolves on Ctrl-C, or on SIGTERM on Unix (ECS sends SIGTERM).
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
