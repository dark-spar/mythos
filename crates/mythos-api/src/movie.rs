//! `/api/libraries/:id/movies` and `/api/movies/:id`.
//!
//! Read-only browse surface used by the SPA grid + detail pages. Both
//! endpoints are auth-only (not admin-only) — anyone with an account
//! can see what's in the library.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use mythos_auth::AuthUser;
use mythos_core::{MediaFile, Movie};
use mythos_db::{MediaFileRepo, MovieRepo};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};

const DEFAULT_LIMIT: i64 = 60;
const MAX_LIMIT: i64 = 200;

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

fn default_limit() -> i64 {
    DEFAULT_LIMIT
}

#[derive(Debug, Serialize)]
pub struct MoviesPage {
    pub items: Vec<Movie>,
    pub total: i64,
    pub limit: i64,
    pub offset: i64,
}

#[derive(Debug, Serialize)]
pub struct MovieDetail {
    pub movie: Movie,
    pub file: MediaFile,
}

pub async fn list(
    State(pool): State<SqlitePool>,
    _user: AuthUser,
    Path(library_id): Path<Uuid>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<MoviesPage>> {
    let limit = q.limit.clamp(1, MAX_LIMIT);
    let offset = q.offset.max(0);

    let repo = MovieRepo::new(pool);
    let items = repo.list_by_library(library_id, limit, offset).await?;
    let total = repo.count_by_library(library_id).await?;

    Ok(Json(MoviesPage {
        items,
        total,
        limit,
        offset,
    }))
}

pub async fn get_one(
    State(pool): State<SqlitePool>,
    _user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<MovieDetail>> {
    let movies = MovieRepo::new(pool.clone());
    let movie = movies
        .find_by_id(id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found"))?;

    let file = MediaFileRepo::new(pool)
        .find_by_id(movie.file_id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "file_missing"))?;

    Ok(Json(MovieDetail { movie, file }))
}
