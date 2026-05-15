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
    ABR_LADDER, HwAccel, ItemKind, Rendition, SEGMENT_WAIT_TIMEOUT, SOURCE_VARIANT, SessionKey,
    TonemapAlgorithm, TonemapConfig, TonemapPipeline, TonemapSupport, TranscodeError,
    TranscodeManager, build_master_playlist, build_variant_playlist, is_known_variant,
    parse_segment_filename, rendition_by_name, source_rendition, wait_for_file,
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
    let tonemap =
        resolve_tonemap_config(pool, file_id, manager.hwaccel(), manager.tonemap_support()).await?;
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
/// When the column is `NULL` (row pre-dates migration 0009, scanner
/// failed, etc.) [`resolve_color_transfer`] ffprobes the file once
/// and writes the result back so this session and the next don't
/// pay the probe again. The admin setting acts as a global on/off
/// so an operator can disable the curve if it doesn't look right on
/// their content.
///
/// The stored [`TonemapPipeline`] is coerced down to
/// [`TonemapPipeline::Software`] under two conditions:
/// 1. The pick isn't valid for the active encoder (e.g. operator
///    set `vaapi` but we're running on NVENC). Stale value left over
///    from an encoder change.
/// 2. The pick is valid but the named filter isn't compiled into
///    this ffmpeg build (per [`TonemapSupport`]).
///
/// In both cases the stored row is preserved so an ffmpeg rebuild
/// or encoder swap restores the operator's intent without a UI dance.
///
/// Defaults: enabled = `true`, algorithm = `Hable`,
/// pipeline = [`TonemapPipeline::Software`]. Software is the safe
/// default — it always works and produces correct output. Operators
/// who want the GPU path opt in explicitly via the admin UI.
async fn resolve_tonemap_config(
    pool: &SqlitePool,
    file_id: Uuid,
    accel: HwAccel,
    support: TonemapSupport,
) -> ApiResult<TonemapConfig> {
    let color_transfer = resolve_color_transfer(pool, file_id).await;
    let is_hdr_source = matches!(
        color_transfer.as_deref(),
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
    let stored_pipeline = repo
        .get(setting_keys::TONEMAP_PIPELINE)
        .await?
        .as_deref()
        .map(TonemapPipeline::from_str_or_default)
        .unwrap_or_default();
    let valid_for_accel = TonemapSupport::valid_for(accel).contains(&stored_pipeline);
    let pipeline = if valid_for_accel && support.supports(stored_pipeline) {
        stored_pipeline
    } else {
        TonemapPipeline::Software
    };

    Ok(TonemapConfig {
        apply: is_hdr_source && enabled,
        algorithm,
        pipeline,
    })
}

/// Best-effort lookup of a file's `color_transfer` for the HDR check.
///
/// Three cases:
/// - Row exists and the column has a value → return it.
/// - Row exists but the column is `NULL` (typical for installs that
///   were scanned before migration 0009 added the color columns) →
///   ffprobe the file on-the-fly, write the result back into the row,
///   and return it. Self-heals stale rows transparently; the next play
///   pays nothing extra.
/// - Row doesn't exist, the file is gone, or ffprobe fails → return
///   `None`. The caller treats `None` as SDR, which is the safe choice
///   (no tonemap applied; SDR sources go through unchanged, HDR
///   sources look washed out but don't crash the session).
///
/// All failures log at `warn` but don't propagate — the call is a hot
/// path on every HLS request and we don't want a transient probe
/// failure to 500 an otherwise valid play.
async fn resolve_color_transfer(pool: &SqlitePool, file_id: Uuid) -> Option<String> {
    let stored: Option<(Option<String>,)> =
        sqlx::query_as("SELECT color_transfer FROM media_files WHERE id = ?")
            .bind(file_id.to_string())
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
    match stored {
        Some((Some(transfer),)) => Some(transfer),
        Some((None,)) => probe_and_persist_color(pool, file_id).await,
        None => None,
    }
}

/// On-demand ffprobe path used by [`resolve_color_transfer`] when the
/// DB row has `color_transfer IS NULL`. Probes the file, persists all
/// three color columns (primaries / transfer / space) back so the
/// scanner doesn't need to re-run, and returns the transfer so the
/// caller can decide whether to tonemap.
///
/// Adds one ffprobe per *file* per *upgrade* — roughly 50–200ms once
/// per HDR file ever played on a pre-0009 install, then nothing. The
/// write-back is a single-row UPDATE so we don't compete with scanner
/// transactions; on the off chance it loses a race with a scanner
/// rewriting the same row, both writes end up with the same values.
async fn probe_and_persist_color(pool: &SqlitePool, file_id: Uuid) -> Option<String> {
    let path = match resolve_input_path_for_file(pool, file_id).await {
        Ok(p) => p,
        Err(err) => {
            tracing::warn!(?err, %file_id, "on-demand color probe: couldn't resolve path");
            return None;
        }
    };
    let probe = match mythos_scan::probe(&path).await {
        Ok(p) => p,
        Err(err) => {
            tracing::warn!(?err, %file_id, "on-demand color probe failed; treating as SDR");
            return None;
        }
    };
    let res = sqlx::query(
        "UPDATE media_files \
         SET color_primaries = ?, color_transfer = ?, color_space = ? \
         WHERE id = ?",
    )
    .bind(&probe.color_primaries)
    .bind(&probe.color_transfer)
    .bind(&probe.color_space)
    .bind(file_id.to_string())
    .execute(pool)
    .await;
    match res {
        Ok(_) => tracing::info!(
            %file_id,
            color_transfer = ?probe.color_transfer,
            "on-demand color probe persisted"
        ),
        Err(err) => tracing::warn!(?err, %file_id, "on-demand color probe write-back failed"),
    }
    probe.color_transfer
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    /// Generate a tiny mp4 with explicit bt709 color tags so the
    /// on-demand probe has deterministic metadata to read. Two layers
    /// are required: `setparams` seeds frame-side tags on the lavfi
    /// source (libx264 only writes SPS color tags when the input
    /// frames carry them), and the matching `-color_*` flags on the
    /// encoder pass them through to the container's stream metadata.
    /// Without setparams, libx264 quietly leaves transfer/primaries
    /// blank in the SPS even when the encoder flags ask for bt709.
    fn ffmpeg_test_input(dir: &std::path::Path) -> std::path::PathBuf {
        let path = dir.join("input.mp4");
        let status = Command::new(mythos_core::ffmpeg_bin())
            .args([
                "-y",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=red:size=160x90:duration=0.5:rate=10",
                "-vf",
                "setparams=color_primaries=bt709:color_trc=bt709:colorspace=bt709,format=yuv420p",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-color_primaries",
                "bt709",
                "-color_trc",
                "bt709",
                "-colorspace",
                "bt709",
            ])
            .arg(&path)
            .status()
            .expect("ffmpeg on PATH");
        assert!(status.success());
        path
    }

    async fn seed_file_with_null_color(pool: &SqlitePool, dir: &std::path::Path) -> Uuid {
        let library_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO libraries (id, name, kind, root_path) VALUES (?, 'test', 'movies', ?)",
        )
        .bind(library_id.to_string())
        .bind(dir.to_str().unwrap())
        .execute(pool)
        .await
        .unwrap();
        let file_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO media_files (id, library_id, path, size_bytes, mtime) \
             VALUES (?, ?, 'input.mp4', 0, '2026-01-01T00:00:00.000Z')",
        )
        .bind(file_id.to_string())
        .bind(library_id.to_string())
        .execute(pool)
        .await
        .unwrap();
        file_id
    }

    /// The core durable-fix promise: a pre-0009 row with
    /// `color_transfer IS NULL` self-heals on first request, the DB is
    /// updated, and a second call returns the same value without
    /// re-probing.
    #[tokio::test]
    async fn resolve_color_transfer_probes_persists_and_caches() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!("../../migrations").run(&pool).await.unwrap();
        let dir = TempDir::new().unwrap();
        ffmpeg_test_input(dir.path());
        let file_id = seed_file_with_null_color(&pool, dir.path()).await;

        // Before: column is NULL.
        let (before,): (Option<String>,) =
            sqlx::query_as("SELECT color_transfer FROM media_files WHERE id = ?")
                .bind(file_id.to_string())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(before.is_none());

        // First call: probes the file, persists, returns the value.
        // Our fixture explicitly tags the stream as bt709 so we can
        // assert on the exact value — looser `is_some()` would hide a
        // bug where the probe parsed but mis-extracted the field.
        let first = resolve_color_transfer(&pool, file_id).await;
        assert_eq!(first.as_deref(), Some("bt709"));

        // The persist must actually write — otherwise every subsequent
        // play re-probes, defeating the cache.
        let (after,): (Option<String>,) =
            sqlx::query_as("SELECT color_transfer FROM media_files WHERE id = ?")
                .bind(file_id.to_string())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(after, first, "stored value must match what we returned");

        // Second call: hits the fast path (stored value present), no
        // probe. We assert equality with `first` — same value, no
        // accidental drift from a stray probe.
        let second = resolve_color_transfer(&pool, file_id).await;
        assert_eq!(second, first);
    }

    /// Missing file: the probe fails; the function returns None
    /// rather than 500-ing the HLS request. SDR fall-through is the
    /// safe behaviour.
    #[tokio::test]
    async fn resolve_color_transfer_returns_none_when_file_missing() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!("../../migrations").run(&pool).await.unwrap();
        let dir = TempDir::new().unwrap();
        // Note: we don't create the file on disk — only the DB row.
        let file_id = seed_file_with_null_color(&pool, dir.path()).await;

        let result = resolve_color_transfer(&pool, file_id).await;
        assert!(result.is_none());
    }

    /// Row missing entirely (file_id from another universe): None,
    /// no panic.
    #[tokio::test]
    async fn resolve_color_transfer_returns_none_when_row_missing() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!("../../migrations").run(&pool).await.unwrap();

        let result = resolve_color_transfer(&pool, Uuid::now_v7()).await;
        assert!(result.is_none());
    }
}
