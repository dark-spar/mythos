use anyhow::{Context, Result};
use axum::Router;
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tokio::signal;
use tower_http::compression::CompressionLayer;
use tower_http::trace::TraceLayer;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

mod config;
mod web;

use config::Config;

#[tokio::main]
async fn main() -> Result<()> {
    let cfg = Config::load().context("loading configuration")?;
    init_tracing(&cfg.log_filter);

    info!(
        version = env!("CARGO_PKG_VERSION"),
        listen = %cfg.listen,
        data_dir = %cfg.data_dir.display(),
        "mythos starting"
    );

    let pool = mythos_db::open(&cfg.db_path())
        .await
        .with_context(|| format!("opening database at {}", cfg.db_path().display()))?;
    mythos_db::migrate(&pool)
        .await
        .context("running database migrations")?;

    let app = build_app(pool);

    let listener = TcpListener::bind(cfg.listen)
        .await
        .with_context(|| format!("binding to {}", cfg.listen))?;
    let local = listener.local_addr().unwrap_or(cfg.listen);
    info!(addr = %local, "ready");

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .context("server crashed")?;

    info!("shutdown complete");
    Ok(())
}

fn build_app(db: mythos_db::SqlitePool) -> Router {
    let api = mythos_api::router(mythos_api::ApiState { db });
    Router::new()
        .merge(api)
        .fallback(web::handler)
        .layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http())
}

fn init_tracing(filter: &str) {
    let env_filter = EnvFilter::try_new(filter)
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(true)
        .compact()
        .init();
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(err) = signal::ctrl_c().await {
            warn!(?err, "failed to install ctrl_c handler");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match signal::unix::signal(signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(err) => warn!(?err, "failed to install SIGTERM handler"),
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => info!("received ctrl-c"),
        _ = terminate => info!("received SIGTERM"),
    }
}
