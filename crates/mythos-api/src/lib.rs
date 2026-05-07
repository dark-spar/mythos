//! HTTP API surface. Phase 0 ships only the health endpoint; resource routers
//! land in subsequent phases.

use axum::{Json, Router, routing::get};
use serde::Serialize;
use sqlx::SqlitePool;

#[derive(Debug, Clone)]
pub struct ApiState {
    pub db: SqlitePool,
}

#[derive(Debug, Serialize)]
struct Health {
    status: &'static str,
    version: &'static str,
}

pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .with_state(state)
}

async fn health() -> Json<Health> {
    Json(Health {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}
