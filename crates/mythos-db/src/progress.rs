//! Per-user playback resume points.
//!
//! Last-write-wins: a second concurrent client (e.g. two tabs) on the
//! same user+movie simply overwrites — acceptable for a self-hosted
//! single-user-per-account model.

use chrono::{DateTime, Utc};
use mythos_core::WatchProgress;
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::{DbError, Result};

#[derive(Debug, Clone)]
pub struct ProgressRepo {
    pool: SqlitePool,
}

#[derive(sqlx::FromRow)]
struct ProgressRow {
    position_seconds: f64,
    duration_seconds: f64,
    updated_at: String,
}

impl ProgressRow {
    fn into_progress(self) -> Result<WatchProgress> {
        Ok(WatchProgress {
            position_seconds: self.position_seconds,
            duration_seconds: self.duration_seconds,
            updated_at: DateTime::parse_from_rfc3339(&self.updated_at)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|err| DbError::Decode(format!("invalid progress timestamp: {err}")))?,
        })
    }
}

impl ProgressRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn find(&self, user_id: Uuid, movie_id: Uuid) -> Result<Option<WatchProgress>> {
        let row: Option<ProgressRow> = sqlx::query_as(
            "SELECT position_seconds, duration_seconds, updated_at \
             FROM movie_progress WHERE user_id = ? AND movie_id = ?",
        )
        .bind(user_id.to_string())
        .bind(movie_id.to_string())
        .fetch_optional(&self.pool)
        .await?;
        row.map(ProgressRow::into_progress).transpose()
    }

    pub async fn upsert(
        &self,
        user_id: Uuid,
        movie_id: Uuid,
        position_seconds: f64,
        duration_seconds: f64,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO movie_progress \
               (user_id, movie_id, position_seconds, duration_seconds) \
             VALUES (?, ?, ?, ?) \
             ON CONFLICT (user_id, movie_id) DO UPDATE SET \
               position_seconds = excluded.position_seconds, \
               duration_seconds = excluded.duration_seconds, \
               updated_at       = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
        )
        .bind(user_id.to_string())
        .bind(movie_id.to_string())
        .bind(position_seconds)
        .bind(duration_seconds)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
