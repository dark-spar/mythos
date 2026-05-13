//! Library scan orchestrator: walk → identify → probe → upsert, then
//! prune anything not touched.
//!
//! For movies-kind libraries we identify and upsert a movie row per
//! file. Non-movies libraries are no-ops in Phase 1c (file rows are not
//! created either, so a future scanner for that kind can pick up
//! whatever schema it needs without colliding with stale movie rows).
//!
//! Pruning collects every relative path the walker yielded and asks the
//! repo to delete media_files outside that set. Cascade FKs remove the
//! associated movies. Path-based pruning (rather than timestamp-based)
//! is robust to wall-clock precision and removes a subtle race in
//! back-to-back scans.

use std::time::Instant;

use chrono::{DateTime, Utc};
use mythos_core::{Library, LibraryKind, NewMediaFile, NewMovie, Probe};
use mythos_db::{MediaFileRepo, MovieRepo};
use serde::Serialize;
use sqlx::SqlitePool;
use tracing::{info, warn};

use crate::identify::identify;
use crate::probe::probe;
use crate::walk::video_files;

#[derive(Debug, Clone, Default, Serialize)]
pub struct ScanReport {
    pub added: u32,
    pub updated: u32,
    pub removed: u64,
    pub errors: Vec<String>,
    pub duration_ms: u64,
}

pub async fn scan_library(pool: &SqlitePool, library: &Library) -> ScanReport {
    let started = Instant::now();
    let mut report = ScanReport::default();
    let mut seen_paths: Vec<String> = Vec::new();

    if library.kind != LibraryKind::Movies {
        info!(
            library = %library.name,
            kind = library.kind.as_str(),
            "scan skipped: only movies are implemented in Phase 1c"
        );
        report.duration_ms = started.elapsed().as_millis() as u64;
        return report;
    }

    let files = video_files(&library.root_path);
    info!(
        library = %library.name,
        files = files.len(),
        "starting scan"
    );

    let files_repo = MediaFileRepo::new(pool.clone());
    let movies_repo = MovieRepo::new(pool.clone());

    for absolute in files {
        let relative = match absolute.strip_prefix(&library.root_path) {
            Ok(p) => p.to_path_buf(),
            Err(_) => {
                report
                    .errors
                    .push(format!("path outside library root: {}", absolute.display()));
                continue;
            }
        };

        let (size_bytes, mtime) = match file_stats(&absolute).await {
            Ok(s) => s,
            Err(err) => {
                report
                    .errors
                    .push(format!("stat {}: {err}", absolute.display()));
                continue;
            }
        };

        let probe_data = match probe(&absolute).await {
            Ok(p) => p,
            Err(err) => {
                warn!(
                    error = %err,
                    file = %absolute.display(),
                    "ffprobe failed; indexing with empty technical metadata"
                );
                Probe::default()
            }
        };

        let upsert = files_repo
            .upsert(NewMediaFile {
                library_id: library.id,
                path: relative.clone(),
                size_bytes,
                mtime,
                probe: probe_data,
            })
            .await;
        let (file, inserted) = match upsert {
            Ok(v) => v,
            Err(err) => {
                report
                    .errors
                    .push(format!("upsert file {}: {err}", relative.display()));
                continue;
            }
        };

        // Record the relative path for pruning. Repo already validated
        // UTF-8 at upsert time, so the path_buf round-trip is safe.
        if let Some(p) = relative.to_str() {
            seen_paths.push(p.to_string());
        }

        let identity = identify(&relative);
        if let Err(err) = movies_repo
            .upsert(NewMovie {
                library_id: library.id,
                file_id: file.id,
                title: identity.title,
                year: identity.year,
            })
            .await
        {
            report
                .errors
                .push(format!("upsert movie {}: {err}", relative.display()));
            continue;
        }

        if inserted {
            report.added += 1;
        } else {
            report.updated += 1;
        }
    }

    report.removed = match files_repo.prune_unseen(library.id, &seen_paths).await {
        Ok(n) => n,
        Err(err) => {
            report.errors.push(format!("prune unseen: {err}"));
            0
        }
    };

    report.duration_ms = started.elapsed().as_millis() as u64;
    info!(
        library = %library.name,
        added = report.added,
        updated = report.updated,
        removed = report.removed,
        errors = report.errors.len(),
        duration_ms = report.duration_ms,
        "scan complete"
    );

    report
}

async fn file_stats(path: &std::path::Path) -> std::io::Result<(i64, DateTime<Utc>)> {
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
