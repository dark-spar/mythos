use anyhow::{Context, Result};
use axum::Router;
use mythos_api::{CookieConfig, HlsHandle, PostersDir, ScanTracker, TmdbHandle};
use mythos_auth::TokenConfig;
use mythos_meta::{TmdbClient, TmdbConfig};
use mythos_stream::TranscodeManager;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::signal;
use tower_http::compression::CompressionLayer;
use tower_http::trace::TraceLayer;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

mod config;
mod secret;
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

    let secret = secret::resolve(&cfg.data_dir).context("resolving JWT secret")?;
    let token = TokenConfig::new(
        Arc::<[u8]>::from(secret.as_slice()),
        Duration::from_secs(cfg.token_ttl_days * 24 * 60 * 60),
    );
    let cookies = CookieConfig {
        secure: cfg.cookie_secure,
    };

    let posters_dir = cfg.posters_dir();
    std::fs::create_dir_all(&posters_dir)
        .with_context(|| format!("creating posters dir at {}", posters_dir.display()))?;

    let transcode_dir = cfg.transcode_dir();
    std::fs::create_dir_all(&transcode_dir)
        .with_context(|| format!("creating transcode dir at {}", transcode_dir.display()))?;
    // Wipe leftovers from a prior run — segments from a crashed/killed
    // session are dead weight, and stale playlists confuse hls.js.
    if let Ok(entries) = std::fs::read_dir(&transcode_dir) {
        for entry in entries.flatten() {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
    let hls = HlsHandle(Some(TranscodeManager::new(transcode_dir)));

    // Periodic reaper for idle transcode sessions. Runs every minute,
    // kills any session with no segment-request activity in 5 minutes.
    if let Some(manager) = hls.0.clone() {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let reaped = manager.reap_idle().await;
                if reaped > 0 {
                    info!(reaped, "transcode sessions reaped");
                }
            }
        });
    }

    let tmdb = match cfg
        .tmdb_api_key
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(key) => {
            info!("TMDb enrichment enabled");
            TmdbHandle(Some(Arc::new(TmdbClient::new(TmdbConfig::new(
                key.to_string(),
                posters_dir.clone(),
            )))))
        }
        None => {
            info!("MYTHOS_TMDB_API_KEY not set; metadata enrichment disabled");
            TmdbHandle::default()
        }
    };

    let app = build_app(pool, token, cookies, tmdb, PostersDir(posters_dir), hls);

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

fn build_app(
    db: mythos_db::SqlitePool,
    token: TokenConfig,
    cookies: CookieConfig,
    tmdb: TmdbHandle,
    posters_dir: PostersDir,
    hls: HlsHandle,
) -> Router {
    let api = mythos_api::router(mythos_api::ApiState {
        db,
        token,
        cookies,
        scans: ScanTracker::new(),
        tmdb,
        posters_dir,
        hls,
    });
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
