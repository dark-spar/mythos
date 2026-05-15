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

use mythos_core::ffmpeg_bin;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tracing::{debug, info, warn};

use crate::abr::Rendition;

/// Tonemap operator chosen by the operator from the admin settings.
/// All four names map onto the CPU `tonemap` filter's `tonemap=`
/// option and the NVENC `tonemap_cuda` filter's `tonemap=` option;
/// VAAPI's `tonemap_vaapi` filter has no algorithm knob (the
/// driver picks its own curve) so this selection is ignored when
/// the active encoder is VAAPI on the Hardware pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TonemapAlgorithm {
    /// Filmic curve from John Hable's "Uncharted 2" presentation —
    /// the default. Generally the best-looking trade-off across
    /// content types and the de facto industry default.
    #[default]
    Hable,
    /// Less contrast crushing than Hable; preserves more detail in
    /// extreme highlights at the cost of looking slightly flatter.
    Mobius,
    /// Simple Reinhard operator. Conservative, slightly washed out
    /// next to Hable but very stable.
    Reinhard,
    /// ITU-R BT.2390 reference tone-mapping. Broadcast-style result,
    /// requires a recent ffmpeg with the BT.2390 mode compiled in.
    Bt2390,
}

impl TonemapAlgorithm {
    pub fn as_str(self) -> &'static str {
        match self {
            TonemapAlgorithm::Hable => "hable",
            TonemapAlgorithm::Mobius => "mobius",
            TonemapAlgorithm::Reinhard => "reinhard",
            TonemapAlgorithm::Bt2390 => "bt2390",
        }
    }

    /// Parse an admin-supplied algorithm name. Unknown values fall
    /// back to [`TonemapAlgorithm::Hable`] so a stale DB value
    /// doesn't break playback.
    pub fn from_str_or_default(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "mobius" => TonemapAlgorithm::Mobius,
            "reinhard" => TonemapAlgorithm::Reinhard,
            "bt2390" => TonemapAlgorithm::Bt2390,
            _ => TonemapAlgorithm::Hable,
        }
    }
}

/// Where the HDR→SDR conversion runs in the filter graph.
///
/// The CPU `zscale`+`tonemap` chain is high-quality but expensive —
/// on a 4K HDR source it can saturate several cores. The hardware
/// paths (`tonemap_cuda` for NVENC, `tonemap_vaapi` for VAAPI) keep
/// frames in GPU memory and shed almost all of that CPU load, at the
/// cost of being dependent on the operator's ffmpeg build and GPU
/// drivers — some distros ship ffmpeg without CUDA tonemap support
/// and there's no clean way to detect that ahead of time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TonemapPipeline {
    /// HW tonemap when the active encoder supports it (NVENC,
    /// VAAPI); CPU fallback for libx264 / QSV / VideoToolbox where
    /// no widely-shipping HW tonemap filter exists. The default —
    /// matches the encoder you've already picked.
    #[default]
    Hardware,
    /// Force CPU tonemap regardless of encoder. Use when the GPU
    /// path's output looks wrong (drivers vary) or when the ffmpeg
    /// build is missing `tonemap_cuda` / `tonemap_vaapi`. Adds the
    /// `hwdownload` → CPU chain → `hwupload` round-trip on
    /// hardware encoders so the GPU encoder still runs.
    Software,
}

impl TonemapPipeline {
    pub fn as_str(self) -> &'static str {
        match self {
            TonemapPipeline::Hardware => "hardware",
            TonemapPipeline::Software => "software",
        }
    }

    /// Parse an admin-supplied pipeline name. Unknown values fall
    /// back to [`TonemapPipeline::Hardware`] — same reason as
    /// [`TonemapAlgorithm::from_str_or_default`].
    pub fn from_str_or_default(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "software" | "cpu" => TonemapPipeline::Software,
            _ => TonemapPipeline::Hardware,
        }
    }
}

/// Whether and how to perform HDR→SDR tonemapping inside the filter
/// graph. Computed per-session from the admin settings + the source's
/// detected HDR transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TonemapConfig {
    /// `true` when both: the source is HDR (PQ or HLG transfer) AND
    /// the operator hasn't disabled tonemapping in settings.
    pub apply: bool,
    pub algorithm: TonemapAlgorithm,
    pub pipeline: TonemapPipeline,
}

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

    /// Flags that go BEFORE `-i`. For VAAPI and NVENC we enable the
    /// full HW pipeline (decode on GPU, surfaces stay on GPU, encode
    /// on GPU) because SW decode of 1080p+ HEVC pins a CPU core,
    /// undoing most of the win from HW encode. The 10-bit format
    /// conversion happens on-GPU in `scale_filter`
    /// (`scale_vaapi=format=nv12` / `scale_cuda=format=yuv420p`)
    /// before the encoder, which is required because h264_nvenc /
    /// h264_vaapi only emit 8-bit.
    ///
    /// If your library has a codec the GPU can't HW-decode, ffmpeg
    /// will error on the input; falling back to `MYTHOS_HW_ENCODER=cpu`
    /// recovers cleanly.
    pub fn decode_args(self) -> &'static [&'static str] {
        match self {
            HwAccel::Vaapi => &[
                "-hwaccel",
                "vaapi",
                "-vaapi_device",
                "/dev/dri/renderD128",
                "-hwaccel_output_format",
                "vaapi",
            ],
            HwAccel::Nvenc => &["-hwaccel", "cuda", "-hwaccel_output_format", "cuda"],
            HwAccel::Qsv | HwAccel::VideoToolbox | HwAccel::Cpu => &[],
        }
    }

    /// Single-rendition encode args used only by the smoke test, where
    /// we just need any working encode to verify the GPU + driver
    /// chain. Production transcoding uses
    /// [`Self::abr_video_encoder_args`] with per-variant bitrate flags.
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
                "-vf".into(),
                "scale_vaapi=format=nv12".into(),
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

    /// HDR→SDR tonemap chain inserted **before** the split fan-out.
    /// Empty string when [`TonemapConfig::apply`] is false so the
    /// filter graph builder can paste it in unconditionally.
    ///
    /// Two pipelines:
    ///
    /// - [`TonemapPipeline::Hardware`] — `tonemap_cuda` for NVENC,
    ///   `tonemap_vaapi` for VAAPI. Frames stay on the GPU; almost
    ///   no CPU load on top of the HW encode. Requires the operator's
    ///   ffmpeg to have the relevant filter compiled in (most
    ///   distro ffmpegs do for VAAPI; CUDA tonemap support varies).
    ///
    /// - [`TonemapPipeline::Software`] — CPU `zscale`+`tonemap`
    ///   chain. Wrapped in `hwdownload`/`hwupload` on hardware
    ///   encoders so the encoder still runs on the GPU; pays the
    ///   round-trip on HDR content but is portable across ffmpeg
    ///   builds.
    ///
    /// For encoders without a usable HW tonemap filter (libx264,
    /// QSV, VideoToolbox) the pipeline choice is moot — both modes
    /// emit the CPU chain inline.
    pub fn tonemap_prefilter(self, cfg: TonemapConfig) -> String {
        if !cfg.apply {
            return String::new();
        }
        let algo = cfg.algorithm.as_str();
        // CPU chain — used as either the inline filter (CPU/QSV/VT)
        // or the meat of the hwdownload→...→hwupload wrap (Software
        // pipeline on HW encoders).
        //
        // 1. zscale to linear light (PQ/HLG → linear).
        // 2. float pixel format — `tonemap` requires float input.
        // 3. zscale primaries BT.2020 → BT.709.
        // 4. tonemap with the operator-chosen curve. desat=0 keeps
        //    saturation untouched; the curve already handles luma.
        // 5. zscale back to BT.709 transfer + matrix at TV range.
        // 6. format=yuv420p so the downstream encoder / hwupload gets
        //    an 8-bit planar input.
        let cpu = format!(
            "zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,\
             tonemap=tonemap={algo}:desat=0,\
             zscale=t=bt709:m=bt709:r=tv,format=yuv420p"
        );
        match (self, cfg.pipeline) {
            // CPU-only encoders — no GPU to download from. Always
            // inline.
            (HwAccel::Cpu | HwAccel::Qsv | HwAccel::VideoToolbox, _) => cpu,

            // NVENC + HW: `tonemap_cuda` keeps frames on the GPU.
            // Matrix/primaries/transfer pinned to BT.709 so the
            // output is correct SDR; `format=yuv420p` matches what
            // h264_nvenc accepts (no 10-bit on H.264).
            (HwAccel::Nvenc, TonemapPipeline::Hardware) => format!(
                "tonemap_cuda=tonemap={algo}:desat=0:\
                 transfer=bt709:matrix=bt709:primaries=bt709:\
                 format=yuv420p"
            ),
            // NVENC + SW: round-trip through system memory.
            (HwAccel::Nvenc, TonemapPipeline::Software) => {
                format!("hwdownload,format=p010le,{cpu},hwupload_cuda")
            }

            // VAAPI + HW: `tonemap_vaapi` doesn't take an algorithm
            // option — the Intel/AMD driver picks one (typically a
            // BT.2390-ish curve). The algorithm setting is ignored
            // on this branch; documented in the UI hint.
            (HwAccel::Vaapi, TonemapPipeline::Hardware) => {
                "tonemap_vaapi=format=nv12:matrix=bt709:primaries=bt709:transfer=bt709".to_string()
            }
            // VAAPI + SW: round-trip + nv12 before reupload because
            // `scale_vaapi` expects nv12 surfaces downstream.
            (HwAccel::Vaapi, TonemapPipeline::Software) => {
                format!("hwdownload,format=p010le,{cpu},format=nv12,hwupload")
            }
        }
    }

    /// Name of the GPU-side HDR→SDR filter for this encoder, or
    /// `None` if no widely-shipped HW tonemap filter exists for it.
    /// `tonemap_cuda` for NVENC and `tonemap_vaapi` for VAAPI are
    /// the two we use; everything else falls back to CPU regardless
    /// of pipeline.
    pub fn hw_tonemap_filter_name(self) -> Option<&'static str> {
        match self {
            HwAccel::Nvenc => Some("tonemap_cuda"),
            HwAccel::Vaapi => Some("tonemap_vaapi"),
            HwAccel::Qsv | HwAccel::VideoToolbox | HwAccel::Cpu => None,
        }
    }

    /// Per-rendition scale filter, applied inside the
    /// `-filter_complex` graph after a `split` fan-out. CPU uses
    /// `scale`; VAAPI uses `scale_vaapi` and NVENC uses `scale_cuda`
    /// so the resize happens on the GPU and frames never round-trip
    /// to system memory. The output of this filter must accept the
    /// encoder's input format (NV12 for VAAPI; yuv420p elsewhere).
    ///
    /// The 8-bit format tail on every path forces a 10→8 downconvert
    /// when the source is 10-bit (HEVC Main10 etc). H.264 hardware
    /// encoders don't accept 10-bit pixel formats; without this
    /// `h264_nvenc` errors with "10 bit encode not supported / No
    /// capable devices found".
    ///
    /// Width is `-2` (auto from source aspect, snapped to an even
    /// number) so non-16:9 prints (Cinerama 2.20:1, IMAX 1.43:1,
    /// Academy 1.37:1, …) don't get horizontally squeezed into our
    /// nominal 16:9 box. The actual output width depends on the
    /// source's display aspect ratio, which is what the player should
    /// render at; height is the dimension we actually want to pin per
    /// rendition.
    pub fn scale_filter(self, rendition: &Rendition) -> String {
        match self {
            HwAccel::Vaapi => format!("scale_vaapi=w=-2:h={}:format=nv12", rendition.height),
            HwAccel::Nvenc => format!("scale_cuda=w=-2:h={}:format=yuv420p", rendition.height),
            HwAccel::Cpu | HwAccel::Qsv | HwAccel::VideoToolbox => {
                format!("scale=w=-2:h={},format=yuv420p", rendition.height)
            }
        }
    }

    /// Per-variant video encoder args, indexed by output position so
    /// `-c:v:N`/`-b:v:N`/etc. apply to the right output stream.
    ///
    /// When `tonemap.apply` is true the pixels have been remapped to
    /// SDR BT.709 by the filter graph, but ffmpeg would otherwise copy
    /// the source's HDR color tags (`bt2020nc` / `smpte2084`) straight
    /// into the output H.264 VUI parameters — players and TVs then
    /// apply an HDR→SDR transform on already-SDR pixels, which is the
    /// classic "washed out / oversaturated" symptom (most visible on
    /// VAAPI, where `tonemap_vaapi` only updates surface metadata that
    /// `h264_vaapi` doesn't forward to the encoded SPS). We force the
    /// SDR tags onto every rendition's output stream so the bitstream
    /// matches the pixels.
    pub fn abr_video_encoder_args(
        self,
        output_index: usize,
        rendition: &Rendition,
        tonemap: TonemapConfig,
    ) -> Vec<String> {
        let kbps = rendition.video_bitrate_kbps;
        // Standard VBV bracket: target bitrate, maxrate ~10% above,
        // bufsize = 2× target. Keeps the encoder honest about
        // bitrate without locking it to CBR.
        let maxrate = (kbps * 110) / 100;
        let bufsize = kbps * 2;

        let mut args = vec![
            format!("-c:v:{output_index}"),
            self.h264_encoder().into(),
            format!("-b:v:{output_index}"),
            format!("{kbps}k"),
            format!("-maxrate:v:{output_index}"),
            format!("{maxrate}k"),
            format!("-bufsize:v:{output_index}"),
            format!("{bufsize}k"),
        ];

        match self {
            HwAccel::Cpu => {
                args.extend([
                    format!("-preset:v:{output_index}"),
                    "veryfast".into(),
                    format!("-profile:v:{output_index}"),
                    "main".into(),
                    format!("-pix_fmt:v:{output_index}"),
                    "yuv420p".into(),
                ]);
            }
            HwAccel::Qsv => {
                args.extend([format!("-preset:v:{output_index}"), "veryfast".into()]);
            }
            HwAccel::Nvenc => {
                args.extend([format!("-preset:v:{output_index}"), "p4".into()]);
            }
            HwAccel::Vaapi | HwAccel::VideoToolbox => {}
        }

        if tonemap.apply {
            args.extend([
                format!("-color_primaries:v:{output_index}"),
                "bt709".into(),
                format!("-color_trc:v:{output_index}"),
                "bt709".into(),
                format!("-colorspace:v:{output_index}"),
                "bt709".into(),
                format!("-color_range:v:{output_index}"),
                "tv".into(),
            ]);
        }

        args
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

/// Does ffmpeg have the GPU tonemap filter for `accel` compiled in?
///
/// `tonemap_cuda` / `tonemap_vaapi` aren't part of the ffmpeg
/// baseline — many distro packages ship without them. We probe once
/// at startup by reading `ffmpeg -filters` so the HLS handler can
/// downgrade `TonemapPipeline::Hardware` to `Software` silently when
/// the chosen filter isn't available, rather than letting the
/// transcode session 504 on every play attempt.
///
/// Returns `true` when:
/// - `accel.hw_tonemap_filter_name()` is `None` (no HW tonemap was
///   ever going to be used — vacuously fine), OR
/// - ffmpeg's `-filters` output contains the relevant filter name.
///
/// Returns `false` only when there's a named GPU filter we *would*
/// use but ffmpeg doesn't have it.
pub async fn probe_hw_tonemap_support(accel: HwAccel) -> bool {
    let Some(filter) = accel.hw_tonemap_filter_name() else {
        return true;
    };
    match list_filters().await {
        Ok(names) => {
            let present = names.iter().any(|n| n == filter);
            if !present {
                info!(
                    encoder = accel.as_str(),
                    filter,
                    "ffmpeg build is missing the GPU tonemap filter; HDR sessions \
                     will use the CPU pipeline regardless of admin setting"
                );
            }
            present
        }
        Err(err) => {
            warn!(
                ?err,
                "couldn't list ffmpeg filters; assuming GPU tonemap is unavailable"
            );
            false
        }
    }
}

/// Parse `ffmpeg -filters` for the names it advertises. The third
/// whitespace-separated token on each filter line is the filter name
/// (`tonemap_cuda`, `scale_vaapi`, …).
async fn list_filters() -> Result<Vec<String>, DetectError> {
    let mut child = Command::new(ffmpeg_bin())
        .arg("-hide_banner")
        .arg("-filters")
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
        // Filter lines look like: " ..C tonemap_cuda     V->V    GPU …".
        // The header section has lines like "T.. = Timeline support";
        // those start in column 0 (no leading space) so we filter them
        // out by requiring a leading space and skipping the marker line
        // "  T.." marker entries (their second token is `=`).
        let trimmed = line.trim_start();
        if trimmed.len() < 5 || !line.starts_with(' ') {
            continue;
        }
        let mut parts = trimmed.split_whitespace();
        // First token: flags (e.g. "T.." / "..C" / "TS."). Skip it.
        let Some(_flags) = parts.next() else { continue };
        let Some(name) = parts.next() else { continue };
        // Skip the legend lines whose "second token" is `=`.
        if name == "=" {
            continue;
        }
        names.push(name.to_string());
    }
    Ok(names)
}

/// Parse `ffmpeg -encoders` for the H.264 encoders it advertises.
/// Returns just the encoder names (e.g. "libx264", "h264_qsv").
async fn list_encoders() -> Result<Vec<String>, DetectError> {
    let mut child = Command::new(ffmpeg_bin())
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
    let mut cmd = Command::new(ffmpeg_bin());
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
    // Production VAAPI uses scale_vaapi on GPU-resident surfaces, but
    // the smoke test feeds CPU frames from lavfi. Substitute the
    // upload-then-encode filter chain here; both still exercise the
    // h264_vaapi encoder + driver chain, which is what we're verifying.
    if accel == HwAccel::Vaapi {
        cmd.arg("-vf").arg("format=nv12,hwupload");
        cmd.arg("-c:v").arg("h264_vaapi").arg("-qp").arg("23");
    } else {
        cmd.args(accel.encode_args());
    }
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

    #[test]
    fn tonemap_algorithm_parses_known_slugs() {
        assert_eq!(
            TonemapAlgorithm::from_str_or_default("hable"),
            TonemapAlgorithm::Hable
        );
        assert_eq!(
            TonemapAlgorithm::from_str_or_default("MOBIUS"),
            TonemapAlgorithm::Mobius
        );
        assert_eq!(
            TonemapAlgorithm::from_str_or_default("  reinhard  "),
            TonemapAlgorithm::Reinhard
        );
        assert_eq!(
            TonemapAlgorithm::from_str_or_default("bt2390"),
            TonemapAlgorithm::Bt2390
        );
    }

    #[test]
    fn tonemap_algorithm_falls_back_to_default_on_unknown() {
        // Stale DB values must not break playback. Default is Hable
        // because it's the de facto industry choice; if the operator
        // disagrees they re-pick from the UI.
        assert_eq!(
            TonemapAlgorithm::from_str_or_default(""),
            TonemapAlgorithm::Hable
        );
        assert_eq!(
            TonemapAlgorithm::from_str_or_default("definitely-not-an-operator"),
            TonemapAlgorithm::Hable
        );
    }

    #[test]
    fn hw_tonemap_filter_name_only_set_for_nvenc_and_vaapi() {
        assert_eq!(
            HwAccel::Nvenc.hw_tonemap_filter_name(),
            Some("tonemap_cuda")
        );
        assert_eq!(
            HwAccel::Vaapi.hw_tonemap_filter_name(),
            Some("tonemap_vaapi")
        );
        for none_accel in [HwAccel::Cpu, HwAccel::Qsv, HwAccel::VideoToolbox] {
            assert_eq!(none_accel.hw_tonemap_filter_name(), None);
        }
    }

    #[test]
    fn tonemap_pipeline_parses_known_slugs() {
        assert_eq!(
            TonemapPipeline::from_str_or_default("software"),
            TonemapPipeline::Software
        );
        assert_eq!(
            TonemapPipeline::from_str_or_default("CPU"),
            TonemapPipeline::Software
        );
        assert_eq!(
            TonemapPipeline::from_str_or_default("hardware"),
            TonemapPipeline::Hardware
        );
    }

    #[test]
    fn tonemap_pipeline_falls_back_to_hardware_on_unknown() {
        // Default is Hardware — the whole point of this setting is
        // that CPU tonemap was too expensive. Stale or junk values
        // shouldn't silently flip an operator back onto the CPU.
        assert_eq!(
            TonemapPipeline::from_str_or_default(""),
            TonemapPipeline::Hardware
        );
        assert_eq!(
            TonemapPipeline::from_str_or_default("definitely-not-a-pipeline"),
            TonemapPipeline::Hardware
        );
    }

    #[test]
    fn tonemap_prefilter_is_empty_when_disabled() {
        let cfg = TonemapConfig::default();
        for accel in [
            HwAccel::Cpu,
            HwAccel::Nvenc,
            HwAccel::Vaapi,
            HwAccel::Qsv,
            HwAccel::VideoToolbox,
        ] {
            assert!(
                accel.tonemap_prefilter(cfg).is_empty(),
                "{accel:?} should emit nothing when apply=false"
            );
        }
    }

    #[test]
    fn tonemap_prefilter_cpu_encoder_is_inline_regardless_of_pipeline() {
        for pipeline in [TonemapPipeline::Hardware, TonemapPipeline::Software] {
            let chain = HwAccel::Cpu.tonemap_prefilter(TonemapConfig {
                apply: true,
                algorithm: TonemapAlgorithm::Hable,
                pipeline,
            });
            // libx264 has no GPU surface to download from. Both
            // pipeline choices collapse to the inline CPU chain.
            assert!(chain.contains("zscale=t=linear"));
            assert!(chain.contains("tonemap=tonemap=hable"));
            assert!(!chain.contains("hwdownload"));
            assert!(!chain.contains("hwupload"));
        }
    }

    #[test]
    fn tonemap_prefilter_nvenc_hardware_uses_tonemap_cuda() {
        let chain = HwAccel::Nvenc.tonemap_prefilter(TonemapConfig {
            apply: true,
            algorithm: TonemapAlgorithm::Bt2390,
            pipeline: TonemapPipeline::Hardware,
        });
        // GPU-side tonemap: filter runs on the CUDA surface, no
        // download to system memory.
        assert!(chain.starts_with("tonemap_cuda"));
        assert!(chain.contains("tonemap=bt2390"));
        assert!(!chain.contains("hwdownload"));
        assert!(!chain.contains("hwupload"));
        // BT.709 output so the encoder doesn't re-flag the stream
        // as HDR.
        assert!(chain.contains("transfer=bt709"));
        assert!(chain.contains("format=yuv420p"));
    }

    #[test]
    fn tonemap_prefilter_nvenc_software_wraps_in_hwdownload_and_hwupload_cuda() {
        let chain = HwAccel::Nvenc.tonemap_prefilter(TonemapConfig {
            apply: true,
            algorithm: TonemapAlgorithm::Bt2390,
            pipeline: TonemapPipeline::Software,
        });
        assert!(chain.starts_with("hwdownload"));
        assert!(chain.contains("tonemap=tonemap=bt2390"));
        assert!(chain.ends_with("hwupload_cuda"));
    }

    #[test]
    fn tonemap_prefilter_vaapi_hardware_uses_tonemap_vaapi() {
        let chain = HwAccel::Vaapi.tonemap_prefilter(TonemapConfig {
            apply: true,
            algorithm: TonemapAlgorithm::Mobius,
            pipeline: TonemapPipeline::Hardware,
        });
        assert!(chain.starts_with("tonemap_vaapi"));
        assert!(chain.contains("format=nv12"));
        // The driver picks the algorithm — our `Mobius` selection
        // is intentionally not threaded into the filter string here.
        assert!(!chain.contains("hwdownload"));
        assert!(!chain.contains("hwupload"));
    }

    #[test]
    fn tonemap_prefilter_vaapi_software_lands_in_nv12_before_hwupload() {
        let chain = HwAccel::Vaapi.tonemap_prefilter(TonemapConfig {
            apply: true,
            algorithm: TonemapAlgorithm::Mobius,
            pipeline: TonemapPipeline::Software,
        });
        assert!(chain.starts_with("hwdownload"));
        // scale_vaapi expects nv12 input — the CPU chain ends in
        // yuv420p, so we need an explicit format step before the
        // upload back to GPU surfaces.
        assert!(chain.contains("format=nv12,hwupload"));
    }

    #[test]
    fn abr_video_encoder_args_force_bt709_output_tags_when_tonemapping() {
        // Without explicit overrides, ffmpeg copies the source's HDR
        // color tags (smpte2084 / bt2020nc) into the output H.264 SPS
        // even though the pixels have been tonemapped to SDR — players
        // then re-tonemap an already-SDR signal and the result is
        // washed out / oversaturated. Most visible on VAAPI because
        // tonemap_vaapi only updates surface metadata that h264_vaapi
        // doesn't forward. Guard against the regression on every
        // encoder.
        let rendition = &crate::ABR_LADDER[0];
        let tonemap = TonemapConfig {
            apply: true,
            algorithm: TonemapAlgorithm::Hable,
            pipeline: TonemapPipeline::Hardware,
        };
        for accel in [
            HwAccel::Cpu,
            HwAccel::Nvenc,
            HwAccel::Qsv,
            HwAccel::Vaapi,
            HwAccel::VideoToolbox,
        ] {
            let args = accel.abr_video_encoder_args(0, rendition, tonemap);
            assert!(
                args.iter().any(|a| a == "-color_primaries:v:0"),
                "{accel:?} missing color_primaries override"
            );
            assert!(
                args.iter().any(|a| a == "-color_trc:v:0"),
                "{accel:?} missing color_trc override"
            );
            assert!(
                args.iter().any(|a| a == "-colorspace:v:0"),
                "{accel:?} missing colorspace override"
            );
            assert!(
                args.iter().any(|a| a == "-color_range:v:0"),
                "{accel:?} missing color_range override"
            );
            assert!(
                args.iter().filter(|a| *a == "bt709").count() >= 3,
                "{accel:?} should pin bt709 on three of the four tags"
            );
        }
    }

    #[test]
    fn abr_video_encoder_args_skip_color_overrides_without_tonemapping() {
        // When tonemap is off, the source's color tags should flow
        // through untouched — SDR sources stay SDR, and an HDR source
        // played without tonemap (operator override) stays tagged HDR
        // so downstream HDR-capable clients render it correctly.
        let rendition = &crate::ABR_LADDER[0];
        let tonemap = TonemapConfig::default();
        for accel in [
            HwAccel::Cpu,
            HwAccel::Nvenc,
            HwAccel::Qsv,
            HwAccel::Vaapi,
            HwAccel::VideoToolbox,
        ] {
            let args = accel.abr_video_encoder_args(0, rendition, tonemap);
            assert!(
                !args.iter().any(|a| a.starts_with("-color_primaries")),
                "{accel:?} must not override color tags when tonemap is off"
            );
            assert!(
                !args.iter().any(|a| a.starts_with("-color_trc")),
                "{accel:?} must not override color tags when tonemap is off"
            );
            assert!(
                !args.iter().any(|a| a.starts_with("-colorspace")),
                "{accel:?} must not override color tags when tonemap is off"
            );
            assert!(
                !args.iter().any(|a| a.starts_with("-color_range")),
                "{accel:?} must not override color tags when tonemap is off"
            );
        }
    }
}
