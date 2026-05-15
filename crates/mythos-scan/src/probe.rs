//! `ffprobe` wrapper.
//!
//! Spawns `ffprobe -v error -print_format json -show_format
//! -show_streams <path>` and parses the JSON into a [`Probe`].
//!
//! On any failure (binary missing, non-zero exit, malformed JSON) we
//! return an `Err`; the scan orchestrator is expected to log and store
//! the file with an empty `Probe` so operators can install ffmpeg later
//! and re-scan to fill in the technical fields.

use std::path::Path;

use mythos_core::{NewSubtitle, Probe, ffprobe_bin, is_image_subtitle_codec};
use serde::Deserialize;
use thiserror::Error;
use tokio::process::Command;

#[derive(Debug, Error)]
pub enum ProbeError {
    #[error("ffprobe failed to start: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("ffprobe exited with status {0}")]
    Status(std::process::ExitStatus),
    #[error("ffprobe returned non-utf-8 output")]
    NonUtf8,
    #[error("ffprobe returned malformed JSON: {0}")]
    Json(#[from] serde_json::Error),
}

pub async fn probe(path: &Path) -> Result<Probe, ProbeError> {
    let output = Command::new(ffprobe_bin())
        .arg("-v")
        .arg("error")
        .arg("-print_format")
        .arg("json")
        .arg("-show_format")
        .arg("-show_streams")
        .arg(path)
        .output()
        .await?;

    if !output.status.success() {
        return Err(ProbeError::Status(output.status));
    }
    let stdout = String::from_utf8(output.stdout).map_err(|_| ProbeError::NonUtf8)?;
    let parsed: FfProbeOutput = serde_json::from_str(&stdout)?;
    Ok(parsed.into_probe())
}

#[derive(Debug, Deserialize)]
struct FfProbeOutput {
    format: Option<Format>,
    #[serde(default)]
    streams: Vec<Stream>,
}

#[derive(Debug, Deserialize)]
struct Format {
    format_name: Option<String>,
    duration: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Stream {
    index: Option<i64>,
    codec_type: Option<String>,
    codec_name: Option<String>,
    width: Option<i64>,
    height: Option<i64>,
    #[serde(default)]
    tags: StreamTags,
    #[serde(default)]
    disposition: StreamDisposition,
}

#[derive(Debug, Default, Deserialize)]
struct StreamTags {
    language: Option<String>,
    title: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct StreamDisposition {
    #[serde(default)]
    default: i64,
    #[serde(default)]
    forced: i64,
}

impl FfProbeOutput {
    fn into_probe(self) -> Probe {
        let format = self.format.unwrap_or(Format {
            format_name: None,
            duration: None,
        });

        // ffprobe returns format_name as a comma-separated list for
        // containers that match multiple profiles (e.g.
        // "mov,mp4,m4a,3gp,3g2,mj2"); take the first token.
        let container = format.format_name.map(|s| {
            let first = s.split(',').next().unwrap_or(&s);
            first.trim().to_string()
        });
        let duration_seconds = format.duration.and_then(|s| s.parse::<f64>().ok());

        let video = self
            .streams
            .iter()
            .find(|s| s.codec_type.as_deref() == Some("video"));
        let audio = self
            .streams
            .iter()
            .find(|s| s.codec_type.as_deref() == Some("audio"));

        let subtitles = self
            .streams
            .iter()
            .filter(|s| s.codec_type.as_deref() == Some("subtitle"))
            .filter_map(|s| {
                let stream_index = s.index?;
                let codec = s.codec_name.clone()?;
                Some(NewSubtitle {
                    stream_index,
                    is_image: is_image_subtitle_codec(&codec),
                    codec,
                    language: s.tags.language.clone(),
                    title: s.tags.title.clone(),
                    is_default: s.disposition.default != 0,
                    is_forced: s.disposition.forced != 0,
                })
            })
            .collect();

        Probe {
            container,
            video_codec: video.and_then(|s| s.codec_name.clone()),
            audio_codec: audio.and_then(|s| s.codec_name.clone()),
            duration_seconds,
            width: video.and_then(|s| s.width),
            height: video.and_then(|s| s.height),
            subtitles,
        }
    }
}
