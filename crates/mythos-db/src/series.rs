//! `series` repository.

use chrono::{DateTime, Utc};
use mythos_core::{NewSeries, Series, sort_title};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::{DbError, Result};

#[derive(sqlx::FromRow)]
struct SeriesRow {
    id: String,
    library_id: String,
    title: String,
    sort_title: String,
    year: Option<i64>,
    tmdb_id: Option<i64>,
    overview: Option<String>,
    poster_url: Option<String>,
    created_at: String,
    updated_at: String,
}

impl SeriesRow {
    fn into_series(self) -> Result<Series> {
        Ok(Series {
            id: parse_uuid("series id", &self.id)?,
            library_id: parse_uuid("series library_id", &self.library_id)?,
            title: self.title,
            sort_title: self.sort_title,
            year: self.year,
            tmdb_id: self.tmdb_id,
            overview: self.overview,
            poster_url: self.poster_url,
            created_at: parse_ts("series created_at", &self.created_at)?,
            updated_at: parse_ts("series updated_at", &self.updated_at)?,
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
pub struct SeriesRepo {
    pool: SqlitePool,
}

impl SeriesRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Insert a series or refresh the identifier-derived fields if one
    /// already exists with the same (library_id, sort_title). Preserves
    /// `tmdb_id`, `overview`, `poster_url` so a re-scan doesn't clobber
    /// enrichment.
    pub async fn upsert(&self, new: NewSeries) -> Result<Series> {
        let new_id = Uuid::now_v7();
        let sort = sort_title(&new.title);

        let row: SeriesRow = sqlx::query_as(
            "INSERT INTO series (id, library_id, title, sort_title, year) \
             VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT (library_id, sort_title) DO UPDATE SET \
               title      = excluded.title, \
               year       = COALESCE(series.year, excluded.year), \
               updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
             RETURNING id, library_id, title, sort_title, year, \
               tmdb_id, overview, poster_url, created_at, updated_at",
        )
        .bind(new_id.to_string())
        .bind(new.library_id.to_string())
        .bind(&new.title)
        .bind(&sort)
        .bind(new.year)
        .fetch_one(&self.pool)
        .await?;

        row.into_series()
    }

    pub async fn find_by_id(&self, id: Uuid) -> Result<Option<Series>> {
        let row: Option<SeriesRow> = sqlx::query_as(
            "SELECT id, library_id, title, sort_title, year, \
                    tmdb_id, overview, poster_url, created_at, updated_at \
             FROM series WHERE id = ?",
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await?;
        row.map(SeriesRow::into_series).transpose()
    }

    pub async fn list_by_library(
        &self,
        library_id: Uuid,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Series>> {
        let rows: Vec<SeriesRow> = sqlx::query_as(
            "SELECT id, library_id, title, sort_title, year, \
                    tmdb_id, overview, poster_url, created_at, updated_at \
             FROM series WHERE library_id = ? \
             ORDER BY sort_title COLLATE NOCASE ASC, year ASC \
             LIMIT ? OFFSET ?",
        )
        .bind(library_id.to_string())
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(SeriesRow::into_series).collect()
    }

    pub async fn count_by_library(&self, library_id: Uuid) -> Result<i64> {
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM series WHERE library_id = ?")
            .bind(library_id.to_string())
            .fetch_one(&self.pool)
            .await?;
        Ok(n)
    }

    /// Series in this library that haven't been TMDb-matched yet.
    pub async fn list_unenriched_by_library(
        &self,
        library_id: Uuid,
    ) -> Result<Vec<UnenrichedSeries>> {
        let rows: Vec<UnenrichedRow> = sqlx::query_as(
            "SELECT id, title, year FROM series \
             WHERE library_id = ? AND tmdb_id IS NULL",
        )
        .bind(library_id.to_string())
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|r| {
                Ok(UnenrichedSeries {
                    id: parse_uuid("series id", &r.id)?,
                    title: r.title,
                    year: r.year,
                })
            })
            .collect()
    }

    /// Apply TMDb metadata to a series row. Bumps `updated_at`.
    pub async fn apply_tmdb(
        &self,
        series_id: Uuid,
        tmdb_id: i64,
        overview: Option<&str>,
        poster_url: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE series SET \
               tmdb_id    = ?, \
               overview   = ?, \
               poster_url = ?, \
               updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
             WHERE id = ?",
        )
        .bind(tmdb_id)
        .bind(overview)
        .bind(poster_url)
        .bind(series_id.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Delete series in this library that have no seasons. Called by
    /// the scanner after the media_file prune pass so empty parents
    /// don't linger when every episode of a series gets removed from
    /// disk. Returns the number of rows pruned.
    pub async fn prune_empty_for_library(&self, library_id: Uuid) -> Result<u64> {
        let res = sqlx::query(
            "DELETE FROM series \
             WHERE library_id = ? \
               AND id NOT IN (SELECT DISTINCT series_id FROM seasons)",
        )
        .bind(library_id.to_string())
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }
}

#[derive(Debug, Clone)]
pub struct UnenrichedSeries {
    pub id: Uuid,
    pub title: String,
    pub year: Option<i64>,
}

#[derive(sqlx::FromRow)]
struct UnenrichedRow {
    id: String,
    title: String,
    year: Option<i64>,
}
