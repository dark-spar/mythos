//! Top-level scan orchestrator.
//!
//! Per-kind scanners live alongside this module ([`crate::movies`],
//! [`crate::tv`]); this file just dispatches on `library.kind` and
//! owns the shared [`ScanReport`] shape so per-kind reports stay
//! comparable.

use std::time::Instant;

use chrono::{DateTime, Utc};
use mythos_core::{Library, LibraryKind};
use mythos_meta::TmdbClient;
use serde::Serialize;
use sqlx::SqlitePool;
use tracing::info;

use crate::movies::scan_movies_library;
use crate::tv::scan_tv_library;

#[derive(Debug, Clone, Default, Serialize)]
pub struct ScanReport {
    pub added: u32,
    pub updated: u32,
    pub removed: u64,
    pub enriched: u32,
    pub errors: Vec<String>,
    pub duration_ms: u64,
}

pub async fn scan_library(
    pool: &SqlitePool,
    library: &Library,
    tmdb: Option<&TmdbClient>,
) -> ScanReport {
    match library.kind {
        LibraryKind::Movies => scan_movies_library(pool, library, tmdb).await,
        LibraryKind::Shows => scan_tv_library(pool, library, tmdb).await,
        other => {
            let started = Instant::now();
            info!(
                library = %library.name,
                kind = other.as_str(),
                "scan skipped: library kind not yet implemented"
            );
            ScanReport {
                duration_ms: started.elapsed().as_millis() as u64,
                ..Default::default()
            }
        }
    }
}

/// stat() a path into the `(size_bytes, mtime)` pair the scanners need
/// to upsert a `media_files` row. Shared by movies and tv scanners.
pub(crate) async fn file_stats(path: &std::path::Path) -> std::io::Result<(i64, DateTime<Utc>)> {
    let metadata = tokio::fs::metadata(path).await?;
    let size_bytes = metadata.len() as i64;
    let mtime = metadata
        .modified()
        .ok()
        .and_then(|t| {
            let dur = t.duration_since(std::time::UNIX_EPOCH).ok()?;
            DateTime::<Utc>::from_timestamp(dur.as_secs() as i64, dur.subsec_nanos())
        })
        .unwrap_or_else(Utc::now);
    Ok((size_bytes, mtime))
}
