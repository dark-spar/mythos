//! HTTP API surface.

pub mod auth;
pub mod error;
pub mod hls;
pub mod library;
pub mod movie;
pub mod play;
pub mod scan;
pub mod settings;
pub mod subtitles;

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::FromRef;
use axum::{
    Json, Router,
    routing::{get, post},
};
use mythos_auth::TokenConfig;
use mythos_db::SettingsRepo;
use mythos_db::settings::keys as setting_keys;
use mythos_meta::{TmdbClient, TmdbConfig};
use serde::Serialize;
use sqlx::SqlitePool;
use tokio::sync::RwLock;

pub use error::{ApiError, ApiResult};
pub use hls::HlsHandle;
pub use scan::ScanTracker;

#[derive(Clone, Debug)]
pub struct CookieConfig {
    pub secure: bool,
}

/// Directory where TMDb-downloaded poster JPEGs live. Wrapped in a
/// newtype so [`axum::extract::FromRef`] can pick it out of `ApiState`
/// without colliding with other `PathBuf` state.
#[derive(Clone, Debug)]
pub struct PostersDir(pub PathBuf);

/// Directory where extracted WebVTT subtitle caches live. Lazy
/// write-through cache: first request triggers extraction + write;
/// subsequent requests serve from disk.
#[derive(Clone, Debug)]
pub struct SubtitlesDir(pub PathBuf);

/// Optional TMDb client wrapped in a runtime-mutable handle. `None`
/// inside means no API key is currently configured and the scanner
/// skips enrichment. The settings PUT handler swaps the inner
/// value live so a freshly-saved key takes effect on the very next
/// scan without a server restart.
#[derive(Clone, Default)]
pub struct TmdbHandle(pub Arc<RwLock<Option<Arc<TmdbClient>>>>);

impl TmdbHandle {
    pub fn new(client: Option<Arc<TmdbClient>>) -> Self {
        Self(Arc::new(RwLock::new(client)))
    }

    pub async fn snapshot(&self) -> Option<Arc<TmdbClient>> {
        self.0.read().await.clone()
    }

    pub async fn replace(&self, client: Option<Arc<TmdbClient>>) {
        *self.0.write().await = client;
    }
}

/// Source of truth for the API key the TMDb client should use.
/// Env var wins so the systemd-unit-and-done deployment story stays
/// intact even when an operator also pokes the value in the
/// browser UI.
pub async fn resolve_tmdb_api_key(pool: &SqlitePool) -> Option<String> {
    if let Ok(value) = std::env::var("MYTHOS_TMDB_API_KEY") {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    SettingsRepo::new(pool.clone())
        .get(setting_keys::TMDB_API_KEY)
        .await
        .ok()
        .flatten()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

pub fn build_tmdb_client(api_key: &str, posters_dir: PathBuf) -> Arc<TmdbClient> {
    Arc::new(TmdbClient::new(TmdbConfig::new(
        api_key.to_string(),
        posters_dir,
    )))
}

#[derive(Clone, FromRef)]
pub struct ApiState {
    pub db: SqlitePool,
    pub token: TokenConfig,
    pub cookies: CookieConfig,
    pub scans: ScanTracker,
    pub tmdb: TmdbHandle,
    pub posters_dir: PostersDir,
    pub subtitles_dir: SubtitlesDir,
    pub hls: HlsHandle,
}

#[derive(Debug, Serialize)]
struct Health {
    status: &'static str,
    version: &'static str,
}

pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/auth/status", get(auth::status))
        .route("/api/auth/register", post(auth::register))
        .route("/api/auth/login", post(auth::login))
        .route("/api/auth/logout", post(auth::logout))
        .route("/api/users/me", get(auth::me))
        .route("/api/libraries", get(library::list).post(library::create))
        .route(
            "/api/libraries/{id}",
            get(library::get_one).delete(library::delete),
        )
        .route(
            "/api/libraries/{id}/scan",
            post(scan::start).get(scan::status),
        )
        .route("/api/libraries/{id}/movies", get(movie::list))
        .route("/api/movies/{id}", get(movie::get_one))
        .route("/api/movies/{id}/poster", get(movie::poster))
        .route("/api/movies/{id}/stream", get(movie::stream))
        .route("/api/movies/{id}/play", post(play::play))
        .route("/api/movies/{id}/hls/master.m3u8", get(hls::master))
        .route(
            "/api/movies/{id}/hls/{variant}/{filename}",
            get(hls::variant_file),
        )
        .route("/api/movies/{id}/hls", axum::routing::delete(hls::stop))
        .route(
            "/api/movies/{id}/subtitles/{sub_id}/vtt",
            get(subtitles::webvtt),
        )
        .route(
            "/api/movies/{id}/progress",
            axum::routing::put(movie::put_progress),
        )
        .route(
            "/api/settings",
            get(settings::get_settings).put(settings::put_settings),
        )
        .with_state(state)
}

async fn health() -> Json<Health> {
    Json(Health {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}
