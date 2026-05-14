//! `episodes` repository.

use chrono::{DateTime, Utc};
use mythos_core::{Episode, EpisodeNeighbor, NewEpisode};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::{DbError, Result};

#[derive(sqlx::FromRow)]
struct EpisodeRow {
    id: String,
    season_id: String,
    file_id: String,
    episode_number: i64,
    title: Option<String>,
    tmdb_id: Option<i64>,
    overview: Option<String>,
    still_url: Option<String>,
    air_date: Option<String>,
    created_at: String,
    updated_at: String,
}

impl EpisodeRow {
    fn into_episode(self) -> Result<Episode> {
        Ok(Episode {
            id: parse_uuid("episode id", &self.id)?,
            season_id: parse_uuid("episode season_id", &self.season_id)?,
            file_id: parse_uuid("episode file_id", &self.file_id)?,
            episode_number: self.episode_number,
            title: self.title,
            tmdb_id: self.tmdb_id,
            overview: self.overview,
            still_url: self.still_url,
            air_date: self.air_date,
            created_at: parse_ts("episode created_at", &self.created_at)?,
            updated_at: parse_ts("episode updated_at", &self.updated_at)?,
        })
    }
}

#[derive(sqlx::FromRow)]
struct NeighborRow {
    id: String,
    season_number: i64,
    episode_number: i64,
    title: Option<String>,
}

impl NeighborRow {
    fn into_neighbor(self) -> Result<EpisodeNeighbor> {
        Ok(EpisodeNeighbor {
            id: parse_uuid("episode neighbor id", &self.id)?,
            season_number: self.season_number,
            episode_number: self.episode_number,
            title: self.title,
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

#[derive(Debug, Clone)]
pub struct EpisodeRepo {
    pool: SqlitePool,
}

impl EpisodeRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Insert an episode or refresh identifier-derived fields if one
    /// already exists for this file. `file_id` is the natural identity
    /// (same shape as `MovieRepo::upsert`); a re-scan that re-identifies
    /// the same file under a different season/episode follows the file.
    ///
    /// Preserves `tmdb_id`, `overview`, `still_url`, `air_date`, and any
    /// TMDb-derived `title` so enrichment isn't clobbered. The
    /// filename-derived `title` only fills in when no enrichment exists.
    pub async fn upsert(&self, new: NewEpisode) -> Result<Episode> {
        let new_id = Uuid::now_v7();

        let row: EpisodeRow = sqlx::query_as(
            "INSERT INTO episodes \
               (id, season_id, file_id, episode_number, title) \
             VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT (file_id) DO UPDATE SET \
               season_id      = excluded.season_id, \
               episode_number = excluded.episode_number, \
               title          = COALESCE(episodes.title, excluded.title), \
               updated_at     = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
             RETURNING id, season_id, file_id, episode_number, title, \
               tmdb_id, overview, still_url, air_date, created_at, updated_at",
        )
        .bind(new_id.to_string())
        .bind(new.season_id.to_string())
        .bind(new.file_id.to_string())
        .bind(new.episode_number)
        .bind(new.title.as_deref())
        .fetch_one(&self.pool)
        .await?;

        row.into_episode()
    }

    pub async fn find_by_id(&self, id: Uuid) -> Result<Option<Episode>> {
        let row: Option<EpisodeRow> = sqlx::query_as(
            "SELECT id, season_id, file_id, episode_number, title, \
                    tmdb_id, overview, still_url, air_date, created_at, updated_at \
             FROM episodes WHERE id = ?",
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await?;
        row.map(EpisodeRow::into_episode).transpose()
    }

    /// Return `(previous, next)` for `episode_id` — adjacent episodes
    /// in the same series, ordered by `(season_number,
    /// episode_number)`. Crosses season boundaries at the ends.
    /// Returns `(None, None)` if the episode doesn't exist or is the
    /// only one in its series.
    pub async fn find_neighbors(
        &self,
        episode_id: Uuid,
    ) -> Result<(Option<EpisodeNeighbor>, Option<EpisodeNeighbor>)> {
        let series_row: Option<(String,)> = sqlx::query_as(
            "SELECT s.series_id FROM episodes e \
             JOIN seasons s ON s.id = e.season_id \
             WHERE e.id = ?",
        )
        .bind(episode_id.to_string())
        .fetch_optional(&self.pool)
        .await?;
        let Some((series_id,)) = series_row else {
            return Ok((None, None));
        };

        let rows: Vec<NeighborRow> = sqlx::query_as(
            "SELECT e.id, s.season_number, e.episode_number, e.title \
             FROM episodes e \
             JOIN seasons s ON s.id = e.season_id \
             WHERE s.series_id = ? \
             ORDER BY s.season_number ASC, e.episode_number ASC",
        )
        .bind(series_id)
        .fetch_all(&self.pool)
        .await?;

        let neighbors: Vec<EpisodeNeighbor> = rows
            .into_iter()
            .map(NeighborRow::into_neighbor)
            .collect::<Result<_>>()?;
        let Some(i) = neighbors.iter().position(|e| e.id == episode_id) else {
            return Ok((None, None));
        };
        let prev = i.checked_sub(1).and_then(|p| neighbors.get(p).cloned());
        let next = neighbors.get(i + 1).cloned();
        Ok((prev, next))
    }

    pub async fn list_by_season(&self, season_id: Uuid) -> Result<Vec<Episode>> {
        let rows: Vec<EpisodeRow> = sqlx::query_as(
            "SELECT id, season_id, file_id, episode_number, title, \
                    tmdb_id, overview, still_url, air_date, created_at, updated_at \
             FROM episodes WHERE season_id = ? \
             ORDER BY episode_number ASC",
        )
        .bind(season_id.to_string())
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(EpisodeRow::into_episode).collect()
    }

    /// Apply TMDb metadata to an episode row. Bumps `updated_at`.
    /// `title` and `air_date` overwrite when supplied; existing values
    /// are preserved when the TMDb side has nothing to say.
    pub async fn apply_tmdb(
        &self,
        episode_id: Uuid,
        tmdb_id: i64,
        title: Option<&str>,
        overview: Option<&str>,
        still_url: Option<&str>,
        air_date: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE episodes SET \
               tmdb_id    = ?, \
               title      = COALESCE(?, title), \
               overview   = COALESCE(?, overview), \
               still_url  = COALESCE(?, still_url), \
               air_date   = COALESCE(?, air_date), \
               updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
             WHERE id = ?",
        )
        .bind(tmdb_id)
        .bind(title)
        .bind(overview)
        .bind(still_url)
        .bind(air_date)
        .bind(episode_id.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
