//! `/api/libraries/:id/movies`, `/api/movies/:id`, plus the poster /
//! stream / progress side endpoints.
//!
//! Read-only browse + playback surface used by the SPA. All endpoints
//! are auth-only (not admin-only) — any authenticated user can see and
//! play what's in the library.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use mythos_auth::AuthUser;
use mythos_core::{MediaFile, Movie, SubtitleTrack, WatchProgress};
use mythos_db::{MediaFileRepo, MovieRepo, ProgressRepo, SubtitleRepo};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::PostersDir;
use crate::error::{ApiError, ApiResult};
use crate::hls::resolve_movie_to_file;
use crate::stream::stream_file;

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
    /// Per-user resume point. `None` when the requesting user has not
    /// played this movie yet.
    pub progress: Option<WatchProgress>,
    /// All subtitle tracks the scanner found on the underlying file.
    /// Empty when no tracks are present.
    pub subtitles: Vec<SubtitleTrack>,
}

#[derive(Debug, Deserialize)]
pub struct ProgressUpdate {
    pub position_seconds: f64,
    pub duration_seconds: f64,
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
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<MovieDetail>> {
    let movies = MovieRepo::new(pool.clone());
    let movie = movies
        .find_by_id(id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found"))?;

    let file = MediaFileRepo::new(pool.clone())
        .find_by_id(movie.file_id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "file_missing"))?;

    let progress = ProgressRepo::new(pool.clone()).find(user.id, id).await?;
    let subtitles = SubtitleRepo::new(pool).list_by_file(file.id).await?;

    Ok(Json(MovieDetail {
        movie,
        file,
        progress,
        subtitles,
    }))
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

pub async fn put_progress(
    State(pool): State<SqlitePool>,
    user: AuthUser,
    Path(movie_id): Path<Uuid>,
    Json(update): Json<ProgressUpdate>,
) -> ApiResult<StatusCode> {
    if !update.position_seconds.is_finite()
        || !update.duration_seconds.is_finite()
        || update.position_seconds < 0.0
        || update.duration_seconds <= 0.0
    {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "invalid_progress"));
    }

    // Make sure the movie exists before writing a row whose FK would
    // be satisfied. Cheaper than relying on the FK to reject — gives a
    // sharper 404.
    let exists = MovieRepo::new(pool.clone())
        .find_by_id(movie_id)
        .await?
        .is_some();
    if !exists {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "not_found"));
    }

    ProgressRepo::new(pool)
        .upsert(
            user.id,
            movie_id,
            update.position_seconds,
            update.duration_seconds,
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Stream the underlying media file with HTTP byte-range support so
/// `<video>` can seek without re-downloading and the browser can spool
/// playback before the whole file arrives.
pub async fn stream(
    State(pool): State<SqlitePool>,
    _user: AuthUser,
    Path(movie_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let file_id = resolve_movie_to_file(&pool, movie_id).await?;
    stream_file(&pool, file_id, &headers).await
}
