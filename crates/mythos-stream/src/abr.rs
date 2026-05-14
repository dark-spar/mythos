//! Adaptive bitrate ladder.
//!
//! ffmpeg produces every [`Rendition`] concurrently from a single
//! decode pass. The client (hls.js) sees them via the master playlist
//! and picks one based on measured bandwidth. Source-resolution-aware
//! pruning (skip renditions strictly larger than the source) is a
//! follow-up; for now we encode all three regardless.
//!
//! Bitrate targets are tuned for content delivered over a WAN
//! connection — local-network users will rarely down-shift, but
//! mobile and outside viewers will appreciate the 480p tier.

#[derive(Debug, Clone, Copy)]
pub struct Rendition {
    pub name: &'static str,
    pub width: u32,
    pub height: u32,
    /// Video target bitrate in kbps.
    pub video_bitrate_kbps: u32,
    /// Audio target bitrate in kbps.
    pub audio_bitrate_kbps: u32,
}

impl Rendition {
    /// Hint to hls.js / the player about the variant's bandwidth.
    /// Slightly higher than the sum of audio + video to account for
    /// container overhead; matches the convention used by ffmpeg's
    /// own master-playlist BANDWIDTH attribute.
    pub fn declared_bandwidth_bps(&self) -> u32 {
        (self.video_bitrate_kbps + self.audio_bitrate_kbps) * 1100
    }

    /// Codec string for H.264 Main + AAC-LC, with the AVC level
    /// scaled to the rendition's resolution. MSE-based players
    /// (hls.js + Firefox/Chrome) check whether the decoder advertises
    /// support for the level in the codec string before creating a
    /// SourceBuffer; advertising 3.1 for every variant would falsely
    /// imply 720p-max, and some implementations reject the buffer
    /// when the actual stream exceeds that level.
    ///
    /// Levels:
    /// - 3.0 (`1e`): up to 720×480@30fps
    /// - 3.1 (`1f`): up to 1280×720@30fps
    /// - 4.0 (`28`): up to 1920×1080@30fps
    pub fn codecs_attr(&self) -> &'static str {
        match self.height {
            h if h <= 480 => "avc1.4d401e,mp4a.40.2",
            h if h <= 720 => "avc1.4d401f,mp4a.40.2",
            _ => "avc1.4d4028,mp4a.40.2",
        }
    }
}

pub const ABR_LADDER: &[Rendition] = &[
    Rendition {
        name: "480p",
        width: 854,
        height: 480,
        video_bitrate_kbps: 1500,
        audio_bitrate_kbps: 96,
    },
    Rendition {
        name: "720p",
        width: 1280,
        height: 720,
        video_bitrate_kbps: 3000,
        audio_bitrate_kbps: 128,
    },
    Rendition {
        name: "1080p",
        width: 1920,
        height: 1080,
        video_bitrate_kbps: 6000,
        audio_bitrate_kbps: 128,
    },
];

/// Variant name reserved for copy-mode sessions (Remux,
/// TranscodeAudio) where there's no scaling and only one output. The
/// master playlist for those modes lists a single STREAM-INF
/// pointing at this name.
pub const SOURCE_VARIANT: &str = "source";

pub fn rendition_by_name(name: &str) -> Option<&'static Rendition> {
    ABR_LADDER.iter().find(|r| r.name == name)
}

pub fn is_known_variant(name: &str) -> bool {
    rendition_by_name(name).is_some() || name == SOURCE_VARIANT
}

/// Build a synthetic rendition representing the source as-is. Used
/// in copy modes (Remux, TranscodeAudio) where ffmpeg passes pixels
/// through without scaling. The bandwidth hint is the source's
/// average bitrate, estimated from `size_bytes / duration_seconds`.
pub fn source_rendition(
    width: u32,
    height: u32,
    size_bytes: u64,
    duration_seconds: f64,
) -> Rendition {
    let avg_kbps = if duration_seconds > 0.0 {
        ((size_bytes as f64 * 8.0 / duration_seconds) / 1000.0) as u32
    } else {
        6000
    };
    Rendition {
        name: SOURCE_VARIANT,
        width: width.max(1),
        height: height.max(1),
        // Carry the source-average split arbitrarily across video +
        // audio for the bandwidth-hint calculation; we don't use the
        // individual fields when copying.
        video_bitrate_kbps: avg_kbps.saturating_sub(128).max(1),
        audio_bitrate_kbps: 128,
    }
}

/// The variant used as a default when one isn't otherwise selected
/// (e.g., diagnostic tools, tests, the rare client that ignores the
/// master playlist).
pub fn default_variant() -> &'static Rendition {
    &ABR_LADDER[ABR_LADDER.len() / 2]
}
