//! `seasons` repository.

use chrono::{DateTime, Utc};
use mythos_core::{NewSeason, Season};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::{DbError, Result};

#[derive(sqlx::FromRow)]
struct SeasonRow {
    id: String,
    series_id: String,
    season_number: i64,
    title: Option<String>,
    tmdb_id: Option<i64>,
    overview: Option<String>,
    poster_url: Option<String>,
    created_at: String,
    updated_at: String,
}

impl SeasonRow {
    fn into_season(self) -> Result<Season> {
        Ok(Season {
            id: parse_uuid("season id", &self.id)?,
            series_id: parse_uuid("season series_id", &self.series_id)?,
            season_number: self.season_number,
            title: self.title,
            tmdb_id: self.tmdb_id,
            overview: self.overview,
            poster_url: self.poster_url,
            created_at: parse_ts("season created_at", &self.created_at)?,
            updated_at: parse_ts("season updated_at", &self.updated_at)?,
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
pub struct SeasonRepo {
    pool: SqlitePool,
}

impl SeasonRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Insert a season or return the existing row. Idempotent under
    /// re-scan: `(series_id, season_number)` is the natural key.
    /// Enrichment fields stay intact.
    pub async fn upsert(&self, new: NewSeason) -> Result<Season> {
        let new_id = Uuid::now_v7();

        let row: SeasonRow = sqlx::query_as(
            "INSERT INTO seasons (id, series_id, season_number) \
             VALUES (?, ?, ?) \
             ON CONFLICT (series_id, season_number) DO UPDATE SET \
               updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
             RETURNING id, series_id, season_number, title, \
               tmdb_id, overview, poster_url, created_at, updated_at",
        )
        .bind(new_id.to_string())
        .bind(new.series_id.to_string())
        .bind(new.season_number)
        .fetch_one(&self.pool)
        .await?;

        row.into_season()
    }

    pub async fn find_by_id(&self, id: Uuid) -> Result<Option<Season>> {
        let row: Option<SeasonRow> = sqlx::query_as(
            "SELECT id, series_id, season_number, title, \
                    tmdb_id, overview, poster_url, created_at, updated_at \
             FROM seasons WHERE id = ?",
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await?;
        row.map(SeasonRow::into_season).transpose()
    }

    pub async fn find_by_series_and_number(
        &self,
        series_id: Uuid,
        season_number: i64,
    ) -> Result<Option<Season>> {
        let row: Option<SeasonRow> = sqlx::query_as(
            "SELECT id, series_id, season_number, title, \
                    tmdb_id, overview, poster_url, created_at, updated_at \
             FROM seasons WHERE series_id = ? AND season_number = ?",
        )
        .bind(series_id.to_string())
        .bind(season_number)
        .fetch_optional(&self.pool)
        .await?;
        row.map(SeasonRow::into_season).transpose()
    }

    pub async fn list_by_series(&self, series_id: Uuid) -> Result<Vec<Season>> {
        let rows: Vec<SeasonRow> = sqlx::query_as(
            "SELECT id, series_id, season_number, title, \
                    tmdb_id, overview, poster_url, created_at, updated_at \
             FROM seasons WHERE series_id = ? \
             ORDER BY season_number ASC",
        )
        .bind(series_id.to_string())
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(SeasonRow::into_season).collect()
    }

    /// Apply TMDb metadata to a season row. Bumps `updated_at`.
    pub async fn apply_tmdb(
        &self,
        season_id: Uuid,
        tmdb_id: i64,
        title: Option<&str>,
        overview: Option<&str>,
        poster_url: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE seasons SET \
               tmdb_id    = ?, \
               title      = COALESCE(?, title), \
               overview   = COALESCE(?, overview), \
               poster_url = COALESCE(?, poster_url), \
               updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
             WHERE id = ?",
        )
        .bind(tmdb_id)
        .bind(title)
        .bind(overview)
        .bind(poster_url)
        .bind(season_id.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Delete seasons in this library that have no episodes. Called by
    /// the scanner after the media_file prune pass. Returns the number
    /// of rows pruned.
    pub async fn prune_empty_for_library(&self, library_id: Uuid) -> Result<u64> {
        let res = sqlx::query(
            "DELETE FROM seasons \
             WHERE series_id IN (SELECT id FROM series WHERE library_id = ?) \
               AND id NOT IN (SELECT DISTINCT season_id FROM episodes)",
        )
        .bind(library_id.to_string())
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }
}
