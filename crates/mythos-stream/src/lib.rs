//! Streaming pipeline.
//!
//! Phase 2 served direct-play byte ranges out of `mythos-api`. This
//! crate owns the Phase 4 piece: spawning `ffmpeg` to transcode files
//! the browser can't handle natively (HEVC, AC-3/DTS audio, MKV
//! container, etc.) into HLS segments served back through the API.

pub mod abr;
pub mod hwaccel;
pub mod transcode;

pub use abr::{
    ABR_LADDER, Rendition, SOURCE_VARIANT, default_variant, is_known_variant, rendition_by_name,
    source_rendition,
};
pub use hwaccel::{HwAccel, resolve as resolve_hwaccel};
pub use transcode::{
    SEGMENT_DURATION_SECS, SEGMENT_WAIT_TIMEOUT, SessionKey, TranscodeError, TranscodeManager,
    TranscodeSession, build_master_playlist, build_variant_playlist, parse_segment_filename,
    wait_for_file,
};
