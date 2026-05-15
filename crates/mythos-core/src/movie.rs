use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::subtitle::NewSubtitle;

/// Technical metadata about a single media file, populated from ffprobe.
///
/// All fields are `Option` because ffprobe may be missing or fail on a
/// given file. The scanner stores the file row regardless so a future
/// re-scan can fill in missing fields.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Probe {
    pub container: Option<String>,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub duration_seconds: Option<f64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    /// Raw ffprobe `color_primaries` (e.g. `"bt2020"`, `"bt709"`).
    /// Used together with [`color_transfer`](Self::color_transfer) to
    /// decide whether to apply HDR→SDR tonemapping when transcoding.
    #[serde(default)]
    pub color_primaries: Option<String>,
    /// Raw ffprobe `color_transfer` (e.g. `"smpte2084"` for HDR10 PQ,
    /// `"arib-std-b67"` for HLG, `"bt709"` for SDR). Load-bearing
    /// signal for HDR detection — see [`Probe::is_hdr`].
    #[serde(default)]
    pub color_transfer: Option<String>,
    /// Raw ffprobe `color_space` (e.g. `"bt2020nc"`, `"bt709"`).
    /// Stored for completeness; not currently consulted by the
    /// transcode-decision code.
    #[serde(default)]
    pub color_space: Option<String>,
    /// Subtitle streams found by ffprobe. Empty when no tracks exist
    /// or ffprobe failed; the scanner clears existing rows and
    /// reinserts these on every successful re-scan.
    #[serde(default)]
    pub subtitles: Vec<NewSubtitle>,
}

impl Probe {
    /// True when the source carries an HDR transfer function — PQ
    /// (HDR10 / Dolby Vision base layer) or HLG. The transfer
    /// characteristic is the actually load-bearing signal: BT.2020
    /// primaries alone don't make a stream HDR, but `smpte2084` or
    /// `arib-std-b67` transfer does.
    pub fn is_hdr(&self) -> bool {
        matches!(
            self.color_transfer.as_deref(),
            Some("smpte2084" | "arib-std-b67")
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaFile {
    pub id: Uuid,
    pub library_id: Uuid,
    /// Path relative to the owning library's `root_path`.
    pub path: PathBuf,
    pub size_bytes: i64,
    pub mtime: DateTime<Utc>,
    #[serde(flatten)]
    pub probe: Probe,
    pub scanned_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewMediaFile {
    pub library_id: Uuid,
    pub path: PathBuf,
    pub size_bytes: i64,
    pub mtime: DateTime<Utc>,
    pub probe: Probe,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Movie {
    pub id: Uuid,
    pub library_id: Uuid,
    pub file_id: Uuid,
    pub title: String,
    pub sort_title: String,
    pub year: Option<i64>,
    pub tmdb_id: Option<i64>,
    pub overview: Option<String>,
    pub poster_url: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewMovie {
    pub library_id: Uuid,
    pub file_id: Uuid,
    pub title: String,
    pub year: Option<i64>,
}

/// Per-user playback resume point.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchProgress {
    pub position_seconds: f64,
    pub duration_seconds: f64,
    pub updated_at: DateTime<Utc>,
}

/// Build a sort key that ignores leading articles ("The", "A", "An").
///
/// "The Matrix" → "Matrix, The". This keeps browse grids alphabetically
/// useful without surfacing every "The" at the same place.
pub fn sort_title(title: &str) -> String {
    let lower = title.trim();
    for prefix in ["The ", "A ", "An "] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            let trimmed = prefix.trim_end();
            return format!("{rest}, {trimmed}");
        }
    }
    lower.to_string()
}
