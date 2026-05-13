//! `/api/movies/:id/hls/*` handlers.
//!
//! Two endpoints share the single `{filename}` route:
//!
//! - `playlist.m3u8` — a synthetic VOD playlist describing the full
//!   movie. Built purely from `media_files.duration_seconds`; ffmpeg
//!   isn't touched. The player gets a complete timeline up front and
//!   can scrub anywhere.
//!
//! - `seg-N.ts` — segment fetch. The transcode manager either reuses
//!   the active session if it covers segment `N` or restarts at offset
//!   `N * SEGMENT_DURATION_SECS`. Then we wait for the corresponding
//!   on-disk file and serve it.
//!
//! Both endpoints are auth-only. Sessions are keyed on `(user_id,
//! movie_id)` so each viewer has their own pipeline.

use std::path::PathBuf;

use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use mythos_auth::AuthUser;
use mythos_stream::{
    SEGMENT_WAIT_TIMEOUT, SessionKey, TranscodeError, TranscodeManager, build_vod_playlist,
    parse_segment_filename, wait_for_file,
};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};

#[derive(Clone, Default)]
pub struct HlsHandle(pub Option<TranscodeManager>);

pub async fn hls(
    State(pool): State<SqlitePool>,
    State(hls): State<HlsHandle>,
    user: AuthUser,
    Path((movie_id, filename)): Path<(Uuid, String)>,
) -> ApiResult<Response> {
    if filename == "playlist.m3u8" {
        return serve_playlist(&pool, movie_id).await;
    }

    let seg_idx = parse_segment_filename(&filename)
        .ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "bad_filename"))?;

    let manager = hls
        .0
        .as_ref()
        .ok_or_else(|| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "transcoding_disabled"))?;

    let abs_path = resolve_input_path(&pool, movie_id).await?;
    let key = SessionKey {
        user_id: user.id,
        movie_id,
    };

    let session = manager
        .ensure_session_for_segment(key, &abs_path, seg_idx)
        .await
        .map_err(map_transcode_error)?;

    let segment_path = session.local_segment_path(seg_idx).map_err(|err| {
        tracing::error!(?err, "local_segment_path failed after ensure_session");
        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal")
    })?;

    wait_for_file(&segment_path, SEGMENT_WAIT_TIMEOUT)
        .await
        .map_err(map_transcode_error)?;

    serve_bytes(&segment_path, "video/mp2t").await
}

async fn serve_playlist(pool: &SqlitePool, movie_id: Uuid) -> ApiResult<Response> {
    let duration = movie_duration_seconds(pool, movie_id).await?;
    let body = build_vod_playlist(duration);
    let mut res = (StatusCode::OK, body).into_response();
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/vnd.apple.mpegurl"),
    );
    // The playlist is deterministic per movie (a pure function of
    // duration) — short-cache is safe.
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=60"),
    );
    Ok(res)
}

async fn serve_bytes(path: &std::path::Path, content_type: &'static str) -> ApiResult<Response> {
    match tokio::fs::read(path).await {
        Ok(bytes) => {
            let mut res = (StatusCode::OK, bytes).into_response();
            let h = res.headers_mut();
            h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
            // Segments are stable for the lifetime of a session but
            // get nuked on restart, so don't cache across sessions.
            h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            Ok(res)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            Err(ApiError::new(StatusCode::NOT_FOUND, "segment_not_ready"))
        }
        Err(err) => {
            tracing::error!(?err, path = %path.display(), "hls file read failed");
            Err(ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal"))
        }
    }
}

async fn resolve_input_path(pool: &SqlitePool, movie_id: Uuid) -> ApiResult<PathBuf> {
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT l.root_path, f.path \
         FROM movies m \
         JOIN media_files f ON f.id = m.file_id \
         JOIN libraries  l ON l.id = m.library_id \
         WHERE m.id = ?",
    )
    .bind(movie_id.to_string())
    .fetch_optional(pool)
    .await
    .map_err(|err| {
        tracing::error!(?err, "hls lookup failed");
        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal")
    })?;

    let (root, rel) = row.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found"))?;
    let abs = PathBuf::from(root).join(rel);
    if !tokio::fs::try_exists(&abs).await.unwrap_or(false) {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "file_missing"));
    }
    Ok(abs)
}

async fn movie_duration_seconds(pool: &SqlitePool, movie_id: Uuid) -> ApiResult<f64> {
    let row: Option<(Option<f64>,)> = sqlx::query_as(
        "SELECT mf.duration_seconds \
         FROM movies m \
         JOIN media_files mf ON mf.id = m.file_id \
         WHERE m.id = ?",
    )
    .bind(movie_id.to_string())
    .fetch_optional(pool)
    .await
    .map_err(|err| {
        tracing::error!(?err, "hls duration lookup failed");
        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal")
    })?;

    let dur = row
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found"))?
        .0
        .ok_or_else(|| {
            // ffprobe failed for this file (typically because ffmpeg
            // wasn't installed when the scan ran) so we don't know how
            // many segments to advertise. Tell the user to rescan.
            ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "unknown_duration")
        })?;
    if dur <= 0.0 {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "unknown_duration",
        ));
    }
    Ok(dur)
}

fn map_transcode_error(err: TranscodeError) -> ApiError {
    match err {
        TranscodeError::Spawn(io) => {
            tracing::error!(?io, "ffmpeg spawn failed");
            ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "ffmpeg_unavailable")
        }
        TranscodeError::Timeout => ApiError::new(StatusCode::GATEWAY_TIMEOUT, "transcode_timeout"),
        TranscodeError::InvalidFilename(_) => {
            ApiError::new(StatusCode::BAD_REQUEST, "bad_filename")
        }
        TranscodeError::BeforeSessionStart { .. } => {
            // Should never reach here because ensure_session_for_segment
            // restarts in that case. If it does, treat as not-ready
            // so the player retries.
            ApiError::new(StatusCode::NOT_FOUND, "segment_not_ready")
        }
        TranscodeError::Io(io) => {
            tracing::error!(?io, "transcode io error");
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal")
        }
    }
}
