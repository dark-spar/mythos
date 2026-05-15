//! `/api/{movies|episodes}/:id/subtitles/:sub_id/vtt` — extract a
//! text subtitle track from its container and serve it as WebVTT for
//! the SPA's `<track>` element.
//!
//! Image-based subs (PGS, VOBSUB, DVB, XSUB) can't be served this way
//! — they're pre-rasterized bitmaps with positioning baked in, with no
//! browser API for rendering them out-of-band. Those go through the
//! HLS burn-in path instead, so this endpoint returns 422 for them.
//!
//! Extraction is slow: ffmpeg has to scan the container end-to-end
//! to find subtitle packets, which for a 60GB Blu-ray remux can take
//! a minute or more. To avoid hitting that on every request (and the
//! 30s default that was previously timing out before the first byte
//! arrived), we cache the extracted WebVTT to disk on first request
//! and serve from cache thereafter. The cache key is the subtitle's
//! UUID, which is regenerated on every rescan — orphaned cache
//! files are tolerated; they're tiny and re-extraction is cheap if
//! they need to be remade.

use std::process::Stdio;

use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use mythos_auth::AuthUser;
use mythos_core::ffmpeg_bin;
use mythos_db::SubtitleRepo;
use sqlx::SqlitePool;
use tokio::process::Command;
use uuid::Uuid;

use crate::SubtitlesDir;
use crate::error::{ApiError, ApiResult};
use crate::hls::{resolve_episode_to_file, resolve_input_path_for_file, resolve_movie_to_file};

/// 5 minutes. Long enough to scan a feature-length Blu-ray remux,
/// short enough that a wedged ffmpeg eventually frees the request.
const FFMPEG_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

pub async fn webvtt(
    State(pool): State<SqlitePool>,
    State(cache_dir): State<SubtitlesDir>,
    _user: AuthUser,
    Path((movie_id, sub_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Response> {
    let file_id = resolve_movie_to_file(&pool, movie_id).await?;
    webvtt_for_file(&pool, &cache_dir, file_id, sub_id).await
}

pub async fn episode_webvtt(
    State(pool): State<SqlitePool>,
    State(cache_dir): State<SubtitlesDir>,
    _user: AuthUser,
    Path((episode_id, sub_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Response> {
    let file_id = resolve_episode_to_file(&pool, episode_id).await?;
    webvtt_for_file(&pool, &cache_dir, file_id, sub_id).await
}

async fn webvtt_for_file(
    pool: &SqlitePool,
    cache_dir: &SubtitlesDir,
    file_id: Uuid,
    sub_id: Uuid,
) -> ApiResult<Response> {
    let sub = SubtitleRepo::new(pool.clone())
        .find_by_id(sub_id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found"))?;
    if sub.file_id != file_id {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "not_found"));
    }
    if sub.is_image {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "image_subtitle_not_extractable",
        ));
    }

    let cache_path = cache_dir.0.join(format!("{sub_id}.vtt"));
    let body = if let Some(cached) = read_if_present(&cache_path).await {
        cached
    } else {
        let abs = resolve_input_path_for_file(pool, file_id).await?;
        let extracted = extract_webvtt(&abs, sub.stream_index).await?;
        // Write-through cache. Best-effort: a write failure here just
        // means we'll re-extract on the next request.
        if let Err(err) = write_cache(&cache_path, &extracted).await {
            tracing::warn!(?err, path = %cache_path.display(), "subtitle cache write failed");
        }
        extracted
    };

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

async fn read_if_present(path: &std::path::Path) -> Option<Vec<u8>> {
    match tokio::fs::read(path).await {
        Ok(bytes) => Some(bytes),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            tracing::warn!(?err, path = %path.display(), "subtitle cache read failed");
            None
        }
    }
}

async fn write_cache(path: &std::path::Path, body: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(path, body).await
}

async fn extract_webvtt(input: &std::path::Path, stream_index: i64) -> ApiResult<Vec<u8>> {
    let mut cmd = Command::new(ffmpeg_bin());
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
