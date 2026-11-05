//! `worker`: the SaveSpace job runner.
//!
//! Design doc §8.2 and §8.6. Phase 0: the process starts and waits for a
//! shutdown signal. Phase 1 adds the job claim loop (`SKIP LOCKED`), the
//! outbox relay, and the scheduler.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    platform_observability::init("worker");
    tracing::info!("worker started; no job handlers are registered yet");

    shutdown_signal().await;

    tracing::info!("worker stopped");
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
