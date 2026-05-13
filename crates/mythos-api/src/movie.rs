//! `/api/libraries/:id/movies`, `/api/movies/:id`, `/api/movies/:id/poster`.
//!
//! Read-only browse surface used by the SPA grid + detail pages. All
//! three are auth-only (not admin-only) — anyone with an account can
//! see what's in the library.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use mythos_auth::AuthUser;
use mythos_core::{MediaFile, Movie};
use mythos_db::{MediaFileRepo, MovieRepo};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::PostersDir;
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

pub async fn poster(
    State(posters): State<PostersDir>,
    _user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let path = posters.0.join(format!("{id}.jpg"));
    match tokio::fs::read(&path).await {
        Ok(bytes) => {
            let mut res = (StatusCode::OK, bytes).into_response();
            res.headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static("image/jpeg"));
            // Poster content for a given movie id is stable until the
            // next scan replaces the file. Long-ish browser cache is
            // safe; the URL ends with the movie's UUID so different
            // movies never collide.
            res.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=3600"),
            );
            Ok(res)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            Err(ApiError::new(StatusCode::NOT_FOUND, "not_found"))
        }
        Err(err) => {
            tracing::error!(?err, path = %path.display(), "failed to read poster");
            Err(ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal"))
        }
    }
}
