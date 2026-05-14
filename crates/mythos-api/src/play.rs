//! `POST /api/movies/:id/play` — per-request playback decision.
//!
//! Client sends what it can decode (codec/container/resolution
//! caps); server consults the source file's technical metadata and
//! returns the cheapest pipeline that satisfies the client, plus the
//! URL to fetch from. The SPA hits this on every playback request
//! and routes accordingly. The Phase 6 Jellyfin shim will eventually
//! translate Jellyfin's `DeviceProfile` JSON into the same
//! `ClientProfile` shape and call straight in here.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use mythos_auth::AuthUser;
use mythos_core::{ClientProfile, MediaCapabilities, PlaybackMode, PlaybackPlan, decide};
use mythos_db::{MediaFileRepo, MovieRepo};
use mythos_stream::ABR_LADDER as ABR_RENDITION_LIST;
use serde::Serialize;
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};

#[derive(Debug, Serialize)]
pub struct PlayResponse {
    pub mode: PlaybackMode,
    /// Relative URL the client should load. Direct-play returns
    /// `/api/movies/:id/stream`; everything else returns an HLS
    /// master URL with `mode=...` and any rendition filter baked in.
    pub stream_url: String,
    /// For HLS-based modes, the ABR tier names this client is
    /// expected to play. Driven by `ClientProfile.max_height` —
    /// a 720p-capable client gets `["480p", "720p"]`. Empty for
    /// direct-play and copy modes.
    pub allowed_renditions: Vec<&'static str>,
    /// Mirror of the per-dimension checks so the SPA can render an
    /// informed banner ("transcoding because: audio codec").
    pub diagnostic: Diagnostic,
}

#[derive(Debug, Serialize)]
pub struct Diagnostic {
    pub container_ok: bool,
    pub video_ok: bool,
    pub audio_ok: bool,
    pub resolution_ok: bool,
}

pub async fn play(
    State(pool): State<SqlitePool>,
    _user: AuthUser,
    Path(movie_id): Path<Uuid>,
    Json(profile): Json<ClientProfile>,
) -> ApiResult<Json<PlayResponse>> {
    let movie = MovieRepo::new(pool.clone())
        .find_by_id(movie_id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found"))?;
    let file = MediaFileRepo::new(pool)
        .find_by_id(movie.file_id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "file_missing"))?;

    let caps = MediaCapabilities {
        container: file.probe.container.as_deref(),
        video_codec: file.probe.video_codec.as_deref(),
        // Scanner doesn't capture AVC/HEVC level today, so the check
        // is permissive on level. Resolution cap is the practical
        // ceiling for now.
        video_level: None,
        audio_codec: file.probe.audio_codec.as_deref(),
        audio_channels: None,
        width: file.probe.width.and_then(|n| u32::try_from(n).ok()),
        height: file.probe.height.and_then(|n| u32::try_from(n).ok()),
    };

    let plan: PlaybackPlan = decide(caps, &profile);
    let allowed = allowed_renditions(plan.mode, &profile);
    let stream_url = build_stream_url(movie_id, plan.mode, &allowed);

    Ok(Json(PlayResponse {
        mode: plan.mode,
        stream_url,
        allowed_renditions: allowed,
        diagnostic: Diagnostic {
            container_ok: plan.container_ok,
            video_ok: plan.video_ok,
            audio_ok: plan.audio_ok,
            resolution_ok: plan.resolution_ok,
        },
    }))
}

/// Pick the ABR tiers the client should see. Copy modes and
/// direct-play return an empty list because they don't use the
/// ladder.
fn allowed_renditions(mode: PlaybackMode, profile: &ClientProfile) -> Vec<&'static str> {
    if matches!(
        mode,
        PlaybackMode::DirectPlay | PlaybackMode::Remux | PlaybackMode::TranscodeAudio
    ) {
        return Vec::new();
    }
    let cap = profile.max_height;
    let mut chosen: Vec<&'static str> = ABR_RENDITION_LIST
        .iter()
        .filter(|r| cap.is_none_or(|max| r.height <= max))
        .map(|r| r.name)
        .collect();
    if chosen.is_empty() {
        // Client's max_height is below our smallest tier — give them
        // the smallest anyway so something plays. The encoder will
        // produce a 480p stream that the client downsamples.
        chosen.push(ABR_RENDITION_LIST[0].name);
    }
    chosen
}

fn build_stream_url(movie_id: Uuid, mode: PlaybackMode, allowed: &[&str]) -> String {
    match mode {
        PlaybackMode::DirectPlay => format!("/api/movies/{movie_id}/stream"),
        PlaybackMode::Remux | PlaybackMode::TranscodeAudio => {
            format!(
                "/api/movies/{movie_id}/hls/master.m3u8?mode={}",
                mode.as_str()
            )
        }
        PlaybackMode::TranscodeVideo | PlaybackMode::TranscodeFull => {
            let v = allowed.join(",");
            format!(
                "/api/movies/{movie_id}/hls/master.m3u8?mode={}&v={v}",
                mode.as_str()
            )
        }
    }
}
