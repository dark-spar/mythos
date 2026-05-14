//! Scan trigger + status.
//!
//! State lives in [`ScanTracker`], an `Arc<RwLock<HashMap<Uuid,
//! ScanState>>>` on `ApiState`. It is in-memory only: a server restart
//! drops the per-library state back to `Idle`. A `scan_jobs` table is
//! deferred until we need history beyond "what's the most recent run."

use std::collections::HashMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use mythos_auth::{AdminUser, AuthUser};
use mythos_db::LibraryRepo;
use mythos_scan::ScanReport;

use crate::TmdbHandle;
use serde::Serialize;
use sqlx::SqlitePool;
use tokio::sync::RwLock;
use tracing::warn;
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum ScanState {
    Idle,
    Running {
        started_at: DateTime<Utc>,
    },
    Completed {
        started_at: DateTime<Utc>,
        finished_at: DateTime<Utc>,
        added: u32,
        updated: u32,
        removed: u64,
        enriched: u32,
        errors: Vec<String>,
        duration_ms: u64,
    },
}

#[derive(Debug, Clone, Default)]
pub struct ScanTracker {
    state: Arc<RwLock<HashMap<Uuid, ScanState>>>,
}

impl ScanTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn get(&self, library_id: Uuid) -> ScanState {
        self.state
            .read()
            .await
            .get(&library_id)
            .cloned()
            .unwrap_or(ScanState::Idle)
    }

    /// Atomically mark a library as running, unless one is already
    /// running. Returns `Some(started_at)` if this call claimed the
    /// slot, `None` if another scan is in flight.
    pub async fn try_start(&self, library_id: Uuid) -> Option<DateTime<Utc>> {
        let mut state = self.state.write().await;
        if let Some(ScanState::Running { .. }) = state.get(&library_id) {
            return None;
        }
        let started_at = Utc::now();
        state.insert(library_id, ScanState::Running { started_at });
        Some(started_at)
    }

    pub async fn complete(&self, library_id: Uuid, started_at: DateTime<Utc>, report: ScanReport) {
        let mut state = self.state.write().await;
        state.insert(
            library_id,
            ScanState::Completed {
                started_at,
                finished_at: Utc::now(),
                added: report.added,
                updated: report.updated,
                removed: report.removed,
                enriched: report.enriched,
                errors: report.errors,
                duration_ms: report.duration_ms,
            },
        );
    }
}

pub async fn start(
    State(pool): State<SqlitePool>,
    State(tracker): State<ScanTracker>,
    State(tmdb): State<TmdbHandle>,
    _user: AdminUser,
    Path(library_id): Path<Uuid>,
) -> ApiResult<(StatusCode, Json<ScanState>)> {
    let library = LibraryRepo::new(pool.clone())
        .find_by_id(library_id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found"))?;

    let started_at = match tracker.try_start(library_id).await {
        Some(t) => t,
        None => {
            // Already running — return current state without starting another.
            return Ok((StatusCode::OK, Json(tracker.get(library_id).await)));
        }
    };

    let tracker_for_task = tracker.clone();
    let pool_for_task = pool.clone();
    let tmdb_for_task = tmdb.clone();
    tokio::spawn(async move {
        // Snapshot the current TMDb client at scan start so the
        // scan uses whatever was configured when the user kicked it
        // off, even if the admin saves a new key mid-scan.
        let tmdb_client = tmdb_for_task.snapshot().await;
        let report =
            mythos_scan::scan_library(&pool_for_task, &library, tmdb_client.as_deref()).await;
        if !report.errors.is_empty() {
            warn!(
                library = %library.name,
                error_count = report.errors.len(),
                "scan finished with errors"
            );
        }
        tracker_for_task
            .complete(library_id, started_at, report)
            .await;
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(ScanState::Running { started_at }),
    ))
}

pub async fn status(
    State(tracker): State<ScanTracker>,
    _user: AuthUser,
    Path(library_id): Path<Uuid>,
) -> ApiResult<Json<ScanState>> {
    Ok(Json(tracker.get(library_id).await))
}
