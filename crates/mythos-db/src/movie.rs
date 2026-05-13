//! `movies` repository.

use chrono::{DateTime, Utc};
use mythos_core::{Movie, NewMovie, sort_title};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::{DbError, Result};

#[derive(sqlx::FromRow)]
struct MovieRow {
    id: String,
    library_id: String,
    file_id: String,
    title: String,
    sort_title: String,
    year: Option<i64>,
    tmdb_id: Option<i64>,
    overview: Option<String>,
    poster_url: Option<String>,
    created_at: String,
    updated_at: String,
}

impl MovieRow {
    fn into_movie(self) -> Result<Movie> {
        Ok(Movie {
            id: parse_uuid("movie id", &self.id)?,
            library_id: parse_uuid("movie library_id", &self.library_id)?,
            file_id: parse_uuid("movie file_id", &self.file_id)?,
            title: self.title,
            sort_title: self.sort_title,
            year: self.year,
            tmdb_id: self.tmdb_id,
            overview: self.overview,
            poster_url: self.poster_url,
            created_at: parse_ts("movie created_at", &self.created_at)?,
            updated_at: parse_ts("movie updated_at", &self.updated_at)?,
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
pub struct MovieRepo {
    pool: SqlitePool,
}

impl MovieRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Insert a movie or refresh the identifier-derived fields if one
    /// already exists for this file. Does not touch `tmdb_id` / `overview`
    /// / `poster_url`: those are owned by Phase 1d metadata, which would
    /// otherwise be clobbered by a re-scan.
    pub async fn upsert(&self, new: NewMovie) -> Result<Movie> {
        let new_id = Uuid::now_v7();
        let sort = sort_title(&new.title);

        let row: MovieRow = sqlx::query_as(
            "INSERT INTO movies (id, library_id, file_id, title, sort_title, year) \
             VALUES (?, ?, ?, ?, ?, ?) \
             ON CONFLICT (file_id) DO UPDATE SET \
               title      = excluded.title, \
               sort_title = excluded.sort_title, \
               year       = excluded.year, \
               updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
             RETURNING id, library_id, file_id, title, sort_title, year, \
               tmdb_id, overview, poster_url, created_at, updated_at",
        )
        .bind(new_id.to_string())
        .bind(new.library_id.to_string())
        .bind(new.file_id.to_string())
        .bind(&new.title)
        .bind(&sort)
        .bind(new.year)
        .fetch_one(&self.pool)
        .await?;

        row.into_movie()
    }

    pub async fn find_by_id(&self, id: Uuid) -> Result<Option<Movie>> {
        let row: Option<MovieRow> = sqlx::query_as(
            "SELECT id, library_id, file_id, title, sort_title, year, \
                    tmdb_id, overview, poster_url, created_at, updated_at \
             FROM movies WHERE id = ?",
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await?;
        row.map(MovieRow::into_movie).transpose()
    }

    pub async fn list_by_library(
        &self,
        library_id: Uuid,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Movie>> {
        let rows: Vec<MovieRow> = sqlx::query_as(
            "SELECT id, library_id, file_id, title, sort_title, year, \
                    tmdb_id, overview, poster_url, created_at, updated_at \
             FROM movies WHERE library_id = ? \
             ORDER BY sort_title COLLATE NOCASE ASC, year ASC \
             LIMIT ? OFFSET ?",
        )
        .bind(library_id.to_string())
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(MovieRow::into_movie).collect()
    }

    pub async fn count_by_library(&self, library_id: Uuid) -> Result<i64> {
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM movies WHERE library_id = ?")
            .bind(library_id.to_string())
            .fetch_one(&self.pool)
            .await?;
        Ok(n)
    }
}
