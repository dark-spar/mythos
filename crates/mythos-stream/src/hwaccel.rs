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
/// the active encoder is VAAPI on the [`TonemapPipeline::Vaapi`]
/// pipeline.
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

/// Which filter performs the HDR→SDR conversion.
///
/// Each variant is a specific filter, not an abstract "GPU vs CPU"
/// toggle. The earlier two-state `Hardware`/`Software` design hid
/// real differences from operators:
///
/// - Output quality is filter-specific: Intel iHD's `tonemap_vaapi`
///   crushes blacks on real HDR sources, while `tonemap_opencl`
///   produces a properly-exposed image on the same hardware. Pinning
///   them under one "Hardware" choice misled operators about what
///   they were getting.
/// - Algorithm support differs: [`TonemapAlgorithm`] is honoured by
///   `tonemap_opencl`, `tonemap_cuda`, and the CPU chain, but ignored
///   by `tonemap_vaapi` (the driver picks).
/// - Build / driver requirements differ: `tonemap_opencl` needs an
///   OpenCL runtime (e.g. `intel-compute-runtime` on Intel);
///   `tonemap_cuda` needs the CUDA build of ffmpeg; `tonemap_vaapi`
///   needs only libva. They fail in different ways and want
///   different diagnostics.
///
/// Not every variant is valid for every encoder. The HLS handler
/// coerces invalid combinations down to [`Software`] before
/// scheduling the session; see `tonemap_prefilter`.
///
/// [`Software`]: TonemapPipeline::Software
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TonemapPipeline {
    /// CPU `zscale`+`tonemap` chain. Always available and
    /// algorithm-respecting; high quality at the cost of CPU load
    /// (saturates several cores on a 4K HDR source). On hardware
    /// encoders the chain is wrapped in `hwdownload`/`hwupload` so
    /// the encoder still runs on the GPU. The safe default.
    #[default]
    Software,
    /// `tonemap_vaapi` running on VAAPI surfaces. Zero CPU cost and
    /// no round-trip, but the driver picks the curve — the operator's
    /// [`TonemapAlgorithm`] is ignored. Known to look bad on Intel iHD
    /// (crushes midtones to black). Only valid when the active
    /// encoder is [`HwAccel::Vaapi`].
    Vaapi,
    /// `tonemap_opencl` reached from VAAPI via `hwmap` (zero-copy via
    /// `cl_intel_va_api_media_sharing` on Intel, similar AMD extension
    /// elsewhere). High-quality, algorithm-respecting, GPU-resident.
    /// Requires `intel-compute-runtime` (or the AMD equivalent) plus
    /// `tonemap_opencl` compiled into ffmpeg. Only valid when the
    /// active encoder is [`HwAccel::Vaapi`].
    Opencl,
    /// `tonemap_cuda` on CUDA surfaces for the NVENC pipeline.
    /// Zero-copy, algorithm-respecting, GPU-resident. Requires the
    /// CUDA build of ffmpeg (many distro packages omit it). Only
    /// valid when the active encoder is [`HwAccel::Nvenc`].
    Cuda,
}

impl TonemapPipeline {
    pub fn as_str(self) -> &'static str {
        match self {
            TonemapPipeline::Software => "software",
            TonemapPipeline::Vaapi => "vaapi",
            TonemapPipeline::Opencl => "opencl",
            TonemapPipeline::Cuda => "cuda",
        }
    }

    /// Parse an admin-supplied pipeline name. Unknown values fall
    /// back to [`TonemapPipeline::Software`] — that's the only
    /// variant guaranteed to work on every encoder, so it's the
    /// safe landing zone for a stale DB row or a typo.
    ///
    /// The legacy string `"hardware"` (from the previous two-state
    /// model, where it secretly meant `tonemap_vaapi` or
    /// `tonemap_cuda` depending on encoder) also maps to Software so
    /// operators upgrading across this change get the correct,
    /// portable behaviour and have to re-pick a HW filter
    /// explicitly. Quietly remapping `"hardware"` to `Vaapi` would
    /// reintroduce the crushed-blacks symptom on Intel.
    pub fn from_str_or_default(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "vaapi" | "tonemap_vaapi" => TonemapPipeline::Vaapi,
            "opencl" | "ocl" | "tonemap_opencl" => TonemapPipeline::Opencl,
            "cuda" | "nvenc" | "tonemap_cuda" => TonemapPipeline::Cuda,
            _ => TonemapPipeline::Software,
        }
    }
}

/// Which HDR→SDR filters this ffmpeg build is capable of running.
///
/// Probed once at server startup (see [`probe_tonemap_support`]) by
/// looking at `ffmpeg -filters`. Stored on [`crate::TranscodeManager`]
/// so the HLS handler can coerce an operator's pick down to
/// [`TonemapPipeline::Software`] when the named filter isn't compiled
/// in — without rewriting the stored row, so swapping to a fuller
/// ffmpeg build later restores the operator's original intent.
///
/// This is a build-presence check, not a runtime-works check. A
/// filter compiled in but missing its driver (e.g. `tonemap_opencl`
/// without `intel-compute-runtime`) will still pass the probe and
/// fail at session start — same trade-off the smoke test makes for
/// encoders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TonemapSupport {
    pub vaapi: bool,
    pub opencl: bool,
    pub cuda: bool,
}

impl TonemapSupport {
    /// Whether this build supports the operator's chosen pipeline.
    /// `Software` is always considered supported; the rest gate on
    /// the corresponding boolean.
    pub fn supports(&self, pipeline: TonemapPipeline) -> bool {
        match pipeline {
            TonemapPipeline::Software => true,
            TonemapPipeline::Vaapi => self.vaapi,
            TonemapPipeline::Opencl => self.opencl,
            TonemapPipeline::Cuda => self.cuda,
        }
    }

    /// Pipelines valid for an active encoder, regardless of
    /// build support. `Software` is in every list because it works
    /// on every encoder. The UI uses this to decide which radio
    /// options to render; build availability is a secondary
    /// dimension shown as an "(unavailable)" hint per option.
    pub fn valid_for(accel: HwAccel) -> &'static [TonemapPipeline] {
        match accel {
            HwAccel::Vaapi => &[
                TonemapPipeline::Software,
                TonemapPipeline::Vaapi,
                TonemapPipeline::Opencl,
            ],
            HwAccel::Nvenc => &[TonemapPipeline::Software, TonemapPipeline::Cuda],
            HwAccel::Cpu | HwAccel::Qsv | HwAccel::VideoToolbox => &[TonemapPipeline::Software],
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
    /// `pipeline` matters for VAAPI: the [`TonemapPipeline::Opencl`]
    /// chain needs a *named* VAAPI device plus a derived OpenCL
    /// device so its `hwmap=derive_device=opencl` can find a target
    /// to land on. The other pipelines use the simpler
    /// `-vaapi_device` form. Switching to the named-device form
    /// unconditionally on VAAPI would also work but is gratuitously
    /// more verbose for the common case.
    ///
    /// If your library has a codec the GPU can't HW-decode, ffmpeg
    /// will error on the input; falling back to `MYTHOS_HW_ENCODER=cpu`
    /// recovers cleanly.
    pub fn decode_args(self, pipeline: TonemapPipeline) -> Vec<String> {
        match (self, pipeline) {
            (HwAccel::Vaapi, TonemapPipeline::Opencl) => vec![
                "-init_hw_device".into(),
                "vaapi=va:/dev/dri/renderD128".into(),
                "-init_hw_device".into(),
                "opencl=ocl@va".into(),
                "-filter_hw_device".into(),
                "ocl".into(),
                "-hwaccel".into(),
                "vaapi".into(),
                "-hwaccel_device".into(),
                "va".into(),
                "-hwaccel_output_format".into(),
                "vaapi".into(),
            ],
            (HwAccel::Vaapi, _) => vec![
                "-hwaccel".into(),
                "vaapi".into(),
                "-vaapi_device".into(),
                "/dev/dri/renderD128".into(),
                "-hwaccel_output_format".into(),
                "vaapi".into(),
            ],
            (HwAccel::Nvenc, _) => vec![
                "-hwaccel".into(),
                "cuda".into(),
                "-hwaccel_output_format".into(),
                "cuda".into(),
            ],
            (HwAccel::Qsv | HwAccel::VideoToolbox | HwAccel::Cpu, _) => vec![],
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
    /// - [`TonemapPipeline::Software`] — CPU `zscale`+`tonemap`
    ///   chain. Wrapped in `hwdownload`/`hwupload` on hardware
    ///   encoders so the encoder still runs on the GPU; pays the
    ///   round-trip on HDR content but is portable across ffmpeg
    ///   builds.
    /// - [`TonemapPipeline::Vaapi`] (VAAPI only) — `tonemap_vaapi`
    ///   on VAAPI surfaces. Driver picks the curve so the algorithm
    ///   setting is ignored.
    /// - [`TonemapPipeline::Opencl`] (VAAPI only) — `tonemap_opencl`
    ///   reached via `hwmap` to an OpenCL device derived from the
    ///   VAAPI one (zero-copy via Intel `cl_intel_va_api_media_sharing`
    ///   or the AMD equivalent). Algorithm-respecting.
    /// - [`TonemapPipeline::Cuda`] (NVENC only) — `tonemap_cuda` on
    ///   CUDA surfaces. Algorithm-respecting.
    ///
    /// Invalid combinations (e.g. asking for `Cuda` on a VAAPI
    /// encoder) silently fall through to the Software chain rather
    /// than emitting an unrunnable filter graph. The HLS handler
    /// rejects invalid picks earlier than this; the fall-through is
    /// belt-and-braces.
    ///
    /// Encoders without a HW tonemap filter (libx264, QSV,
    /// VideoToolbox) always emit the CPU chain inline regardless of
    /// pipeline — there's no GPU surface to download from.
    pub fn tonemap_prefilter(self, cfg: TonemapConfig) -> String {
        if !cfg.apply {
            return String::new();
        }
        let algo = cfg.algorithm.as_str();
        // CPU chain — the inline filter for non-HW encoders, the
        // meat of the hwdownload→...→hwupload wrap on HW encoders,
        // and the fall-through for invalid (encoder, pipeline) combos.
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
            // CPU-only encoders — no GPU surface to download from.
            // Pipeline choice is moot, always inline.
            (HwAccel::Cpu | HwAccel::Qsv | HwAccel::VideoToolbox, _) => cpu,

            // NVENC + Cuda: `tonemap_cuda` keeps frames on the GPU.
            // Matrix/primaries/transfer pinned to BT.709 so the
            // output is correct SDR; `format=yuv420p` matches what
            // h264_nvenc accepts (no 10-bit on H.264).
            (HwAccel::Nvenc, TonemapPipeline::Cuda) => format!(
                "tonemap_cuda=tonemap={algo}:desat=0:\
                 transfer=bt709:matrix=bt709:primaries=bt709:\
                 format=yuv420p"
            ),
            // NVENC + Software: round-trip through system memory so
            // the encoder still runs on the GPU.
            (HwAccel::Nvenc, TonemapPipeline::Software) => {
                format!("hwdownload,format=p010le,{cpu},hwupload_cuda")
            }
            // NVENC + Vaapi/Opencl: invalid combo. Fall through to
            // the Software chain rather than emit a broken graph.
            (HwAccel::Nvenc, _) => format!("hwdownload,format=p010le,{cpu},hwupload_cuda"),

            // VAAPI + Vaapi: `tonemap_vaapi` doesn't take an algorithm
            // option — the Intel/AMD driver picks one. The operator's
            // algorithm setting is ignored on this branch; documented
            // in the UI. Known to crush blacks on Intel iHD.
            (HwAccel::Vaapi, TonemapPipeline::Vaapi) => {
                "tonemap_vaapi=format=nv12:matrix=bt709:primaries=bt709:transfer=bt709".to_string()
            }
            // VAAPI + Opencl: zero-copy hwmap to OpenCL surfaces,
            // tonemap_opencl honours the algorithm, then hwmap back
            // to VAAPI for scale_vaapi + h264_vaapi. Requires the
            // OpenCL device init args from `decode_args` — kept in
            // sync so the filter has a device to land on.
            //
            // `desat=0` matches the rest of the codebase; the curve
            // does the work. `r=tv` flags TV-range output so the
            // VAAPI scaler and encoder don't reinterpret as full range.
            (HwAccel::Vaapi, TonemapPipeline::Opencl) => format!(
                "hwmap=derive_device=opencl:mode=read,\
                 tonemap_opencl=tonemap={algo}:format=nv12:\
                 p=bt709:t=bt709:m=bt709:r=tv:desat=0,\
                 hwmap=derive_device=vaapi:reverse=1"
            ),
            // VAAPI + Software: round-trip + nv12 before reupload
            // because `scale_vaapi` expects nv12 surfaces downstream.
            (HwAccel::Vaapi, TonemapPipeline::Software) => {
                format!("hwdownload,format=p010le,{cpu},format=nv12,hwupload")
            }
            // VAAPI + Cuda: invalid combo. Software fall-through.
            (HwAccel::Vaapi, TonemapPipeline::Cuda) => {
                format!("hwdownload,format=p010le,{cpu},format=nv12,hwupload")
            }
        }
    }

    /// Name of the GPU-side HDR→SDR filter advertised by a specific
    /// pipeline choice, or `None` if the choice is the CPU chain
    /// (Software) or invalid for this encoder. The startup probe
    /// uses this to know which filter name to look for in
    /// `ffmpeg -filters` per (encoder, pipeline).
    pub fn hw_tonemap_filter_name_for(self, pipeline: TonemapPipeline) -> Option<&'static str> {
        match (self, pipeline) {
            (HwAccel::Vaapi, TonemapPipeline::Vaapi) => Some("tonemap_vaapi"),
            (HwAccel::Vaapi, TonemapPipeline::Opencl) => Some("tonemap_opencl"),
            (HwAccel::Nvenc, TonemapPipeline::Cuda) => Some("tonemap_cuda"),
            _ => None,
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
    pub fn abr_video_encoder_args(self, output_index: usize, rendition: &Rendition) -> Vec<String> {
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

/// Which HDR→SDR filters this ffmpeg build advertises.
///
/// `tonemap_vaapi` / `tonemap_opencl` / `tonemap_cuda` are all
/// optional in the ffmpeg baseline — distro packages vary. Probed
/// once at startup by reading `ffmpeg -filters`; the result is
/// stored on the [`crate::TranscodeManager`] so the HLS handler can
/// downgrade an unavailable [`TonemapPipeline`] to
/// [`TonemapPipeline::Software`] without rewriting the operator's
/// stored choice (so an ffmpeg upgrade later restores their intent).
///
/// Probe failure is treated as "no HW tonemap available" — operators
/// in that state still get correct output via the Software chain.
pub async fn probe_tonemap_support() -> TonemapSupport {
    let names = match list_filters().await {
        Ok(n) => n,
        Err(err) => {
            warn!(
                ?err,
                "couldn't list ffmpeg filters; assuming no HW tonemap is available"
            );
            return TonemapSupport::default();
        }
    };
    let has = |target: &str| names.iter().any(|n| n == target);
    let support = TonemapSupport {
        vaapi: has("tonemap_vaapi"),
        opencl: has("tonemap_opencl"),
        cuda: has("tonemap_cuda"),
    };
    info!(
        vaapi = support.vaapi,
        opencl = support.opencl,
        cuda = support.cuda,
        "HW tonemap filters probed"
    );
    support
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
    fn hw_tonemap_filter_name_for_maps_each_pipeline_to_a_filter() {
        // The startup probe looks up which named filter to check for
        // by encoder+pipeline. Cover the three valid HW combos plus a
        // couple of invalid ones (None) so we don't accidentally probe
        // for a filter on an encoder that can't host it.
        assert_eq!(
            HwAccel::Vaapi.hw_tonemap_filter_name_for(TonemapPipeline::Vaapi),
            Some("tonemap_vaapi")
        );
        assert_eq!(
            HwAccel::Vaapi.hw_tonemap_filter_name_for(TonemapPipeline::Opencl),
            Some("tonemap_opencl")
        );
        assert_eq!(
            HwAccel::Nvenc.hw_tonemap_filter_name_for(TonemapPipeline::Cuda),
            Some("tonemap_cuda")
        );
        // Software is the CPU chain — no probe needed.
        assert_eq!(
            HwAccel::Vaapi.hw_tonemap_filter_name_for(TonemapPipeline::Software),
            None
        );
        // Invalid combos (Cuda on VAAPI, Vaapi on NVENC, anything on
        // CPU/QSV/VideoToolbox) return None — these get coerced to
        // Software upstream anyway.
        assert_eq!(
            HwAccel::Vaapi.hw_tonemap_filter_name_for(TonemapPipeline::Cuda),
            None
        );
        assert_eq!(
            HwAccel::Nvenc.hw_tonemap_filter_name_for(TonemapPipeline::Vaapi),
            None
        );
        for accel in [HwAccel::Cpu, HwAccel::Qsv, HwAccel::VideoToolbox] {
            for pipeline in [
                TonemapPipeline::Software,
                TonemapPipeline::Vaapi,
                TonemapPipeline::Opencl,
                TonemapPipeline::Cuda,
            ] {
                assert_eq!(accel.hw_tonemap_filter_name_for(pipeline), None);
            }
        }
    }

    #[test]
    fn tonemap_pipeline_parses_known_slugs() {
        assert_eq!(
            TonemapPipeline::from_str_or_default("software"),
            TonemapPipeline::Software
        );
        assert_eq!(
            TonemapPipeline::from_str_or_default("vaapi"),
            TonemapPipeline::Vaapi
        );
        assert_eq!(
            TonemapPipeline::from_str_or_default("OPENCL"),
            TonemapPipeline::Opencl
        );
        assert_eq!(
            TonemapPipeline::from_str_or_default("cuda"),
            TonemapPipeline::Cuda
        );
        assert_eq!(
            TonemapPipeline::from_str_or_default("  tonemap_opencl  "),
            TonemapPipeline::Opencl
        );
    }

    #[test]
    fn tonemap_pipeline_unknown_and_legacy_hardware_fall_back_to_software() {
        // Software is the only variant guaranteed to work on every
        // encoder, so it's the safe landing zone for junk values.
        assert_eq!(
            TonemapPipeline::from_str_or_default(""),
            TonemapPipeline::Software
        );
        assert_eq!(
            TonemapPipeline::from_str_or_default("definitely-not-a-pipeline"),
            TonemapPipeline::Software
        );
        // Legacy "hardware" — used to mean tonemap_vaapi / tonemap_cuda
        // depending on encoder, including the broken Intel iHD case.
        // Remap to Software so operators upgrading across this change
        // re-pick explicitly rather than silently re-acquiring the
        // crushed-blacks symptom.
        assert_eq!(
            TonemapPipeline::from_str_or_default("hardware"),
            TonemapPipeline::Software
        );
    }

    #[test]
    fn tonemap_support_validates_per_encoder() {
        // VAAPI: Software + Vaapi + Opencl (no Cuda).
        let vaapi_valid = TonemapSupport::valid_for(HwAccel::Vaapi);
        assert!(vaapi_valid.contains(&TonemapPipeline::Software));
        assert!(vaapi_valid.contains(&TonemapPipeline::Vaapi));
        assert!(vaapi_valid.contains(&TonemapPipeline::Opencl));
        assert!(!vaapi_valid.contains(&TonemapPipeline::Cuda));
        // NVENC: Software + Cuda only.
        let nvenc_valid = TonemapSupport::valid_for(HwAccel::Nvenc);
        assert!(nvenc_valid.contains(&TonemapPipeline::Software));
        assert!(nvenc_valid.contains(&TonemapPipeline::Cuda));
        assert!(!nvenc_valid.contains(&TonemapPipeline::Vaapi));
        // Non-HW encoders: Software only.
        for accel in [HwAccel::Cpu, HwAccel::Qsv, HwAccel::VideoToolbox] {
            assert_eq!(
                TonemapSupport::valid_for(accel),
                &[TonemapPipeline::Software]
            );
        }
    }

    #[test]
    fn tonemap_support_supports_is_software_always_plus_gated_hw() {
        let support = TonemapSupport {
            vaapi: true,
            opencl: false,
            cuda: true,
        };
        // Software is unconditionally supported — it's the CPU chain.
        assert!(support.supports(TonemapPipeline::Software));
        assert!(support.supports(TonemapPipeline::Vaapi));
        assert!(!support.supports(TonemapPipeline::Opencl));
        assert!(support.supports(TonemapPipeline::Cuda));
        // Empty support still allows Software.
        let empty = TonemapSupport::default();
        assert!(empty.supports(TonemapPipeline::Software));
        assert!(!empty.supports(TonemapPipeline::Vaapi));
        assert!(!empty.supports(TonemapPipeline::Opencl));
        assert!(!empty.supports(TonemapPipeline::Cuda));
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
        for pipeline in [
            TonemapPipeline::Software,
            TonemapPipeline::Vaapi,
            TonemapPipeline::Opencl,
            TonemapPipeline::Cuda,
        ] {
            let chain = HwAccel::Cpu.tonemap_prefilter(TonemapConfig {
                apply: true,
                algorithm: TonemapAlgorithm::Hable,
                pipeline,
            });
            // libx264 has no GPU surface to download from. Every
            // pipeline choice collapses to the inline CPU chain.
            assert!(chain.contains("zscale=t=linear"));
            assert!(chain.contains("tonemap=tonemap=hable"));
            assert!(!chain.contains("hwdownload"));
            assert!(!chain.contains("hwupload"));
        }
    }

    #[test]
    fn tonemap_prefilter_nvenc_cuda_uses_tonemap_cuda() {
        let chain = HwAccel::Nvenc.tonemap_prefilter(TonemapConfig {
            apply: true,
            algorithm: TonemapAlgorithm::Bt2390,
            pipeline: TonemapPipeline::Cuda,
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
    fn tonemap_prefilter_nvenc_invalid_pipelines_fall_through_to_software() {
        // Vaapi / Opencl on NVENC are nonsense combinations. They
        // should land on the Software fall-through rather than
        // emitting a graph that ffmpeg can't run. HLS handler
        // coerces these earlier; this is belt-and-braces.
        for pipeline in [TonemapPipeline::Vaapi, TonemapPipeline::Opencl] {
            let chain = HwAccel::Nvenc.tonemap_prefilter(TonemapConfig {
                apply: true,
                algorithm: TonemapAlgorithm::Hable,
                pipeline,
            });
            assert!(chain.starts_with("hwdownload"), "{pipeline:?}: {chain}");
            assert!(chain.ends_with("hwupload_cuda"), "{pipeline:?}: {chain}");
        }
    }

    #[test]
    fn tonemap_prefilter_vaapi_vaapi_uses_tonemap_vaapi() {
        let chain = HwAccel::Vaapi.tonemap_prefilter(TonemapConfig {
            apply: true,
            algorithm: TonemapAlgorithm::Mobius,
            pipeline: TonemapPipeline::Vaapi,
        });
        assert!(chain.starts_with("tonemap_vaapi"));
        assert!(chain.contains("format=nv12"));
        // The driver picks the algorithm — our `Mobius` selection
        // is intentionally not threaded into the filter string here.
        assert!(!chain.contains("hwdownload"));
        assert!(!chain.contains("hwupload"));
    }

    #[test]
    fn tonemap_prefilter_vaapi_opencl_uses_hwmap_to_opencl_and_back() {
        let chain = HwAccel::Vaapi.tonemap_prefilter(TonemapConfig {
            apply: true,
            algorithm: TonemapAlgorithm::Hable,
            pipeline: TonemapPipeline::Opencl,
        });
        // Zero-copy round-trip: hwmap to OpenCL, run tonemap_opencl
        // (algorithm-respecting unlike tonemap_vaapi), hwmap back to
        // VAAPI for the downstream scale_vaapi / h264_vaapi.
        assert!(chain.contains("hwmap=derive_device=opencl"));
        assert!(chain.contains("tonemap_opencl"));
        assert!(chain.contains("tonemap=hable"));
        assert!(chain.contains("hwmap=derive_device=vaapi:reverse=1"));
        assert!(chain.contains("format=nv12"));
        assert!(chain.contains("p=bt709"));
        assert!(chain.contains("t=bt709"));
        // No system-memory round-trip — that's the whole point.
        assert!(!chain.contains("hwdownload"));
        assert!(!chain.contains("hwupload"));
    }

    #[test]
    fn tonemap_prefilter_vaapi_cuda_falls_through_to_software() {
        // Cuda on VAAPI: invalid. Software fall-through, same as the
        // NVENC mirror case.
        let chain = HwAccel::Vaapi.tonemap_prefilter(TonemapConfig {
            apply: true,
            algorithm: TonemapAlgorithm::Hable,
            pipeline: TonemapPipeline::Cuda,
        });
        assert!(chain.starts_with("hwdownload"));
        assert!(chain.contains("format=nv12,hwupload"));
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
}
