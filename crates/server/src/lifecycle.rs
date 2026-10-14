//! Process lifecycle: the `serve` and `migrate` commands and shutdown.

use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;
use tokio::sync::watch;

use crate::clock::SystemClock;
use crate::config::Config;
use crate::http::{AppState, router};
use crate::mastodon::{ClientSettings, MastodonClient};
use crate::poller::Poller;
use crate::store::{self, PoolSettings, Store};

/// The time for active requests after a stop signal. Railway waits
/// `RAILWAY_DEPLOYMENT_DRAINING_SECONDS` (30 s) before SIGKILL.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

/// The time for the poller task and the pool to stop after the server.
const STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// The deadline of the `migrate` command.
const MIGRATE_DEADLINE: Duration = Duration::from_secs(120);

/// Applies the pending migrations and exits. Railpack runs this before
/// `serve` in the start command (STACK.md §15).
///
/// # Errors
///
/// Returns a fixed stage name when a step fails.
pub async fn migrate(config: &Config) -> Result<(), &'static str> {
    let pool = store::connect_lazy(&config.database_url, PoolSettings::default())
        .map_err(|_| "database_url")?;
    let result = tokio::time::timeout(MIGRATE_DEADLINE, store::migrate(&pool)).await;
    pool.close().await;
    match result {
        Ok(Ok(())) => {
            tracing::info!(stage = "migrate", "migrations are up to date");
            Ok(())
        }
        Ok(Err(_)) | Err(_) => Err("migrate"),
    }
}

/// Serves HTTP and polls Mastodon until SIGTERM or SIGINT.
///
/// # Errors
///
/// Returns a fixed stage name when start-up or serving fails.
pub async fn serve(config: Config) -> Result<(), &'static str> {
    let pool = store::connect_lazy(&config.database_url, PoolSettings::default())
        .map_err(|_| "database_url")?;
    let store = Store::new(pool.clone());
    let client = MastodonClient::new(
        &config.mastodon_base_url,
        &config.mastodon_access_token,
        ClientSettings::default(),
    )
    .map_err(|_| "http_client")?;
    let state = AppState {
        store: store.clone(),
        feed: Arc::new(config.feed.clone()),
        feed_limit: config.feed_limit,
        version: config.version.as_deref().map(Arc::from),
    };
    let app = router(state, &config.public_dir);

    let listener = TcpListener::bind(config.bind).await.map_err(|_| "bind")?;
    let bound = listener.local_addr().map_err(|_| "bind")?;
    tracing::info!(
        bind = bound.to_string(),
        version = config.version.as_deref().unwrap_or("unknown"),
        "service started"
    );

    let (stop, stopped) = watch::channel(false);
    let poller = Poller::new(client, store, Arc::new(SystemClock));
    let poller_task = tokio::spawn(poller.run(config.cron_schedule.clone(), stopped.clone()));

    let shutdown = async move {
        shutdown_signal().await;
        tracing::info!(stage = "shutdown", "shutdown started");
        stop.send_replace(true);
    };
    let server = axum::serve(listener, app).with_graceful_shutdown(shutdown);
    let mut grace = stopped.clone();
    let outcome = tokio::select! {
        result = server.into_future() => result.map_err(|_| "serve"),
        () = async {
            if grace.wait_for(|stop| *stop).await.is_ok() {
                tokio::time::sleep(SHUTDOWN_GRACE).await;
            } else {
                std::future::pending::<()>().await;
            }
        } => {
            tracing::warn!(stage = "shutdown", "the shutdown grace period ended with active requests");
            Ok(())
        }
    };

    // The poller stops at its next await point. A sync that it stops writes
    // nothing, because the upsert transaction rolls back.
    let mut poller_task = poller_task;
    if tokio::time::timeout(STOP_TIMEOUT, &mut poller_task)
        .await
        .is_err()
    {
        poller_task.abort();
        tracing::warn!(stage = "shutdown", "the poller did not stop in time");
    }
    if tokio::time::timeout(STOP_TIMEOUT, pool.close())
        .await
        .is_err()
    {
        tracing::warn!(
            stage = "shutdown",
            "the database pool did not close in time"
        );
    }
    tracing::info!(stage = "shutdown", "service stopped");
    outcome
}

/// Waits for SIGTERM or SIGINT. If a handler cannot be installed, that
/// signal is logged and ignored, and the other signal still works.
async fn shutdown_signal() {
    let interrupt = async {
        if tokio::signal::ctrl_c().await.is_err() {
            tracing::error!(
                stage = "signal",
                code = "sigint",
                "the SIGINT handler could not be installed"
            );
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        } else {
            tracing::error!(
                stage = "signal",
                code = "sigterm",
                "the SIGTERM handler could not be installed"
            );
            std::future::pending::<()>().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = interrupt => {}
        () = terminate => {}
    }
}
