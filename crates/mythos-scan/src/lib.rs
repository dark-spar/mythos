//! Filesystem scanner: walks library roots, identifies video files,
//! probes them with ffprobe, and upserts into the database.
//!
//! Phase 1c only scans movies — other library kinds are no-ops until
//! their dedicated identifiers / metadata providers land in later
//! phases.

pub mod identify;
pub mod probe;
pub mod scan;
pub mod walk;

pub use identify::{Identity, identify};
pub use probe::{ProbeError, probe};
pub use scan::{ScanReport, scan_library};
pub use walk::{VIDEO_EXTENSIONS, video_files};
