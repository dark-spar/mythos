//! Movies-kind library scanner.
//!
//! Walk → identify → probe → upsert media_file + movie → prune. After
//! the structural pass, TMDb enrichment fills in titles/posters for
//! any movies still missing a `tmdb_id`. Search and poster-download
//! failures are accumulated into `ScanReport::errors` but do not
//! abort the scan; the file rows themselves are always written so a
//! later run can fill the gaps.

use std::time::Instant;

use mythos_core::{Library, NewMediaFile, NewMovie, Probe};
use mythos_db::{MediaFileRepo, MovieRepo};
use mythos_meta::TmdbClient;
use sqlx::SqlitePool;
use tracing::{info, warn};

use crate::identify::identify_movie;
use crate::probe::probe;
use crate::scan::{ScanReport, file_stats};
use crate::walk::video_files;

pub async fn scan_movies_library(
    pool: &SqlitePool,
    library: &Library,
    tmdb: Option<&TmdbClient>,
) -> ScanReport {
    let started = Instant::now();
    let mut report = ScanReport::default();
    let mut seen_paths: Vec<String> = Vec::new();

    let files = video_files(&library.root_path);
    info!(
        library = %library.name,
        files = files.len(),
        "starting movies scan"
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

        let identity = identify_movie(&relative);
        info!(
            file = %relative.display(),
            title = %identity.title,
            year = ?identity.year,
            "identified"
        );

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
            Ok(p) => {
                info!(
                    title = %identity.title,
                    container = p.container.as_deref().unwrap_or("?"),
                    video = p.video_codec.as_deref().unwrap_or("?"),
                    audio = p.audio_codec.as_deref().unwrap_or("?"),
                    duration_seconds = p.duration_seconds.unwrap_or(0.0),
                    "probed"
                );
                p
            }
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

        if let Some(p) = relative.to_str() {
            seen_paths.push(p.to_string());
        }

        let title_for_log = identity.title.clone();
        let year_for_log = identity.year;

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

        let action = if inserted { "added" } else { "updated" };
        info!(
            title = %title_for_log,
            year = ?year_for_log,
            action,
            "indexed"
        );

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

    if let Some(client) = tmdb {
        enrich_pass(&movies_repo, library.id, client, &mut report).await;
    }

    report.duration_ms = started.elapsed().as_millis() as u64;
    info!(
        library = %library.name,
        added = report.added,
        updated = report.updated,
        removed = report.removed,
        enriched = report.enriched,
        errors = report.errors.len(),
        duration_ms = report.duration_ms,
        "movies scan complete"
    );

    report
}

async fn enrich_pass(
    movies: &MovieRepo,
    library_id: uuid::Uuid,
    tmdb: &TmdbClient,
    report: &mut ScanReport,
) {
    let pending = match movies.list_unenriched_by_library(library_id).await {
        Ok(v) => v,
        Err(err) => {
            report.errors.push(format!("list unenriched: {err}"));
            return;
        }
    };

    info!(pending = pending.len(), "tmdb enrichment pass starting");

    for movie in pending {
        let search = match tmdb.search_movie(&movie.title, movie.year).await {
            Ok(v) => v,
            Err(err) => {
                warn!(
                    error = %err,
                    title = %movie.title,
                    year = ?movie.year,
                    "tmdb search failed"
                );
                report
                    .errors
                    .push(format!("tmdb search {}: {err}", movie.title));
                continue;
            }
        };
        let Some(matched) = search else {
            info!(
                title = %movie.title,
                year = ?movie.year,
                "tmdb: no match"
            );
            continue;
        };

        info!(
            title = %movie.title,
            tmdb_id = matched.tmdb_id,
            tmdb_title = %matched.title,
            tmdb_year = ?matched.release_year,
            has_poster = matched.poster_path.is_some(),
            "tmdb: matched"
        );

        let poster_url = match matched.poster_path.as_deref() {
            Some(path) => match tmdb.download_poster(path, &movie.id.to_string()).await {
                Ok(_) => Some(format!("/api/movies/{}/poster", movie.id)),
                Err(err) => {
                    warn!(
                        error = %err,
                        movie = %movie.title,
                        "poster download failed; persisting metadata without poster"
                    );
                    None
                }
            },
            None => None,
        };

        if let Err(err) = movies
            .apply_tmdb(
                movie.id,
                matched.tmdb_id,
                matched.overview.as_deref(),
                poster_url.as_deref(),
            )
            .await
        {
            report
                .errors
                .push(format!("apply tmdb {}: {err}", movie.title));
            continue;
        }
        report.enriched += 1;
    }
}
