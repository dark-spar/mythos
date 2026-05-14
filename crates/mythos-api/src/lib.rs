//! HTTP API surface.

pub mod auth;
pub mod error;
pub mod hls;
pub mod library;
pub mod movie;
pub mod scan;

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::FromRef;
use axum::{
    Json, Router,
    routing::{get, post},
};
use mythos_auth::TokenConfig;
use mythos_meta::TmdbClient;
use serde::Serialize;
use sqlx::SqlitePool;

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

/// Optional TMDb client. `None` means no API key was configured and the
/// scanner skips enrichment.
#[derive(Clone, Default)]
pub struct TmdbHandle(pub Option<Arc<TmdbClient>>);

#[derive(Clone, FromRef)]
pub struct ApiState {
    pub db: SqlitePool,
    pub token: TokenConfig,
    pub cookies: CookieConfig,
    pub scans: ScanTracker,
    pub tmdb: TmdbHandle,
    pub posters_dir: PostersDir,
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
        .route("/api/movies/{id}/hls/{filename}", get(hls::hls))
        .route(
            "/api/movies/{id}/hls",
            axum::routing::delete(hls::stop),
        )
        .route(
            "/api/movies/{id}/progress",
            axum::routing::put(movie::put_progress),
        )
        .with_state(state)
}

async fn health() -> Json<Health> {
    Json(Health {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}
