//! `/api/movies/:id/hls/*` handlers (ABR multi-rendition).
//!
//! Three endpoints share the routing space:
//!
//! - `GET .../hls/master.m3u8` — synthetic master playlist listing
//!   every rendition. Built purely from the ABR ladder constant;
//!   ffmpeg isn't touched.
//!
//! - `GET .../hls/:variant/playlist.m3u8` — synthetic per-variant
//!   media playlist describing every segment up to the movie's full
//!   duration. Also built without invoking ffmpeg, so the player gets
//!   the complete timeline up front.
//!
//! - `GET .../hls/:variant/seg-N.ts` — the actual segment for a given
//!   variant. Triggers the transcode session if it doesn't exist or
//!   restarts it if `N` is far from the current frontier (seek), then
//!   waits for ffmpeg to produce the file. All renditions are
//!   produced in lockstep by one ffmpeg, so the variant the player
//!   chose to fetch implicitly drives session start_segment.
//!
//! `DELETE .../hls` stops the active session for the requesting user.

use std::path::PathBuf;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use mythos_auth::AuthUser;
use mythos_db::SubtitleRepo;
use mythos_stream::{
    SEGMENT_WAIT_TIMEOUT, SessionKey, TranscodeError, TranscodeManager, build_master_playlist,
    build_variant_playlist, is_known_variant, parse_segment_filename, rendition_by_name,
    wait_for_file,
};
use serde::Deserialize;
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};

#[derive(Clone, Default)]
pub struct HlsHandle(pub Option<TranscodeManager>);

#[derive(Debug, Default, Deserialize)]
pub struct HlsQuery {
    /// UUID of the subtitle track to burn in. Only honoured when the
    /// track is image-based (PGS/VOBSUB/...); text subs are served as
    /// WebVTT sidecars and don't affect the transcode.
    #[serde(default)]
    pub sub: Option<Uuid>,
}

pub async fn stop(
    State(hls): State<HlsHandle>,
    user: AuthUser,
    Path(movie_id): Path<Uuid>,
) -> Response {
    if let Some(manager) = hls.0.as_ref() {
        let key = SessionKey {
            user_id: user.id,
            movie_id,
        };
        manager.stop(&key).await;
    }
    (StatusCode::NO_CONTENT, ()).into_response()
}

/// Handler for `master.m3u8`.
pub async fn master(
    State(pool): State<SqlitePool>,
    _user: AuthUser,
    Path(movie_id): Path<Uuid>,
    Query(q): Query<HlsQuery>,
) -> ApiResult<Response> {
    // Validate the movie exists and has a known duration so we don't
    // hand the player a master that points at variants we can't serve.
    movie_duration_seconds(&pool, movie_id).await?;
    let query = playlist_query_for_sub(&pool, movie_id, q.sub).await?;
    let body = build_master_playlist(&query);
    Ok(playlist_response(body))
}

/// Handler for everything under `:variant/`. Dispatches between the
/// per-variant playlist and individual segments.
pub async fn variant_file(
    State(pool): State<SqlitePool>,
    State(hls): State<HlsHandle>,
    user: AuthUser,
    Path((movie_id, variant, filename)): Path<(Uuid, String, String)>,
    Query(q): Query<HlsQuery>,
) -> ApiResult<Response> {
    if !is_known_variant(&variant) {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "unknown_variant"));
    }

    if filename == "playlist.m3u8" {
        let duration = movie_duration_seconds(&pool, movie_id).await?;
        let rendition = rendition_by_name(&variant)
            .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "unknown_variant"))?;
        let query = playlist_query_for_sub(&pool, movie_id, q.sub).await?;
        let body = build_variant_playlist(duration, rendition, &query);
        return Ok(playlist_response(body));
    }

    let seg_idx = parse_segment_filename(&filename)
        .ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "bad_filename"))?;

    let manager = hls
        .0
        .as_ref()
        .ok_or_else(|| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "transcoding_disabled"))?;

    let abs_path = resolve_input_path(&pool, movie_id).await?;
    let burn_in_sub = resolve_burn_in_sub(&pool, movie_id, q.sub).await?;
    let key = SessionKey {
        user_id: user.id,
        movie_id,
    };

    let session = manager
        .ensure_session_for_segment(key, &abs_path, &variant, seg_idx, burn_in_sub)
        .await
        .map_err(map_transcode_error)?;

    let segment_path = session
        .local_segment_path(&variant, seg_idx)
        .map_err(|err| {
            tracing::error!(?err, "local_segment_path failed after ensure_session");
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal")
        })?;

    wait_for_file(&segment_path, SEGMENT_WAIT_TIMEOUT)
        .await
        .map_err(map_transcode_error)?;

    serve_bytes(&segment_path, "video/mp2t").await
}

/// Build the query-string suffix appended to URLs in the synthetic
/// playlists. Empty when no sub is selected; `"?sub=<uuid>"` when an
/// image sub is selected; empty when a text sub is selected (text
/// subs go via WebVTT sidecar, not burn-in).
///
/// Returns a 404 if the sub_id doesn't exist or doesn't belong to
/// the movie.
async fn playlist_query_for_sub(
    pool: &SqlitePool,
    movie_id: Uuid,
    sub: Option<Uuid>,
) -> ApiResult<String> {
    let Some(sub_id) = sub else {
        return Ok(String::new());
    };
    let track = lookup_sub_for_movie(pool, movie_id, sub_id).await?;
    if track.is_image {
        Ok(format!("?sub={sub_id}"))
    } else {
        Ok(String::new())
    }
}

/// Translate the `?sub=<uuid>` query into the absolute ffprobe stream
/// index the transcoder needs, or `None` if no sub was requested or
/// the requested sub is text (text subs go via WebVTT sidecar).
async fn resolve_burn_in_sub(
    pool: &SqlitePool,
    movie_id: Uuid,
    sub: Option<Uuid>,
) -> ApiResult<Option<i64>> {
    let Some(sub_id) = sub else {
        return Ok(None);
    };
    let track = lookup_sub_for_movie(pool, movie_id, sub_id).await?;
    Ok(track.is_image.then_some(track.stream_index))
}

async fn lookup_sub_for_movie(
    pool: &SqlitePool,
    movie_id: Uuid,
    sub_id: Uuid,
) -> ApiResult<mythos_core::SubtitleTrack> {
    let movie_file: Option<(String,)> = sqlx::query_as("SELECT file_id FROM movies WHERE id = ?")
        .bind(movie_id.to_string())
        .fetch_optional(pool)
        .await
        .map_err(|err| {
            tracing::error!(?err, "movie file lookup failed");
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal")
        })?;
    let file_id_str = movie_file
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found"))?
        .0;
    let track = SubtitleRepo::new(pool.clone())
        .find_by_id(sub_id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "unknown_subtitle"))?;
    if track.file_id.to_string() != file_id_str {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "unknown_subtitle"));
    }
    Ok(track)
}

fn playlist_response(body: String) -> Response {
    let mut res = (StatusCode::OK, body).into_response();
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/vnd.apple.mpegurl"),
    );
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=60"),
    );
    res
}

async fn serve_bytes(path: &std::path::Path, content_type: &'static str) -> ApiResult<Response> {
    match tokio::fs::read(path).await {
        Ok(bytes) => {
            let mut res = (StatusCode::OK, bytes).into_response();
            let h = res.headers_mut();
            h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
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
        .ok_or_else(|| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "unknown_duration"))?;
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
        TranscodeError::InvalidVariant(_) => {
            ApiError::new(StatusCode::NOT_FOUND, "unknown_variant")
        }
        TranscodeError::BeforeSessionStart { .. } => {
            ApiError::new(StatusCode::NOT_FOUND, "segment_not_ready")
        }
        TranscodeError::SessionStillBooting => {
            ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "session_booting")
        }
        TranscodeError::Io(io) => {
            tracing::error!(?io, "transcode io error");
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal")
        }
    }
}
