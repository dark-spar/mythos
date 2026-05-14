//! Cross-cutting "user activity" queries that span multiple per-kind
//! tables. The first inhabitant is continue-watching, which unions
//! `movie_progress` and `episode_progress` for the home-page row.

use chrono::{DateTime, Utc};
use mythos_core::{ContinueWatchingEpisode, ContinueWatchingItem, ContinueWatchingMovie};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::{DbError, Result};

/// Minimum position (in seconds) for an item to count as
/// "in-progress". Filters out accidental clicks and the few-second
/// auto-saves the player issues on metadata load.
pub const MIN_POSITION_SECONDS: f64 = 60.0;

/// Fraction of the runtime past which we consider the item watched
/// and stop surfacing it in continue-watching. 0.95 leaves a small
/// trailing credits window before falling off the list.
pub const WATCHED_FRACTION: f64 = 0.95;

#[derive(Debug, Clone)]
pub struct ActivityRepo {
    pool: SqlitePool,
}

impl ActivityRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Most-recently-watched in-progress items for `user_id`, merged
    /// across movies and episodes and capped at `limit`.
    ///
    /// Implementation: two queries (one per kind), each returning at
    /// most `limit` rows pre-sorted by `updated_at` desc. Merge in
    /// Rust and take the top `limit`. This is correct (each per-kind
    /// query exhausts the candidates for that kind that could
    /// possibly land in the merged top-N) and avoids wrangling a
    /// heterogenous SQL UNION.
    pub async fn continue_watching(
        &self,
        user_id: Uuid,
        limit: i64,
    ) -> Result<Vec<ContinueWatchingItem>> {
        let limit = limit.max(0);
        let movies = self.continue_watching_movies(user_id, limit).await?;
        let episodes = self.continue_watching_episodes(user_id, limit).await?;

        let mut merged: Vec<ContinueWatchingItem> = movies
            .into_iter()
            .map(ContinueWatchingItem::Movie)
            .chain(episodes.into_iter().map(ContinueWatchingItem::Episode))
            .collect();
        merged.sort_by_key(|item| std::cmp::Reverse(item.updated_at()));
        let n = usize::try_from(limit).unwrap_or(0);
        merged.truncate(n);
        Ok(merged)
    }

    async fn continue_watching_movies(
        &self,
        user_id: Uuid,
        limit: i64,
    ) -> Result<Vec<ContinueWatchingMovie>> {
        let rows: Vec<MovieRow> = sqlx::query_as(
            "SELECT m.id, m.library_id, m.title, m.year, m.poster_url, \
                    mp.position_seconds, mp.duration_seconds, mp.updated_at \
             FROM movie_progress mp \
             JOIN movies m ON m.id = mp.movie_id \
             WHERE mp.user_id = ? \
               AND mp.position_seconds > ? \
               AND mp.duration_seconds > 0 \
               AND mp.position_seconds < mp.duration_seconds * ? \
             ORDER BY mp.updated_at DESC \
             LIMIT ?",
        )
        .bind(user_id.to_string())
        .bind(MIN_POSITION_SECONDS)
        .bind(WATCHED_FRACTION)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(MovieRow::into_item).collect()
    }

    async fn continue_watching_episodes(
        &self,
        user_id: Uuid,
        limit: i64,
    ) -> Result<Vec<ContinueWatchingEpisode>> {
        let rows: Vec<EpisodeRow> = sqlx::query_as(
            "SELECT e.id, ser.library_id, ser.id AS series_id, ser.title AS series_title, \
                    ser.poster_url, se.season_number, e.episode_number, \
                    e.title AS episode_title, e.still_url, \
                    ep.position_seconds, ep.duration_seconds, ep.updated_at \
             FROM episode_progress ep \
             JOIN episodes e ON e.id = ep.episode_id \
             JOIN seasons se ON se.id = e.season_id \
             JOIN series ser ON ser.id = se.series_id \
             WHERE ep.user_id = ? \
               AND ep.position_seconds > ? \
               AND ep.duration_seconds > 0 \
               AND ep.position_seconds < ep.duration_seconds * ? \
             ORDER BY ep.updated_at DESC \
             LIMIT ?",
        )
        .bind(user_id.to_string())
        .bind(MIN_POSITION_SECONDS)
        .bind(WATCHED_FRACTION)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(EpisodeRow::into_item).collect()
    }
}

#[derive(sqlx::FromRow)]
struct MovieRow {
    id: String,
    library_id: String,
    title: String,
    year: Option<i64>,
    poster_url: Option<String>,
    position_seconds: f64,
    duration_seconds: f64,
    updated_at: String,
}

impl MovieRow {
    fn into_item(self) -> Result<ContinueWatchingMovie> {
        Ok(ContinueWatchingMovie {
            id: parse_uuid("movie id", &self.id)?,
            library_id: parse_uuid("library_id", &self.library_id)?,
            title: self.title,
            year: self.year,
            poster_url: self.poster_url,
            position_seconds: self.position_seconds,
            duration_seconds: self.duration_seconds,
            updated_at: parse_ts("movie_progress updated_at", &self.updated_at)?,
        })
    }
}

#[derive(sqlx::FromRow)]
struct EpisodeRow {
    id: String,
    library_id: String,
    series_id: String,
    series_title: String,
    poster_url: Option<String>,
    season_number: i64,
    episode_number: i64,
    episode_title: Option<String>,
    still_url: Option<String>,
    position_seconds: f64,
    duration_seconds: f64,
    updated_at: String,
}

impl EpisodeRow {
    fn into_item(self) -> Result<ContinueWatchingEpisode> {
        Ok(ContinueWatchingEpisode {
            id: parse_uuid("episode id", &self.id)?,
            library_id: parse_uuid("library_id", &self.library_id)?,
            series_id: parse_uuid("series_id", &self.series_id)?,
            series_title: self.series_title,
            season_number: self.season_number,
            episode_number: self.episode_number,
            episode_title: self.episode_title,
            poster_url: self.poster_url,
            still_url: self.still_url,
            position_seconds: self.position_seconds,
            duration_seconds: self.duration_seconds,
            updated_at: parse_ts("episode_progress updated_at", &self.updated_at)?,
        })
    }
}

fn parse_uuid(label: &str, s: &str) -> Result<Uuid> {
    Uuid::parse_str(s).map_err(|err| DbError::Decode(format!("invalid {label}: {err}")))
}

fn parse_ts(label: &str, s: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|err| DbError::Decode(format!("invalid {label}: {err}")))
}
