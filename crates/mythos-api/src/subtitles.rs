//! `/api/movies/:id/subtitles/:sub_id/vtt` — extract a text subtitle
//! track from its container and serve it as WebVTT for the SPA's
//! `<track>` element.
//!
//! Image-based subs (PGS, VOBSUB, DVB, XSUB) can't be served this way
//! — they're pre-rasterized bitmaps with positioning baked in, with no
//! browser API for rendering them out-of-band. Those go through the
//! HLS burn-in path instead, so this endpoint returns 422 for them.
//!
//! WebVTT is built on demand by spawning ffmpeg with the captured
//! stream index. Subtitle tracks are small (≤ a few hundred KB even
//! for a feature) so we collect the output into memory; that
//! sidesteps streaming-ffmpeg-stdout-back-out plumbing and keeps the
//! handler easy to reason about. The browser caches by URL, and
//! sub_id only changes on rescan.

use std::path::PathBuf;
use std::process::Stdio;

use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use mythos_auth::AuthUser;
use mythos_db::{MovieRepo, SubtitleRepo};
use sqlx::SqlitePool;
use tokio::process::Command;
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};

const FFMPEG_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

pub async fn webvtt(
    State(pool): State<SqlitePool>,
    _user: AuthUser,
    Path((movie_id, sub_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Response> {
    let movie = MovieRepo::new(pool.clone())
        .find_by_id(movie_id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found"))?;

    let sub = SubtitleRepo::new(pool.clone())
        .find_by_id(sub_id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found"))?;
    if sub.file_id != movie.file_id {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "not_found"));
    }
    if sub.is_image {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "image_subtitle_not_extractable",
        ));
    }

    let abs = resolve_input_path(&pool, movie_id).await?;
    let body = extract_webvtt(&abs, sub.stream_index).await?;

    let mut res = (StatusCode::OK, body).into_response();
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/vtt"));
    // Subs are stable until the next rescan; browser may cache. The
    // sub_id in the URL changes after a rescan, so no stale risk.
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=3600"),
    );
    Ok(res)
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
        tracing::error!(?err, "subtitle input lookup failed");
        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal")
    })?;
    let (root, rel) = row.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found"))?;
    let abs = PathBuf::from(root).join(rel);
    if !tokio::fs::try_exists(&abs).await.unwrap_or(false) {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "file_missing"));
    }
    Ok(abs)
}

async fn extract_webvtt(input: &std::path::Path, stream_index: i64) -> ApiResult<Vec<u8>> {
    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-i")
        .arg(input)
        .arg("-map")
        .arg(format!("0:{stream_index}"))
        .arg("-c:s")
        .arg("webvtt")
        .arg("-f")
        .arg("webvtt")
        .arg("-")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let child = cmd.spawn().map_err(|err| {
        tracing::error!(?err, "ffmpeg spawn for webvtt extraction failed");
        ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "ffmpeg_unavailable")
    })?;

    let output = match tokio::time::timeout(FFMPEG_TIMEOUT, child.wait_with_output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(err)) => {
            tracing::error!(?err, "ffmpeg wait failed");
            return Err(ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal"));
        }
        Err(_) => {
            return Err(ApiError::new(
                StatusCode::GATEWAY_TIMEOUT,
                "subtitle_extract_timeout",
            ));
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!(
            stream_index,
            stderr = %stderr.trim(),
            "ffmpeg failed to extract subtitle as webvtt"
        );
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "subtitle_extract_failed",
        ));
    }
    Ok(output.stdout)
}
