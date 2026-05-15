//! `/api/{movies|episodes}/:id/hls/*` handlers (ABR multi-rendition).
//!
//! Three endpoints share the routing space per item kind:
//!
//! - `GET .../hls/master.m3u8` — synthetic master playlist listing
//!   every rendition. Built purely from the ABR ladder constant;
//!   ffmpeg isn't touched.
//!
//! - `GET .../hls/:variant/playlist.m3u8` — synthetic per-variant
//!   media playlist describing every segment up to the item's full
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
//!
//! Movie and episode handlers each resolve their path param to a
//! `file_id`, then delegate to the file-id-keyed inner helpers; the
//! shared `SessionKey { user_id, item_id, kind }` keeps movie and
//! episode sessions in their own slots.

use std::path::PathBuf;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use mythos_auth::AuthUser;
use mythos_core::PlaybackMode;
use mythos_db::SettingsRepo;
use mythos_db::SubtitleRepo;
use mythos_db::settings::keys as setting_keys;
use mythos_stream::{
    ABR_LADDER, ItemKind, Rendition, SEGMENT_WAIT_TIMEOUT, SOURCE_VARIANT, SessionKey,
    TonemapAlgorithm, TonemapConfig, TranscodeError, TranscodeManager, build_master_playlist,
    build_variant_playlist, is_known_variant, parse_segment_filename, rendition_by_name,
    source_rendition, wait_for_file,
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
    /// Playback mode this stream serves. Driven by the `/play`
    /// endpoint; defaults to `transcode_full` so existing clients
    /// that don't yet send `?mode=` still get the full-ABR pipeline.
    #[serde(default)]
    pub mode: Option<PlaybackMode>,
    /// Comma-separated rendition names (`"480p,720p"`) for ABR modes.
    /// Ignored for copy modes (which always emit a single source
    /// rendition). Defaults to the full ABR ladder when missing.
    #[serde(default)]
    pub v: Option<String>,
}

impl HlsQuery {
    fn mode_or_default(&self) -> PlaybackMode {
        self.mode.unwrap_or(PlaybackMode::TranscodeFull)
    }
}

// =========================================================================
// Movie handlers: resolve movie_id → file_id, then delegate.
// =========================================================================

pub async fn stop(
    State(hls): State<HlsHandle>,
    user: AuthUser,
    Path(movie_id): Path<Uuid>,
) -> Response {
    stop_inner(&hls, user.id, movie_id, ItemKind::Movie).await
}

pub async fn master(
    State(pool): State<SqlitePool>,
    _user: AuthUser,
    Path(movie_id): Path<Uuid>,
    Query(q): Query<HlsQuery>,
) -> ApiResult<Response> {
    let file_id = resolve_movie_to_file(&pool, movie_id).await?;
    master_inner(&pool, file_id, q).await
}

pub async fn variant_file(
    State(pool): State<SqlitePool>,
    State(hls): State<HlsHandle>,
    user: AuthUser,
    Path((movie_id, variant, filename)): Path<(Uuid, String, String)>,
    Query(q): Query<HlsQuery>,
) -> ApiResult<Response> {
    let file_id = resolve_movie_to_file(&pool, movie_id).await?;
    variant_file_inner(
        &pool,
        &hls,
        user.id,
        movie_id,
        ItemKind::Movie,
        file_id,
        variant,
        filename,
        q,
    )
    .await
}

// =========================================================================
// Episode handlers.
// =========================================================================

pub async fn episode_stop(
    State(hls): State<HlsHandle>,
    user: AuthUser,
    Path(episode_id): Path<Uuid>,
) -> Response {
    stop_inner(&hls, user.id, episode_id, ItemKind::Episode).await
}

pub async fn episode_master(
    State(pool): State<SqlitePool>,
    _user: AuthUser,
    Path(episode_id): Path<Uuid>,
    Query(q): Query<HlsQuery>,
) -> ApiResult<Response> {
    let file_id = resolve_episode_to_file(&pool, episode_id).await?;
    master_inner(&pool, file_id, q).await
}

pub async fn episode_variant_file(
    State(pool): State<SqlitePool>,
    State(hls): State<HlsHandle>,
    user: AuthUser,
    Path((episode_id, variant, filename)): Path<(Uuid, String, String)>,
    Query(q): Query<HlsQuery>,
) -> ApiResult<Response> {
    let file_id = resolve_episode_to_file(&pool, episode_id).await?;
    variant_file_inner(
        &pool,
        &hls,
        user.id,
        episode_id,
        ItemKind::Episode,
        file_id,
        variant,
        filename,
        q,
    )
    .await
}

// =========================================================================
// Inner kind-agnostic handlers, keyed on file_id (+ item_id/kind for
// SessionKey when running ffmpeg).
// =========================================================================

async fn stop_inner(hls: &HlsHandle, user_id: Uuid, item_id: Uuid, kind: ItemKind) -> Response {
    if let Some(manager) = hls.0.as_ref() {
        let key = SessionKey {
            user_id,
            item_id,
            kind,
        };
        manager.stop(&key).await;
    }
    (StatusCode::NO_CONTENT, ()).into_response()
}

async fn master_inner(pool: &SqlitePool, file_id: Uuid, q: HlsQuery) -> ApiResult<Response> {
    let mode = q.mode_or_default();
    if mode == PlaybackMode::DirectPlay {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "direct_play_has_no_master",
        ));
    }
    // Validate the file has a known duration so we don't hand the
    // player a master that points at variants we can't serve.
    file_duration_seconds(pool, file_id).await?;
    let renditions = resolve_renditions(pool, file_id, mode, q.v.as_deref()).await?;
    let query = build_query_string(pool, file_id, &q).await?;
    let body = build_master_playlist(&renditions, &query);
    Ok(playlist_response(body))
}

#[allow(clippy::too_many_arguments)]
async fn variant_file_inner(
    pool: &SqlitePool,
    hls: &HlsHandle,
    user_id: Uuid,
    item_id: Uuid,
    kind: ItemKind,
    file_id: Uuid,
    variant: String,
    filename: String,
    q: HlsQuery,
) -> ApiResult<Response> {
    if !is_known_variant(&variant) {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "unknown_variant"));
    }
    let mode = q.mode_or_default();
    let renditions = resolve_renditions(pool, file_id, mode, q.v.as_deref()).await?;
    let rendition = *renditions
        .iter()
        .find(|r| r.name == variant)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "unknown_variant"))?;

    if filename == "playlist.m3u8" {
        let duration = file_duration_seconds(pool, file_id).await?;
        let query = build_query_string(pool, file_id, &q).await?;
        let body = build_variant_playlist(duration, &rendition, &query);
        return Ok(playlist_response(body));
    }

    let seg_idx = parse_segment_filename(&filename)
        .ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "bad_filename"))?;

    let manager = hls
        .0
        .as_ref()
        .ok_or_else(|| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "transcoding_disabled"))?;

    let abs_path = resolve_input_path_for_file(pool, file_id).await?;
    let burn_in_sub = resolve_burn_in_sub(pool, file_id, q.sub).await?;
    let tonemap = resolve_tonemap_config(pool, file_id).await?;
    let key = SessionKey {
        user_id,
        item_id,
        kind,
    };

    let session = manager
        .ensure_session_for_segment(
            key,
            &abs_path,
            &variant,
            seg_idx,
            burn_in_sub,
            mode,
            tonemap,
            &renditions,
        )
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

// =========================================================================
// File-id-keyed lookups (shared between movie and episode paths).
// =========================================================================

/// Compute the rendition list for the given mode + optional `v=` hint.
///
/// - Copy modes (Remux, TranscodeAudio) always emit a single
///   source-resolution rendition. We need media_files to know the
///   source width/height/duration/size for the bandwidth hint.
/// - ABR modes emit a subset of [`ABR_LADDER`] selected by the
///   comma-separated `v=` names; missing or empty `v` means the full
///   ladder.
async fn resolve_renditions(
    pool: &SqlitePool,
    file_id: Uuid,
    mode: PlaybackMode,
    v: Option<&str>,
) -> ApiResult<Vec<Rendition>> {
    if matches!(mode, PlaybackMode::Remux | PlaybackMode::TranscodeAudio) {
        let info = source_dimensions_for_file(pool, file_id).await?;
        return Ok(vec![source_rendition(
            info.width,
            info.height,
            info.size_bytes,
            info.duration_seconds,
        )]);
    }
    // ABR modes (TranscodeVideo, TranscodeFull).
    let names: Vec<String> = match v {
        Some(s) if !s.trim().is_empty() => s
            .split(',')
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
            .collect(),
        _ => ABR_LADDER.iter().map(|r| r.name.to_string()).collect(),
    };
    let mut chosen: Vec<Rendition> = Vec::with_capacity(names.len());
    for name in &names {
        if name == SOURCE_VARIANT {
            // Reject mixing source with ABR — it would advertise a
            // pass-through tier in a master that's wired for re-encode.
            return Err(ApiError::new(StatusCode::BAD_REQUEST, "invalid_renditions"));
        }
        let r = rendition_by_name(name)
            .ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "unknown_rendition"))?;
        chosen.push(*r);
    }
    if chosen.is_empty() {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "no_renditions"));
    }
    Ok(chosen)
}

#[derive(Debug)]
struct SourceInfo {
    width: u32,
    height: u32,
    duration_seconds: f64,
    size_bytes: u64,
}

async fn source_dimensions_for_file(pool: &SqlitePool, file_id: Uuid) -> ApiResult<SourceInfo> {
    type Row = (Option<i64>, Option<i64>, Option<f64>, i64);
    let row: Option<Row> = sqlx::query_as(
        "SELECT width, height, duration_seconds, size_bytes \
         FROM media_files WHERE id = ?",
    )
    .bind(file_id.to_string())
    .fetch_optional(pool)
    .await
    .map_err(|err| {
        tracing::error!(?err, "source_dimensions lookup failed");
        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal")
    })?;
    let (w, h, dur, size) = row.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found"))?;
    let width = w.and_then(|n| u32::try_from(n).ok()).unwrap_or(0);
    let height = h.and_then(|n| u32::try_from(n).ok()).unwrap_or(0);
    if width == 0 || height == 0 {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "unknown_dimensions",
        ));
    }
    let duration_seconds = dur.unwrap_or(0.0).max(0.0);
    let size_bytes = u64::try_from(size).unwrap_or(0);
    Ok(SourceInfo {
        width,
        height,
        duration_seconds,
        size_bytes,
    })
}

/// Build the query-string suffix to append to URLs inside the
/// synthetic playlists, combining `?mode=...`, `?sub=...`, and
/// `?v=...` from the original request so they propagate through
/// hls.js relative URL resolution.
async fn build_query_string(pool: &SqlitePool, file_id: Uuid, q: &HlsQuery) -> ApiResult<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(mode) = q.mode {
        parts.push(format!("mode={}", mode.as_str()));
    }
    if let Some(v) = q.v.as_deref().filter(|s| !s.trim().is_empty()) {
        parts.push(format!("v={v}"));
    }
    if let Some(sub_id) = q.sub {
        let track = lookup_sub_for_file(pool, file_id, sub_id).await?;
        if track.is_image {
            parts.push(format!("sub={sub_id}"));
        }
    }
    Ok(if parts.is_empty() {
        String::new()
    } else {
        format!("?{}", parts.join("&"))
    })
}

/// Translate the `?sub=<uuid>` query into the absolute ffprobe stream
/// index the transcoder needs, or `None` if no sub was requested or
/// the requested sub is text (text subs go via WebVTT sidecar).
async fn resolve_burn_in_sub(
    pool: &SqlitePool,
    file_id: Uuid,
    sub: Option<Uuid>,
) -> ApiResult<Option<i64>> {
    let Some(sub_id) = sub else {
        return Ok(None);
    };
    let track = lookup_sub_for_file(pool, file_id, sub_id).await?;
    Ok(track.is_image.then_some(track.stream_index))
}

/// Fetch a subtitle track and verify it belongs to `file_id`. Returns
/// 404 if the sub doesn't exist or belongs to a different file.
pub(crate) async fn lookup_sub_for_file(
    pool: &SqlitePool,
    file_id: Uuid,
    sub_id: Uuid,
) -> ApiResult<mythos_core::SubtitleTrack> {
    let track = SubtitleRepo::new(pool.clone())
        .find_by_id(sub_id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "unknown_subtitle"))?;
    if track.file_id != file_id {
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

/// Look up the absolute path of a file by joining through libraries.
pub(crate) async fn resolve_input_path_for_file(
    pool: &SqlitePool,
    file_id: Uuid,
) -> ApiResult<PathBuf> {
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT l.root_path, f.path \
         FROM media_files f JOIN libraries l ON l.id = f.library_id \
         WHERE f.id = ?",
    )
    .bind(file_id.to_string())
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

/// Decide whether this session should tonemap HDR→SDR.
///
/// HDR is detected from the source's `color_transfer` column
/// (`smpte2084` = HDR10 PQ, `arib-std-b67` = HLG); anything else
/// means an SDR source where tonemapping would be a no-op or worse.
/// The admin setting acts as a global on/off so an operator can
/// disable the curve if it doesn't look right on their content.
///
/// Defaults: enabled = `true`, algorithm = `Hable`. Both are
/// chosen so a fresh install with no settings rows still tonemaps
/// HDR content — the un-tonemapped fallback is the bad surprise.
async fn resolve_tonemap_config(pool: &SqlitePool, file_id: Uuid) -> ApiResult<TonemapConfig> {
    let transfer: Option<(Option<String>,)> =
        sqlx::query_as("SELECT color_transfer FROM media_files WHERE id = ?")
            .bind(file_id.to_string())
            .fetch_optional(pool)
            .await
            .map_err(|err| {
                tracing::error!(?err, "color_transfer lookup failed");
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal")
            })?;
    let is_hdr_source = matches!(
        transfer.and_then(|(t,)| t).as_deref(),
        Some("smpte2084" | "arib-std-b67")
    );

    let repo = SettingsRepo::new(pool.clone());
    let enabled = repo
        .get(setting_keys::TONEMAP_ENABLED)
        .await?
        .as_deref()
        .map(|v| {
            !matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "false" | "0" | "off"
            )
        })
        .unwrap_or(true);
    let algorithm = repo
        .get(setting_keys::TONEMAP_ALGORITHM)
        .await?
        .as_deref()
        .map(TonemapAlgorithm::from_str_or_default)
        .unwrap_or_default();

    Ok(TonemapConfig {
        apply: is_hdr_source && enabled,
        algorithm,
    })
}

async fn file_duration_seconds(pool: &SqlitePool, file_id: Uuid) -> ApiResult<f64> {
    let row: Option<(Option<f64>,)> =
        sqlx::query_as("SELECT duration_seconds FROM media_files WHERE id = ?")
            .bind(file_id.to_string())
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

/// Resolve a movie id to its `file_id`. Returns 404 if missing.
pub(crate) async fn resolve_movie_to_file(pool: &SqlitePool, movie_id: Uuid) -> ApiResult<Uuid> {
    let row: Option<(String,)> = sqlx::query_as("SELECT file_id FROM movies WHERE id = ?")
        .bind(movie_id.to_string())
        .fetch_optional(pool)
        .await
        .map_err(|err| {
            tracing::error!(?err, "movie file lookup failed");
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal")
        })?;
    let file_id_str = row
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found"))?
        .0;
    Uuid::parse_str(&file_id_str).map_err(|err| {
        tracing::error!(?err, "decoded file_id is not a uuid");
        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal")
    })
}

/// Resolve an episode id to its `file_id`. Returns 404 if missing.
pub(crate) async fn resolve_episode_to_file(
    pool: &SqlitePool,
    episode_id: Uuid,
) -> ApiResult<Uuid> {
    let row: Option<(String,)> = sqlx::query_as("SELECT file_id FROM episodes WHERE id = ?")
        .bind(episode_id.to_string())
        .fetch_optional(pool)
        .await
        .map_err(|err| {
            tracing::error!(?err, "episode file lookup failed");
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal")
        })?;
    let file_id_str = row
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found"))?
        .0;
    Uuid::parse_str(&file_id_str).map_err(|err| {
        tracing::error!(?err, "decoded file_id is not a uuid");
        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal")
    })
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
