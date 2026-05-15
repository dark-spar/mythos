//! FFmpeg/ffprobe binary resolution.
//!
//! By default we shell out to whatever `ffmpeg` / `ffprobe` is on PATH.
//! Operators can point Mythos at a specific build (for example
//! `jellyfin-ffmpeg`, which ships extra HW codecs not always present in
//! distro ffmpeg) by exporting:
//!
//! ```text
//! MYTHOS_FFMPEG_BIN=/usr/lib/jellyfin-ffmpeg/ffmpeg
//! MYTHOS_FFPROBE_BIN=/usr/lib/jellyfin-ffmpeg/ffprobe
//! ```
//!
//! Resolution happens per spawn so an operator restarting the server
//! with a new env value picks it up without code changes here.

/// Path / name of the `ffmpeg` binary to spawn. Falls back to `"ffmpeg"`
/// (resolved via PATH) when `MYTHOS_FFMPEG_BIN` is unset or empty.
pub fn ffmpeg_bin() -> String {
    env_or("MYTHOS_FFMPEG_BIN", "ffmpeg")
}

/// Path / name of the `ffprobe` binary to spawn. Falls back to
/// `"ffprobe"` when `MYTHOS_FFPROBE_BIN` is unset or empty.
pub fn ffprobe_bin() -> String {
    env_or("MYTHOS_FFPROBE_BIN", "ffprobe")
}

fn env_or(var: &str, default: &str) -> String {
    match std::env::var(var) {
        Ok(v) if !v.trim().is_empty() => v,
        _ => default.to_string(),
    }
}
