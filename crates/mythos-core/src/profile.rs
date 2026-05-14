//! Client capability profile + per-request playback decision.
//!
//! The transport story is "client declares, server decides": each
//! playback request carries a `ClientProfile` describing what the
//! caller can actually decode, and the server picks the cheapest
//! pipeline that satisfies it. No server-side device table, no
//! User-Agent matching — the Phase 6 Jellyfin shim will translate
//! Jellyfin's device profiles into this same shape.
//!
//! The full transcode taxonomy lives in [`PlaybackMode`]. Most
//! requests fall into [`PlaybackMode::DirectPlay`] (raw byte-range)
//! or [`PlaybackMode::TranscodeFull`] (current HLS path); the middle
//! tiers exist so we don't burn CPU re-encoding pieces the client
//! could already handle.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClientProfile {
    /// Container/format extensions the client can demux (lowercased,
    /// without the dot — `"mp4"`, `"webm"`, ...). Match is
    /// case-insensitive against `MediaFile.container`.
    #[serde(default)]
    pub containers: Vec<String>,
    #[serde(default)]
    pub video_codecs: Vec<VideoCodecCap>,
    #[serde(default)]
    pub audio_codecs: Vec<AudioCodecCap>,
    pub max_width: Option<u32>,
    pub max_height: Option<u32>,
    pub max_audio_channels: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoCodecCap {
    /// Lowercase codec name as ffprobe reports it: `"h264"`,
    /// `"hevc"`, `"av1"`, `"vp9"`, ...
    pub codec: String,
    /// Optional profile constraint: `"main"`, `"high"`, `"main10"`.
    /// `None` means any profile of this codec is fine.
    pub profile: Option<String>,
    /// Optional level constraint, scaled ×10 (level 4.0 = 40, 4.1 = 41).
    /// Source must be `<= level` to direct-play; `None` means any.
    pub level: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioCodecCap {
    /// Lowercase codec name: `"aac"`, `"ac3"`, `"eac3"`, `"opus"`, ...
    pub codec: String,
    /// Optional channel-count ceiling. Source must have
    /// `<= max_channels` to direct-play; `None` means any.
    pub max_channels: Option<u32>,
}

/// Per-request decision: which pipeline serves this playback.
///
/// The matrix maps from `(container_ok, video_ok, audio_ok, resolution_ok)`
/// onto the cheapest mode that satisfies all mismatched dimensions:
///
/// |  container  |  video  |  audio  |  resolution  |       mode       |
/// |:-----------:|:-------:|:-------:|:------------:|:----------------:|
/// |     OK      |   OK    |   OK    |      OK      |    DirectPlay    |
/// |     ✗       |   OK    |   OK    |      OK      |      Remux       |
/// |    any      |   OK    |   ✗     |      OK      |  TranscodeAudio  |
/// |    any      |   ✗     |   OK    |      OK      |  TranscodeVideo  |
/// |    any      |  any    |  any    |      ✗       |  TranscodeFull   |
/// |    any      |   ✗     |   ✗     |     any      |  TranscodeFull   |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackMode {
    /// Range-served raw file.
    DirectPlay,
    /// HLS with `-c copy`: same codecs, repackaged into MPEG-TS.
    Remux,
    /// HLS with `-c:v copy -c:a aac`.
    TranscodeAudio,
    /// HLS with the normal video encoder + `-c:a copy`.
    TranscodeVideo,
    /// HLS with the full ABR re-encode pipeline.
    TranscodeFull,
}

impl PlaybackMode {
    pub fn as_str(self) -> &'static str {
        match self {
            PlaybackMode::DirectPlay => "direct_play",
            PlaybackMode::Remux => "remux",
            PlaybackMode::TranscodeAudio => "transcode_audio",
            PlaybackMode::TranscodeVideo => "transcode_video",
            PlaybackMode::TranscodeFull => "transcode_full",
        }
    }

    /// Whether this mode goes through the HLS pipeline. Only
    /// [`PlaybackMode::DirectPlay`] doesn't.
    pub fn is_hls(self) -> bool {
        !matches!(self, PlaybackMode::DirectPlay)
    }
}

/// What the playback endpoint hands back to the caller. Carries the
/// chosen mode plus diagnostic flags so the SPA can render an
/// informed banner if needed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaybackPlan {
    pub mode: PlaybackMode,
    /// Diagnostic: per-dimension yes/no so the UI can say "we're
    /// transcoding because: audio codec." Independent of `mode`
    /// (transcode-full implies multiple `false`s but the reason
    /// might be specifically the resolution).
    pub container_ok: bool,
    pub video_ok: bool,
    pub audio_ok: bool,
    pub resolution_ok: bool,
}

/// Decide how to play `(container, video_codec, audio_codec,
/// width, height)` against a client profile. Resolution mismatch
/// forces a full re-encode because we have to actually scale; any
/// other single mismatch picks the cheapest mode that fixes it.
pub fn decide(source: MediaCapabilities<'_>, profile: &ClientProfile) -> PlaybackPlan {
    let container_ok = profile.supports_container(source.container);
    let video_ok = profile.supports_video(source.video_codec, source.video_level);
    let audio_ok = profile.supports_audio(source.audio_codec, source.audio_channels);
    let resolution_ok = profile.supports_resolution(source.width, source.height);

    let mode = if !resolution_ok || (!video_ok && !audio_ok) {
        PlaybackMode::TranscodeFull
    } else if container_ok && video_ok && audio_ok {
        PlaybackMode::DirectPlay
    } else if video_ok && audio_ok {
        // Only the container is wrong.
        PlaybackMode::Remux
    } else if video_ok {
        // Video is fine, just re-encode audio.
        PlaybackMode::TranscodeAudio
    } else {
        // Audio is fine, just re-encode video.
        PlaybackMode::TranscodeVideo
    };

    PlaybackPlan {
        mode,
        container_ok,
        video_ok,
        audio_ok,
        resolution_ok,
    }
}

/// Lightweight view of a source file for [`decide`]. Keeps the
/// signature decoupled from [`crate::MediaFile`] so call sites can
/// pass partial info (e.g. tests, or a future API that takes only
/// the bits it needs).
#[derive(Debug, Clone, Copy, Default)]
pub struct MediaCapabilities<'a> {
    pub container: Option<&'a str>,
    pub video_codec: Option<&'a str>,
    /// AVC/HEVC level ×10 (level 4.0 = 40). Not currently populated
    /// by the scanner; passing `None` makes the level check
    /// permissive.
    pub video_level: Option<u32>,
    pub audio_codec: Option<&'a str>,
    pub audio_channels: Option<u32>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

impl ClientProfile {
    pub fn supports_container(&self, container: Option<&str>) -> bool {
        let Some(c) = container else {
            // Source container unknown — be conservative and say no.
            return false;
        };
        self.containers
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(c))
    }

    pub fn supports_video(&self, codec: Option<&str>, level: Option<u32>) -> bool {
        let Some(c) = codec else { return false };
        self.video_codecs.iter().any(|cap| {
            cap.codec.eq_ignore_ascii_case(c)
                && cap
                    .level
                    .is_none_or(|max| level.is_none_or(|src| src <= max))
        })
    }

    pub fn supports_audio(&self, codec: Option<&str>, channels: Option<u32>) -> bool {
        let Some(c) = codec else { return false };
        self.audio_codecs.iter().any(|cap| {
            cap.codec.eq_ignore_ascii_case(c)
                && cap
                    .max_channels
                    .is_none_or(|max| channels.is_none_or(|src| src <= max))
        })
    }

    pub fn supports_resolution(&self, width: Option<u32>, height: Option<u32>) -> bool {
        let w_ok = match (self.max_width, width) {
            (None, _) => true,
            (Some(_), None) => true, // source resolution unknown — be permissive
            (Some(max), Some(src)) => src <= max,
        };
        let h_ok = match (self.max_height, height) {
            (None, _) => true,
            (Some(_), None) => true,
            (Some(max), Some(src)) => src <= max,
        };
        w_ok && h_ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h264_profile() -> ClientProfile {
        ClientProfile {
            containers: vec!["mp4".into()],
            video_codecs: vec![VideoCodecCap {
                codec: "h264".into(),
                profile: None,
                level: Some(40),
            }],
            audio_codecs: vec![AudioCodecCap {
                codec: "aac".into(),
                max_channels: Some(2),
            }],
            max_width: Some(1920),
            max_height: Some(1080),
            max_audio_channels: Some(2),
        }
    }

    #[test]
    fn container_match_is_case_insensitive() {
        let p = h264_profile();
        assert!(p.supports_container(Some("mp4")));
        assert!(p.supports_container(Some("MP4")));
        assert!(!p.supports_container(Some("mkv")));
        assert!(!p.supports_container(None));
    }

    #[test]
    fn video_level_ceiling_enforced() {
        let p = h264_profile();
        assert!(p.supports_video(Some("h264"), Some(31)));
        assert!(p.supports_video(Some("h264"), Some(40)));
        assert!(!p.supports_video(Some("h264"), Some(41)));
        // No level on either side → permissive.
        assert!(p.supports_video(Some("h264"), None));
        assert!(!p.supports_video(Some("hevc"), Some(30)));
    }

    #[test]
    fn audio_channel_ceiling_enforced() {
        let p = h264_profile();
        assert!(p.supports_audio(Some("aac"), Some(2)));
        assert!(!p.supports_audio(Some("aac"), Some(6)));
        assert!(!p.supports_audio(Some("dts"), Some(2)));
    }

    #[test]
    fn resolution_caps_height_and_width_independently() {
        let p = h264_profile();
        assert!(p.supports_resolution(Some(1920), Some(1080)));
        assert!(!p.supports_resolution(Some(3840), Some(2160)));
        assert!(p.supports_resolution(Some(720), Some(480)));
        // Missing side → permissive.
        assert!(p.supports_resolution(None, Some(1080)));
    }

    fn caps<'a>(
        container: &'a str,
        v: &'a str,
        a: &'a str,
        w: u32,
        h: u32,
    ) -> MediaCapabilities<'a> {
        MediaCapabilities {
            container: Some(container),
            video_codec: Some(v),
            video_level: None,
            audio_codec: Some(a),
            audio_channels: None,
            width: Some(w),
            height: Some(h),
        }
    }

    #[test]
    fn all_match_direct_play() {
        let p = h264_profile();
        let plan = decide(caps("mp4", "h264", "aac", 1280, 720), &p);
        assert_eq!(plan.mode, PlaybackMode::DirectPlay);
    }

    #[test]
    fn container_only_mismatch_is_remux() {
        let p = h264_profile();
        let plan = decide(caps("mkv", "h264", "aac", 1280, 720), &p);
        assert_eq!(plan.mode, PlaybackMode::Remux);
        assert!(!plan.container_ok);
    }

    #[test]
    fn audio_only_mismatch_is_transcode_audio() {
        let p = h264_profile();
        let plan = decide(caps("mp4", "h264", "dts", 1280, 720), &p);
        assert_eq!(plan.mode, PlaybackMode::TranscodeAudio);
    }

    #[test]
    fn video_only_mismatch_is_transcode_video() {
        let p = h264_profile();
        let plan = decide(caps("mp4", "hevc", "aac", 1280, 720), &p);
        assert_eq!(plan.mode, PlaybackMode::TranscodeVideo);
    }

    #[test]
    fn resolution_over_cap_forces_full_re_encode() {
        let p = h264_profile();
        // Codecs match but 4K > 1080p ceiling → must scale → full.
        let plan = decide(caps("mp4", "h264", "aac", 3840, 2160), &p);
        assert_eq!(plan.mode, PlaybackMode::TranscodeFull);
    }

    #[test]
    fn double_mismatch_falls_through_to_full() {
        let p = h264_profile();
        let plan = decide(caps("mkv", "hevc", "dts", 1280, 720), &p);
        assert_eq!(plan.mode, PlaybackMode::TranscodeFull);
    }
}
