//! `/api/libraries/:id/movies`, `/api/movies/:id`, plus the poster /
//! stream / progress side endpoints.
//!
//! Read-only browse + playback surface used by the SPA. All endpoints
//! are auth-only (not admin-only) — any authenticated user can see and
//! play what's in the library.

use std::io::SeekFrom;
use std::path::PathBuf;

use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use mythos_auth::AuthUser;
use mythos_core::{MediaFile, Movie, SubtitleTrack, WatchProgress};
use mythos_db::{MediaFileRepo, MovieRepo, ProgressRepo, SubtitleRepo};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;
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
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT l.root_path, f.path \
         FROM movies m \
         JOIN media_files f ON f.id = m.file_id \
         JOIN libraries  l ON l.id = m.library_id \
         WHERE m.id = ?",
    )
    .bind(movie_id.to_string())
    .fetch_optional(&pool)
    .await
    .map_err(|err| {
        tracing::error!(?err, "stream lookup failed");
        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal")
    })?;

    let (root, rel) = row.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found"))?;
    let abs = PathBuf::from(root).join(rel);

    let metadata = tokio::fs::metadata(&abs).await.map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            ApiError::new(StatusCode::NOT_FOUND, "file_missing")
        } else {
            tracing::error!(?err, path = %abs.display(), "stat failed");
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal")
        }
    })?;
    if !metadata.is_file() {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "file_missing"));
    }
    let size = metadata.len();
    let mime = mime_guess::from_path(&abs).first_or_octet_stream();
    let mime_header = HeaderValue::from_str(mime.as_ref())
        .unwrap_or(HeaderValue::from_static("application/octet-stream"));

    let range = match headers.get(header::RANGE).and_then(|v| v.to_str().ok()) {
        Some(value) => match parse_range(value, size) {
            Ok(maybe) => maybe,
            Err(()) => {
                let mut res = (StatusCode::RANGE_NOT_SATISFIABLE, Body::empty()).into_response();
                res.headers_mut().insert(
                    header::CONTENT_RANGE,
                    HeaderValue::from_str(&format!("bytes */{size}"))
                        .expect("size formats cleanly"),
                );
                return Ok(res);
            }
        },
        None => None,
    };

    let file = tokio::fs::File::open(&abs).await.map_err(|err| {
        tracing::error!(?err, path = %abs.display(), "open failed");
        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal")
    })?;

    if let Some((start, end)) = range {
        let mut file = file;
        if start > 0 {
            file.seek(SeekFrom::Start(start)).await.map_err(|err| {
                tracing::error!(?err, "seek failed");
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal")
            })?;
        }
        let take_len = end - start + 1;
        let reader = file.take(take_len);
        let body = Body::from_stream(ReaderStream::new(reader));
        let mut res = (StatusCode::PARTIAL_CONTENT, body).into_response();
        let h = res.headers_mut();
        h.insert(header::CONTENT_TYPE, mime_header);
        h.insert(header::CONTENT_LENGTH, header_num(take_len));
        h.insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {start}-{end}/{size}"))
                .expect("range formats cleanly"),
        );
        h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
        Ok(res)
    } else {
        let body = Body::from_stream(ReaderStream::new(file));
        let mut res = (StatusCode::OK, body).into_response();
        let h = res.headers_mut();
        h.insert(header::CONTENT_TYPE, mime_header);
        h.insert(header::CONTENT_LENGTH, header_num(size));
        h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
        Ok(res)
    }
}

/// Parse an RFC 7233 single-range `Range: bytes=…` header.
///
/// Returns:
/// - `Ok(Some((start, end)))` on success
/// - `Ok(None)` if the header isn't a `bytes=` range (treat as no range)
/// - `Err(())` if the range is syntactically present but unsatisfiable
///   (caller responds 416)
///
/// Multi-range (`bytes=0-100,200-300`) is intentionally rejected — the
/// HTML5 video element only ever asks for single ranges, so the
/// implementation cost isn't justified.
fn parse_range(value: &str, size: u64) -> Result<Option<(u64, u64)>, ()> {
    let Some(spec) = value.trim().strip_prefix("bytes=") else {
        return Ok(None);
    };
    if spec.contains(',') {
        return Err(());
    }
    let mut parts = spec.splitn(2, '-');
    let start_s = parts.next().ok_or(())?.trim();
    let end_s = parts.next().ok_or(())?.trim();

    let (start, end) = if start_s.is_empty() {
        // bytes=-N → last N bytes
        let n: u64 = end_s.parse().map_err(|_| ())?;
        if n == 0 {
            return Err(());
        }
        let start = size.saturating_sub(n);
        (start, size - 1)
    } else if end_s.is_empty() {
        // bytes=N- → from N to end
        let start: u64 = start_s.parse().map_err(|_| ())?;
        if start >= size {
            return Err(());
        }
        (start, size - 1)
    } else {
        let start: u64 = start_s.parse().map_err(|_| ())?;
        let end: u64 = end_s.parse().map_err(|_| ())?;
        if start > end || start >= size {
            return Err(());
        }
        (start, end.min(size - 1))
    };
    Ok(Some((start, end)))
}

fn header_num(n: u64) -> HeaderValue {
    HeaderValue::from_str(&n.to_string()).expect("u64 formats as ASCII")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_full() {
        assert_eq!(parse_range("bytes=0-99", 1000), Ok(Some((0, 99))));
    }

    #[test]
    fn range_open_ended() {
        assert_eq!(parse_range("bytes=500-", 1000), Ok(Some((500, 999))));
    }

    #[test]
    fn range_suffix() {
        assert_eq!(parse_range("bytes=-100", 1000), Ok(Some((900, 999))));
    }

    #[test]
    fn range_clamps_end_to_size() {
        assert_eq!(parse_range("bytes=0-9999", 1000), Ok(Some((0, 999))));
    }

    #[test]
    fn range_invalid_start_past_end() {
        assert_eq!(parse_range("bytes=2000-", 1000), Err(()));
    }

    #[test]
    fn range_invalid_inverted() {
        assert_eq!(parse_range("bytes=100-50", 1000), Err(()));
    }

    #[test]
    fn range_no_bytes_prefix_is_ignored() {
        assert_eq!(parse_range("seconds=0-10", 1000), Ok(None));
    }

    #[test]
    fn range_multi_rejected() {
        assert_eq!(parse_range("bytes=0-99,200-299", 1000), Err(()));
    }
}
