//! Hardware-accelerated H.264 encoder detection.
//!
//! At server startup we probe what `ffmpeg -encoders` advertises, then
//! smoke-test each candidate with a tiny synthetic encode so a build
//! that has the encoder compiled in but the GPU/driver doesn't
//! actually work falls back cleanly to CPU. Smoke-testing is
//! non-trivial: on a desktop with the GPU passed through, NVENC may
//! work but the running user lacks `/dev/dri` permissions; or libva
//! is present but the userspace driver
//! (`intel-media-driver` / `mesa-va-drivers`) isn't installed and
//! `vaInitialize` returns -1.
//!
//! Priority order (best first):
//! 1. NVENC (NVIDIA dedicated GPU — fastest in absolute terms)
//! 2. QSV (Intel iGPU via oneVPL/MSDK — fast and very common)
//! 3. VAAPI (generic Linux acceleration — works on Intel + AMD)
//! 4. VideoToolbox (macOS native)
//! 5. CPU fallback (always available)
//!
//! Operators can override via `MYTHOS_HW_ENCODER`:
//! - `auto` (default): probe + smoke-test, pick the best working one.
//! - `cpu`: force software libx264 even if hardware is available.
//! - `nvenc` / `qsv` / `vaapi` / `videotoolbox`: pin a specific
//!   encoder, smoke-test it once, fail to start if it doesn't work.

use std::process::Stdio;

use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tracing::{debug, info, warn};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HwAccel {
    Nvenc,
    Qsv,
    Vaapi,
    VideoToolbox,
    /// Software libx264. Always works as long as ffmpeg is on PATH.
    Cpu,
}

impl HwAccel {
    pub fn as_str(self) -> &'static str {
        match self {
            HwAccel::Nvenc => "nvenc",
            HwAccel::Qsv => "qsv",
            HwAccel::Vaapi => "vaapi",
            HwAccel::VideoToolbox => "videotoolbox",
            HwAccel::Cpu => "cpu",
        }
    }

    /// Encoder name passed to `-c:v`.
    pub fn h264_encoder(self) -> &'static str {
        match self {
            HwAccel::Nvenc => "h264_nvenc",
            HwAccel::Qsv => "h264_qsv",
            HwAccel::Vaapi => "h264_vaapi",
            HwAccel::VideoToolbox => "h264_videotoolbox",
            HwAccel::Cpu => "libx264",
        }
    }

    /// Flags that go BEFORE `-i` to enable HW-accelerated decode where
    /// available. Returned as a flat `Vec<&str>` so the caller can
    /// `cmd.args(...)` them.
    ///
    /// For most production HEVC->H.264 transcodes the decode side is
    /// the bigger CPU cost — pulling HEVC frames out of an mkv on
    /// the CPU then handing them to GPU encode still costs an entire
    /// core. With HW decode the whole pipeline lives on the GPU.
    pub fn decode_args(self) -> &'static [&'static str] {
        match self {
            HwAccel::Qsv => &["-hwaccel", "qsv", "-hwaccel_output_format", "qsv"],
            HwAccel::Vaapi => &[
                "-hwaccel",
                "vaapi",
                "-vaapi_device",
                "/dev/dri/renderD128",
                "-hwaccel_output_format",
                "vaapi",
            ],
            HwAccel::Nvenc => &["-hwaccel", "cuda", "-hwaccel_output_format", "cuda"],
            HwAccel::VideoToolbox => &["-hwaccel", "videotoolbox"],
            HwAccel::Cpu => &[],
        }
    }

    /// Encoder-specific quality / preset flags + any video filter
    /// needed to feed frames into the encoder. Returned as a
    /// `Vec<String>` because some flags carry numeric values; the
    /// caller `cmd.args(...)`-es them.
    pub fn encode_args(self) -> Vec<String> {
        match self {
            HwAccel::Cpu => vec![
                "-c:v".into(),
                "libx264".into(),
                "-preset".into(),
                "veryfast".into(),
                "-crf".into(),
                "23".into(),
                "-pix_fmt".into(),
                "yuv420p".into(),
                "-profile:v".into(),
                "main".into(),
            ],
            HwAccel::Qsv => vec![
                "-c:v".into(),
                "h264_qsv".into(),
                "-preset".into(),
                "veryfast".into(),
                "-global_quality".into(),
                "23".into(),
            ],
            HwAccel::Vaapi => vec![
                // `format=nv12|vaapi,hwupload` accepts frames already on the
                // GPU (when HW decode is active) and uploads from CPU when
                // they aren't, so the same args work whether or not the
                // input codec is HW-decodable.
                "-vf".into(),
                "format=nv12|vaapi,hwupload".into(),
                "-c:v".into(),
                "h264_vaapi".into(),
                "-qp".into(),
                "23".into(),
            ],
            HwAccel::Nvenc => vec![
                "-c:v".into(),
                "h264_nvenc".into(),
                "-preset".into(),
                "p4".into(),
                "-cq".into(),
                "23".into(),
            ],
            HwAccel::VideoToolbox => vec![
                "-c:v".into(),
                "h264_videotoolbox".into(),
                "-q:v".into(),
                "50".into(),
            ],
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DetectError {
    #[error("ffmpeg not available: {0}")]
    NoFfmpeg(std::io::Error),
    #[error("user requested encoder '{0}' but it failed its smoke test")]
    PinnedEncoderUnavailable(String),
}

/// Resolve which encoder to use based on `MYTHOS_HW_ENCODER` and
/// runtime availability. `mode` is the value of the env var, or
/// `"auto"` if unset.
pub async fn resolve(mode: &str) -> Result<HwAccel, DetectError> {
    let normalized = mode.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "" | "auto" => Ok(auto_detect().await),
        "cpu" | "off" | "none" | "software" | "libx264" => {
            info!("HW_ENCODER=cpu — using software libx264");
            Ok(HwAccel::Cpu)
        }
        "nvenc" | "h264_nvenc" => pin(HwAccel::Nvenc).await,
        "qsv" | "h264_qsv" => pin(HwAccel::Qsv).await,
        "vaapi" | "h264_vaapi" => pin(HwAccel::Vaapi).await,
        "videotoolbox" | "h264_videotoolbox" => pin(HwAccel::VideoToolbox).await,
        other => {
            warn!("MYTHOS_HW_ENCODER={other:?} is not recognised; falling back to auto-detect");
            Ok(auto_detect().await)
        }
    }
}

async fn pin(accel: HwAccel) -> Result<HwAccel, DetectError> {
    if smoke_test(accel).await {
        info!(accel = accel.as_str(), "HW encoder pinned and verified");
        Ok(accel)
    } else {
        Err(DetectError::PinnedEncoderUnavailable(
            accel.h264_encoder().to_string(),
        ))
    }
}

/// Try each hardware encoder in priority order. The first one whose
/// smoke test passes wins. CPU is the unconditional fallback.
async fn auto_detect() -> HwAccel {
    let available = match list_encoders().await {
        Ok(set) => set,
        Err(err) => {
            warn!(?err, "couldn't list ffmpeg encoders; falling back to CPU");
            return HwAccel::Cpu;
        }
    };

    for candidate in [
        HwAccel::Nvenc,
        HwAccel::Qsv,
        HwAccel::Vaapi,
        HwAccel::VideoToolbox,
    ] {
        if !available.iter().any(|e| e == candidate.h264_encoder()) {
            debug!(
                accel = candidate.as_str(),
                "encoder not compiled into ffmpeg"
            );
            continue;
        }
        if smoke_test(candidate).await {
            info!(accel = candidate.as_str(), "HW encoder selected");
            return candidate;
        } else {
            info!(
                accel = candidate.as_str(),
                "encoder compiled in but smoke test failed (driver / device probably missing)"
            );
        }
    }

    info!("no working hardware encoder found; using software libx264");
    HwAccel::Cpu
}

/// Parse `ffmpeg -encoders` for the H.264 encoders it advertises.
/// Returns just the encoder names (e.g. "libx264", "h264_qsv").
async fn list_encoders() -> Result<Vec<String>, DetectError> {
    let mut child = Command::new("ffmpeg")
        .arg("-hide_banner")
        .arg("-encoders")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(DetectError::NoFfmpeg)?;

    let mut stdout = child.stdout.take().expect("stdout piped");
    let mut buf = String::new();
    stdout.read_to_string(&mut buf).await.ok();
    let _ = child.wait().await;

    let mut names = Vec::new();
    for line in buf.lines() {
        // Lines look like: " V....D h264_nvenc           NVIDIA NVENC …"
        // The encoder name is the second whitespace-separated token.
        let trimmed = line.trim_start();
        if !trimmed.starts_with('V') {
            continue;
        }
        let mut parts = trimmed.split_whitespace();
        let _flags = parts.next();
        if let Some(name) = parts.next()
            && (name == "libx264" || name.starts_with("h264_"))
        {
            names.push(name.to_string());
        }
    }
    Ok(names)
}

/// Encode a few frames of a synthetic color source through `accel`.
/// If ffmpeg exits 0, the encoder + GPU + driver chain works on this
/// host. Synthetic input means smoke-test passing doesn't guarantee
/// hardware decode of arbitrary container formats — but encoder
/// failures (wrong driver / missing libraries / permission errors on
/// `/dev/dri`) all surface here.
async fn smoke_test(accel: HwAccel) -> bool {
    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-y");
    // VAAPI needs the device handle before any input. Other encoders
    // attach their decode flags here but the smoke test uses lavfi
    // (always software-decoded) so HW decode args would only confuse
    // ffmpeg. Pick `-vaapi_device` for VAAPI only, since that's the
    // device-binding flag (not strictly an HW decode flag).
    if accel == HwAccel::Vaapi {
        cmd.arg("-vaapi_device").arg("/dev/dri/renderD128");
    }
    cmd.arg("-f")
        .arg("lavfi")
        .arg("-i")
        .arg("color=red:size=160x90:duration=0.3:rate=10");
    cmd.args(accel.encode_args());
    cmd.arg("-frames:v").arg("3").arg("-f").arg("null").arg("-");

    cmd.stdout(Stdio::null()).stderr(Stdio::null());

    let result = tokio::time::timeout(std::time::Duration::from_secs(10), cmd.status()).await;
    match result {
        Ok(Ok(status)) => status.success(),
        Ok(Err(_)) | Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hwaccel_strings_round_trip() {
        for accel in [
            HwAccel::Nvenc,
            HwAccel::Qsv,
            HwAccel::Vaapi,
            HwAccel::VideoToolbox,
            HwAccel::Cpu,
        ] {
            assert!(!accel.as_str().is_empty());
            assert!(!accel.h264_encoder().is_empty());
        }
    }

    #[tokio::test]
    async fn cpu_mode_short_circuits_detection() {
        let chosen = resolve("cpu").await.unwrap();
        assert_eq!(chosen, HwAccel::Cpu);
    }

    #[tokio::test]
    async fn unknown_mode_falls_back_to_auto() {
        // Even with a bogus mode, resolve() must return *something* —
        // it falls through to auto_detect which always succeeds (at
        // worst with HwAccel::Cpu).
        let chosen = resolve("definitely-not-a-mode").await.unwrap();
        let _ = chosen.as_str();
    }
}
