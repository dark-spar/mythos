//! TV-kind library scanner.
//!
//! Walk → identify (S01E03) → probe → upsert media_file + series +
//! season + episode → prune empty parents. TMDb enrichment is a second
//! pass: search each un-matched series, then walk its seasons via
//! `/tv/{id}/season/{n}` to enrich episode metadata in one round-trip
//! per season.
//!
//! Files that don't match any supported TV filename pattern are
//! logged at WARN and counted in `ScanReport::errors`; the scan still
//! completes successfully. Operators can rename the file or add a
//! `Season N/SxxEyy` layout to pick them up on the next run.

use std::time::Instant;

use mythos_core::{Library, NewEpisode, NewMediaFile, NewSeason, NewSeries, Probe};
use mythos_db::{EpisodeRepo, MediaFileRepo, SeasonRepo, SeriesRepo};
use mythos_meta::TmdbClient;
use sqlx::SqlitePool;
use tracing::{info, warn};
use uuid::Uuid;

use crate::identify_tv::identify_tv;
use crate::probe::probe;
use crate::scan::{ScanReport, file_stats};
use crate::walk::video_files;

pub async fn scan_tv_library(
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
        "starting tv scan"
    );

    let files_repo = MediaFileRepo::new(pool.clone());
    let series_repo = SeriesRepo::new(pool.clone());
    let season_repo = SeasonRepo::new(pool.clone());
    let episode_repo = EpisodeRepo::new(pool.clone());

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

        let Some(identity) = identify_tv(&relative) else {
            warn!(
                file = %relative.display(),
                "unrecognized tv filename; expected S01E01 or 1x01 pattern"
            );
            report
                .errors
                .push(format!("unrecognized: {}", relative.display()));
            continue;
        };
        info!(
            file = %relative.display(),
            series = %identity.series,
            season = identity.season_number,
            episode = identity.episode_number,
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
                    series = %identity.series,
                    season = identity.season_number,
                    episode = identity.episode_number,
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

        let series = match series_repo
            .upsert(NewSeries {
                library_id: library.id,
                title: identity.series.clone(),
                year: identity.year,
            })
            .await
        {
            Ok(s) => s,
            Err(err) => {
                report
                    .errors
                    .push(format!("upsert series {}: {err}", identity.series));
                continue;
            }
        };

        let season = match season_repo
            .upsert(NewSeason {
                series_id: series.id,
                season_number: identity.season_number,
            })
            .await
        {
            Ok(s) => s,
            Err(err) => {
                report.errors.push(format!(
                    "upsert season {} S{}: {err}",
                    identity.series, identity.season_number
                ));
                continue;
            }
        };

        if let Err(err) = episode_repo
            .upsert(NewEpisode {
                season_id: season.id,
                file_id: file.id,
                episode_number: identity.episode_number,
                title: identity.episode_title.clone(),
            })
            .await
        {
            // A second file claiming the same (season_id, episode_number)
            // hits SQLite's secondary UNIQUE. Log + skip rather than
            // polluting ScanReport::errors: the first file already
            // owns the slot, the duplicate is usually an extras /
            // bonus rip the operator can rename later.
            if err.is_unique_violation() {
                warn!(
                    series = %identity.series,
                    season = identity.season_number,
                    episode = identity.episode_number,
                    file = %relative.display(),
                    "duplicate episode slot; another file already occupies S{}E{}",
                    identity.season_number,
                    identity.episode_number
                );
                continue;
            }
            report.errors.push(format!(
                "upsert episode {} S{}E{}: {err}",
                identity.series, identity.season_number, identity.episode_number
            ));
            continue;
        }

        let action = if inserted { "added" } else { "updated" };
        info!(
            series = %identity.series,
            season = identity.season_number,
            episode = identity.episode_number,
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

    // Seasons and series have no FK back to episodes / seasons, so
    // cascade-deletes don't clean up the parents when every child
    // vanishes. Do the sweep here.
    match season_repo.prune_empty_for_library(library.id).await {
        Ok(n) if n > 0 => info!(removed_seasons = n, "pruned empty seasons"),
        Ok(_) => {}
        Err(err) => report.errors.push(format!("prune seasons: {err}")),
    }
    match series_repo.prune_empty_for_library(library.id).await {
        Ok(n) if n > 0 => info!(removed_series = n, "pruned empty series"),
        Ok(_) => {}
        Err(err) => report.errors.push(format!("prune series: {err}")),
    }

    if let Some(client) = tmdb {
        enrich_pass(
            &series_repo,
            &season_repo,
            &episode_repo,
            library.id,
            client,
            &mut report,
        )
        .await;
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
        "tv scan complete"
    );

    report
}

async fn enrich_pass(
    series_repo: &SeriesRepo,
    season_repo: &SeasonRepo,
    episode_repo: &EpisodeRepo,
    library_id: Uuid,
    tmdb: &TmdbClient,
    report: &mut ScanReport,
) {
    let pending = match series_repo.list_unenriched_by_library(library_id).await {
        Ok(v) => v,
        Err(err) => {
            report.errors.push(format!("list unenriched series: {err}"));
            return;
        }
    };
    info!(pending = pending.len(), "tmdb tv enrichment pass starting");

    for series in pending {
        let search = match tmdb.search_tv(&series.title, series.year).await {
            Ok(v) => v,
            Err(err) => {
                warn!(
                    error = %err,
                    title = %series.title,
                    year = ?series.year,
                    "tmdb tv search failed"
                );
                report
                    .errors
                    .push(format!("tmdb search {}: {err}", series.title));
                continue;
            }
        };
        let Some(matched) = search else {
            info!(title = %series.title, year = ?series.year, "tmdb: no series match");
            continue;
        };
        info!(
            title = %series.title,
            tmdb_id = matched.tmdb_id,
            tmdb_title = %matched.name,
            tmdb_year = ?matched.first_air_year,
            has_poster = matched.poster_path.is_some(),
            "tmdb: matched series"
        );

        let poster_url = match matched.poster_path.as_deref() {
            Some(path) => match tmdb.download_poster(path, &series.id.to_string()).await {
                Ok(_) => Some(format!("/api/series/{}/poster", series.id)),
                Err(err) => {
                    warn!(
                        error = %err,
                        series = %series.title,
                        "series poster download failed"
                    );
                    None
                }
            },
            None => None,
        };

        if let Err(err) = series_repo
            .apply_tmdb(
                series.id,
                matched.tmdb_id,
                matched.overview.as_deref(),
                poster_url.as_deref(),
            )
            .await
        {
            report
                .errors
                .push(format!("apply tmdb series {}: {err}", series.title));
            continue;
        }
        report.enriched += 1;

        // Enrich each season's episodes in one TMDb request per season.
        let seasons = match season_repo.list_by_series(series.id).await {
            Ok(v) => v,
            Err(err) => {
                report
                    .errors
                    .push(format!("list seasons {}: {err}", series.title));
                continue;
            }
        };

        for season in seasons {
            let tmdb_season = match tmdb
                .get_tv_season(matched.tmdb_id, season.season_number)
                .await
            {
                Ok(v) => v,
                Err(err) => {
                    warn!(
                        error = %err,
                        series = %series.title,
                        season = season.season_number,
                        "tmdb season fetch failed"
                    );
                    report.errors.push(format!(
                        "tmdb season {} S{}: {err}",
                        series.title, season.season_number
                    ));
                    continue;
                }
            };

            if let Err(err) = season_repo
                .apply_tmdb(
                    season.id,
                    tmdb_season.tmdb_id,
                    tmdb_season.name.as_deref().filter(|s| !s.is_empty()),
                    tmdb_season.overview.as_deref().filter(|s| !s.is_empty()),
                    None,
                )
                .await
            {
                report.errors.push(format!(
                    "apply tmdb season {} S{}: {err}",
                    series.title, season.season_number
                ));
                continue;
            }

            let our_episodes = match episode_repo.list_by_season(season.id).await {
                Ok(v) => v,
                Err(err) => {
                    report.errors.push(format!(
                        "list episodes {} S{}: {err}",
                        series.title, season.season_number
                    ));
                    continue;
                }
            };

            for tmdb_ep in tmdb_season.episodes {
                let Some(ours) = our_episodes
                    .iter()
                    .find(|e| e.episode_number == tmdb_ep.episode_number)
                else {
                    continue;
                };
                if ours.tmdb_id.is_some() {
                    continue;
                }

                let still_url = match tmdb_ep.still_path.as_deref() {
                    Some(path) => match tmdb.download_still(path, &ours.id.to_string()).await {
                        Ok(_) => Some(format!("/api/episodes/{}/still", ours.id)),
                        Err(err) => {
                            warn!(
                                error = %err,
                                series = %series.title,
                                season = season.season_number,
                                episode = ours.episode_number,
                                "episode still download failed"
                            );
                            None
                        }
                    },
                    None => None,
                };

                if let Err(err) = episode_repo
                    .apply_tmdb(
                        ours.id,
                        tmdb_ep.tmdb_id,
                        tmdb_ep.name.as_deref().filter(|s| !s.is_empty()),
                        tmdb_ep.overview.as_deref().filter(|s| !s.is_empty()),
                        still_url.as_deref(),
                        tmdb_ep.air_date.as_deref().filter(|s| !s.is_empty()),
                    )
                    .await
                {
                    report.errors.push(format!(
                        "apply tmdb episode {} S{}E{}: {err}",
                        series.title, season.season_number, ours.episode_number
                    ));
                    continue;
                }
                report.enriched += 1;
            }
        }
    }
}
