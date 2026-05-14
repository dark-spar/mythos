//! Filesystem scanner: walks library roots, identifies video files,
//! probes them with ffprobe, and upserts into the database.
//!
//! Per-kind scanners live alongside the dispatcher: `movies::` handles
//! `LibraryKind::Movies`, `tv::` handles `LibraryKind::Shows`. Music /
//! photos / books are still no-ops; they get their own modules when
//! their phases land.

pub mod identify;
pub mod identify_tv;
pub mod movies;
pub mod probe;
pub mod scan;
pub mod tv;
pub mod walk;

pub use identify::{Identity, identify_movie};
pub use identify_tv::{TvIdentity, identify_tv};
pub use movies::scan_movies_library;
pub use probe::{ProbeError, probe};
pub use scan::{ScanReport, scan_library};
pub use tv::scan_tv_library;
pub use walk::{VIDEO_EXTENSIONS, video_files};
