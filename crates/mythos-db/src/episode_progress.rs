//! Per-user playback resume points for TV episodes.
//!
//! Parallel to [`crate::ProgressRepo`] (which owns the `movie_progress`
//! table); the API surface stays kind-specific until Phase 3 unifies
//! enough of the playback flow to make consolidation worth a migration.

use chrono::{DateTime, Utc};
use mythos_core::EpisodeProgress;
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::{DbError, Result};

#[derive(Debug, Clone)]
pub struct EpisodeProgressRepo {
    pool: SqlitePool,
}

#[derive(sqlx::FromRow)]
struct ProgressRow {
    position_seconds: f64,
    duration_seconds: f64,
    updated_at: String,
}

impl ProgressRow {
    fn into_progress(self) -> Result<EpisodeProgress> {
        Ok(EpisodeProgress {
            position_seconds: self.position_seconds,
            duration_seconds: self.duration_seconds,
            updated_at: DateTime::parse_from_rfc3339(&self.updated_at)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|err| {
                    DbError::Decode(format!("invalid episode_progress timestamp: {err}"))
                })?,
        })
    }
}

impl EpisodeProgressRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn find(&self, user_id: Uuid, episode_id: Uuid) -> Result<Option<EpisodeProgress>> {
        let row: Option<ProgressRow> = sqlx::query_as(
            "SELECT position_seconds, duration_seconds, updated_at \
             FROM episode_progress WHERE user_id = ? AND episode_id = ?",
        )
        .bind(user_id.to_string())
        .bind(episode_id.to_string())
        .fetch_optional(&self.pool)
        .await?;
        row.map(ProgressRow::into_progress).transpose()
    }

    pub async fn upsert(
        &self,
        user_id: Uuid,
        episode_id: Uuid,
        position_seconds: f64,
        duration_seconds: f64,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO episode_progress \
               (user_id, episode_id, position_seconds, duration_seconds) \
             VALUES (?, ?, ?, ?) \
             ON CONFLICT (user_id, episode_id) DO UPDATE SET \
               position_seconds = excluded.position_seconds, \
               duration_seconds = excluded.duration_seconds, \
               updated_at       = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
        )
        .bind(user_id.to_string())
        .bind(episode_id.to_string())
        .bind(position_seconds)
        .bind(duration_seconds)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
