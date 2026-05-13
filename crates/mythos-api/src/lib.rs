//! HTTP API surface.

pub mod auth;
pub mod error;
pub mod library;
pub mod movie;
pub mod scan;

use axum::extract::FromRef;
use axum::{
    Json, Router,
    routing::{get, post},
};
use mythos_auth::TokenConfig;
use serde::Serialize;
use sqlx::SqlitePool;

pub use error::{ApiError, ApiResult};
pub use scan::ScanTracker;

#[derive(Clone, Debug)]
pub struct CookieConfig {
    pub secure: bool,
}

#[derive(Clone, FromRef)]
pub struct ApiState {
    pub db: SqlitePool,
    pub token: TokenConfig,
    pub cookies: CookieConfig,
    pub scans: ScanTracker,
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
        .with_state(state)
}

async fn health() -> Json<Health> {
    Json(Health {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}
