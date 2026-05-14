use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A subtitle track exposed by a single media file.
///
/// Text vs image is the key distinction at the serving layer:
/// text tracks become a WebVTT sidecar that the `<video>` element
/// renders client-side; image tracks have to be burned into the
/// transcoded video stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubtitleTrack {
    pub id: Uuid,
    pub file_id: Uuid,
    pub stream_index: i64,
    pub codec: String,
    pub language: Option<String>,
    pub title: Option<String>,
    pub is_image: bool,
    pub is_default: bool,
    pub is_forced: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewSubtitle {
    pub stream_index: i64,
    pub codec: String,
    pub language: Option<String>,
    pub title: Option<String>,
    pub is_image: bool,
    pub is_default: bool,
    pub is_forced: bool,
}

/// Codecs whose subtitles are pre-rasterized bitmaps rather than
/// timed text. The serving layer routes these into the burn-in path
/// because there's no standard browser API for displaying them as a
/// sidecar.
const IMAGE_SUBTITLE_CODECS: &[&str] = &[
    "hdmv_pgs_subtitle", // PGS — Blu-ray
    "dvd_subtitle",      // VOBSUB — DVD
    "dvb_subtitle",      // DVB digital broadcast
    "xsub",              // DivX
];

pub fn is_image_subtitle_codec(codec: &str) -> bool {
    let lower = codec.to_ascii_lowercase();
    IMAGE_SUBTITLE_CODECS.contains(&lower.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_codec_classifier() {
        assert!(is_image_subtitle_codec("hdmv_pgs_subtitle"));
        assert!(is_image_subtitle_codec("HDMV_PGS_SUBTITLE"));
        assert!(is_image_subtitle_codec("dvd_subtitle"));
        assert!(!is_image_subtitle_codec("subrip"));
        assert!(!is_image_subtitle_codec("ass"));
        assert!(!is_image_subtitle_codec("mov_text"));
        assert!(!is_image_subtitle_codec(""));
    }
}
