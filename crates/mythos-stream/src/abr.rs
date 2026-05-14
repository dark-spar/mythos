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

    /// Conservative codec string for H.264 Main + AAC-LC. Real codec
    /// strings would encode the AVC level too, but most player ABR
    /// logic only cares about the profile name.
    pub fn codecs_attr(&self) -> &'static str {
        "avc1.4d401f,mp4a.40.2"
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

pub fn rendition_by_name(name: &str) -> Option<&'static Rendition> {
    ABR_LADDER.iter().find(|r| r.name == name)
}

pub fn is_known_variant(name: &str) -> bool {
    rendition_by_name(name).is_some()
}

/// The variant used as a default when one isn't otherwise selected
/// (e.g., diagnostic tools, tests, the rare client that ignores the
/// master playlist).
pub fn default_variant() -> &'static Rendition {
    &ABR_LADDER[ABR_LADDER.len() / 2]
}
