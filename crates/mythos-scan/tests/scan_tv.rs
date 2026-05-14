//! Integration tests for `scan_library` on `LibraryKind::Shows`.
//!
//! Same in-memory SQLite + tempdir pattern as the movies tests. ffprobe
//! is not required — its failure mode is empty metadata, not a scan
//! abort.

use std::fs;
use std::path::Path;

use axum::Json;
use axum::Router;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;
use mythos_core::{Library, LibraryKind};
use mythos_db::{EpisodeRepo, LibraryRepo, MediaFileRepo, MovieRepo, SeasonRepo, SeriesRepo};
use mythos_meta::{TmdbClient, TmdbConfig};
use mythos_scan::scan_library;
use serde_json::json;
use sqlx::SqlitePool;
use tempfile::TempDir;
use tokio::net::TcpListener;
use uuid::Uuid;

async fn fresh_pool() -> SqlitePool {
    let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    sqlx::migrate!("../../migrations").run(&pool).await.unwrap();
    pool
}

async fn library(pool: &SqlitePool, kind: LibraryKind, root: &Path) -> Library {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO libraries (id, name, kind, root_path) VALUES (?, ?, ?, ?)")
        .bind(id.to_string())
        .bind("test")
        .bind(kind.as_str())
        .bind(root.to_str().unwrap())
        .execute(pool)
        .await
        .unwrap();
    LibraryRepo::new(pool.clone())
        .find_by_id(id)
        .await
        .unwrap()
        .unwrap()
}

fn touch(path: &Path) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, b"").unwrap();
}

#[tokio::test]
async fn scan_indexes_tv_files() {
    let dir = TempDir::new().unwrap();
    touch(&dir.path().join("Severance/Season 01/Severance.S01E01.mkv"));
    touch(&dir.path().join("Severance/Season 01/Severance.S01E02.mkv"));
    touch(
        &dir.path()
            .join("Severance/Season 01/notes-but-no-pattern.mkv"),
    );

    let pool = fresh_pool().await;
    let lib = library(&pool, LibraryKind::Shows, dir.path()).await;

    let report = scan_library(&pool, &lib, None).await;
    assert_eq!(report.added, 2, "two matched episodes should be added");
    assert_eq!(report.updated, 0);
    assert_eq!(
        report.errors.len(),
        1,
        "unrecognized file should produce one error entry; got: {:?}",
        report.errors
    );

    let series = SeriesRepo::new(pool.clone())
        .list_by_library(lib.id, 100, 0)
        .await
        .unwrap();
    assert_eq!(series.len(), 1);
    assert_eq!(series[0].title, "Severance");

    let seasons = SeasonRepo::new(pool.clone())
        .list_by_series(series[0].id)
        .await
        .unwrap();
    assert_eq!(seasons.len(), 1);
    assert_eq!(seasons[0].season_number, 1);

    let episodes = EpisodeRepo::new(pool.clone())
        .list_by_season(seasons[0].id)
        .await
        .unwrap();
    let nums: Vec<i64> = episodes.iter().map(|e| e.episode_number).collect();
    assert_eq!(nums, vec![1, 2]);

    let files = MediaFileRepo::new(pool)
        .list_by_library(lib.id)
        .await
        .unwrap();
    assert_eq!(
        files.len(),
        2,
        "non-tv file should not get a media_file row"
    );
}

#[tokio::test]
async fn rescan_marks_existing_episodes_as_updated() {
    let dir = TempDir::new().unwrap();
    touch(&dir.path().join("Show/S01E01.mkv"));
    touch(&dir.path().join("Show/S01E02.mkv"));

    let pool = fresh_pool().await;
    let lib = library(&pool, LibraryKind::Shows, dir.path()).await;

    let first = scan_library(&pool, &lib, None).await;
    assert_eq!(first.added, 2);

    let second = scan_library(&pool, &lib, None).await;
    assert_eq!(second.added, 0);
    assert_eq!(second.updated, 2);
    assert_eq!(second.removed, 0);
}

#[tokio::test]
async fn rescan_prunes_vanished_episodes_and_empty_parents() {
    let dir = TempDir::new().unwrap();
    let keep = dir.path().join("Show/S01E01.mkv");
    let drop = dir.path().join("Show/S01E02.mkv");
    touch(&keep);
    touch(&drop);

    let pool = fresh_pool().await;
    let lib = library(&pool, LibraryKind::Shows, dir.path()).await;

    let first = scan_library(&pool, &lib, None).await;
    assert_eq!(first.added, 2);

    fs::remove_file(&drop).unwrap();
    let second = scan_library(&pool, &lib, None).await;
    assert_eq!(second.removed, 1);

    let series = SeriesRepo::new(pool.clone())
        .list_by_library(lib.id, 100, 0)
        .await
        .unwrap();
    assert_eq!(series.len(), 1, "series stays while it still has episodes");
    let seasons = SeasonRepo::new(pool.clone())
        .list_by_series(series[0].id)
        .await
        .unwrap();
    assert_eq!(seasons.len(), 1);

    // Now remove the last episode and rescan: cascade through file → episode,
    // and the post-prune sweep should drop the empty season + series.
    fs::remove_file(&keep).unwrap();
    let third = scan_library(&pool, &lib, None).await;
    assert_eq!(third.removed, 1);

    let series = SeriesRepo::new(pool.clone())
        .list_by_library(lib.id, 100, 0)
        .await
        .unwrap();
    assert!(
        series.is_empty(),
        "empty series should be pruned, got {series:?}"
    );
}

#[tokio::test]
async fn non_tv_libraries_with_sxxeyy_names_dont_create_tv_rows() {
    // Same filenames look TV-ish, but a Movies-kind library must go
    // through the movies scanner — no series/season/episode rows.
    let dir = TempDir::new().unwrap();
    touch(&dir.path().join("Show.S01E01.mkv"));

    let pool = fresh_pool().await;
    let lib = library(&pool, LibraryKind::Movies, dir.path()).await;

    let report = scan_library(&pool, &lib, None).await;
    assert_eq!(report.added, 1);

    let series = SeriesRepo::new(pool.clone())
        .list_by_library(lib.id, 100, 0)
        .await
        .unwrap();
    assert!(series.is_empty());

    let movies = MovieRepo::new(pool)
        .list_by_library(lib.id, 100, 0)
        .await
        .unwrap();
    assert_eq!(
        movies.len(),
        1,
        "movies-kind library still indexes the file"
    );
}

#[tokio::test]
async fn duplicate_episode_slot_does_not_block_other_files() {
    // Two files identify as the same Show S01E01 (the second is some
    // extras rip with the same SxxEyy in its name). The first wins
    // the episode slot; the second is logged + skipped without
    // polluting ScanReport::errors. Other episodes still come through.
    let dir = TempDir::new().unwrap();
    touch(&dir.path().join("Show/Season 01/Show.S01E01.mkv"));
    touch(&dir.path().join("Show/Season 01/Show.S01E01.extras.mkv"));
    touch(&dir.path().join("Show/Season 01/Show.S01E02.mkv"));

    let pool = fresh_pool().await;
    let lib = library(&pool, LibraryKind::Shows, dir.path()).await;

    let report = scan_library(&pool, &lib, None).await;
    assert!(
        report.errors.is_empty(),
        "duplicate slot should not push to errors; got {:?}",
        report.errors
    );

    let series = SeriesRepo::new(pool.clone())
        .list_by_library(lib.id, 100, 0)
        .await
        .unwrap();
    assert_eq!(series.len(), 1);

    let seasons = SeasonRepo::new(pool.clone())
        .list_by_series(series[0].id)
        .await
        .unwrap();
    let episodes = EpisodeRepo::new(pool)
        .list_by_season(seasons[0].id)
        .await
        .unwrap();
    let nums: Vec<i64> = episodes.iter().map(|e| e.episode_number).collect();
    assert_eq!(
        nums,
        vec![1, 2],
        "both episode slots should be populated even when one had a duplicate"
    );
}

#[tokio::test]
async fn unimplemented_kinds_skip_cleanly() {
    let dir = TempDir::new().unwrap();
    touch(&dir.path().join("Show/S01E01.mkv"));

    let pool = fresh_pool().await;
    let lib = library(&pool, LibraryKind::Music, dir.path()).await;

    let report = scan_library(&pool, &lib, None).await;
    assert_eq!(report.added, 0);
    assert_eq!(report.updated, 0);
    assert_eq!(report.removed, 0);
    assert!(report.errors.is_empty());

    let series = SeriesRepo::new(pool.clone())
        .list_by_library(lib.id, 100, 0)
        .await
        .unwrap();
    assert!(series.is_empty());
    let files = MediaFileRepo::new(pool)
        .list_by_library(lib.id)
        .await
        .unwrap();
    assert!(files.is_empty());
}

async fn spawn_mock_tv_tmdb() -> (String, String) {
    let app = Router::new()
        .route(
            "/search/tv",
            get(|| async {
                Json(json!({
                    "results": [{
                        "id": 95396,
                        "name": "Severance",
                        "overview": "Mark leads a team of office workers whose memories...",
                        "first_air_date": "2022-02-18",
                        "poster_path": "/severance.jpg"
                    }]
                }))
                .into_response()
            }),
        )
        .route(
            "/tv/{id}/season/{season_number}",
            get(|| async {
                Json(json!({
                    "id": 144290,
                    "season_number": 1,
                    "name": "Season 1",
                    "overview": "First season blurb.",
                    "poster_path": "/season1.jpg",
                    "episodes": [
                        {
                            "id": 3025471,
                            "episode_number": 1,
                            "name": "Good News About Hell",
                            "overview": "Pilot",
                            "still_path": "/ep1.jpg",
                            "air_date": "2022-02-18"
                        },
                        {
                            "id": 3025472,
                            "episode_number": 2,
                            "name": "Half Loop",
                            "overview": "Ep 2",
                            "still_path": "/ep2.jpg",
                            "air_date": "2022-02-18"
                        }
                    ]
                }))
                .into_response()
            }),
        )
        .route(
            "/t/p/{size}/{*path}",
            get(|| async { ([(header::CONTENT_TYPE, "image/jpeg")], b"JPEGDATA".to_vec()) }),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), format!("http://{addr}/t/p"))
}

#[tokio::test]
async fn scan_with_tmdb_enriches_series_seasons_and_episodes() {
    let media = TempDir::new().unwrap();
    touch(
        &media
            .path()
            .join("Severance/Season 01/Severance.S01E01.mkv"),
    );
    touch(
        &media
            .path()
            .join("Severance/Season 01/Severance.S01E02.mkv"),
    );

    let posters = TempDir::new().unwrap();
    let (api_base, image_base) = spawn_mock_tv_tmdb().await;
    let client = TmdbClient::new(TmdbConfig {
        api_key: "fake".to_string(),
        api_base,
        image_base,
        poster_size: "w500".to_string(),
        posters_dir: posters.path().to_path_buf(),
    });

    let pool = fresh_pool().await;
    let lib = library(&pool, LibraryKind::Shows, media.path()).await;

    let report = scan_library(&pool, &lib, Some(&client)).await;
    assert_eq!(report.added, 2);
    // One series + two episodes enriched → 3 enrichment events
    assert_eq!(report.enriched, 3);
    assert!(report.errors.is_empty(), "errors: {:?}", report.errors);

    let series = SeriesRepo::new(pool.clone())
        .list_by_library(lib.id, 100, 0)
        .await
        .unwrap();
    assert_eq!(series.len(), 1);
    let s = &series[0];
    assert_eq!(s.tmdb_id, Some(95396));
    assert!(s.overview.as_deref().unwrap().contains("Mark"));
    assert_eq!(
        s.poster_url.as_deref().unwrap(),
        format!("/api/series/{}/poster", s.id)
    );

    // Series poster on disk
    let on_disk = posters.path().join(format!("{}.jpg", s.id));
    assert_eq!(std::fs::read(&on_disk).unwrap(), b"JPEGDATA");

    // Season + episodes
    let seasons = SeasonRepo::new(pool.clone())
        .list_by_series(s.id)
        .await
        .unwrap();
    assert_eq!(seasons.len(), 1);
    assert_eq!(seasons[0].tmdb_id, Some(144290));
    assert_eq!(seasons[0].title.as_deref(), Some("Season 1"));

    let episodes = EpisodeRepo::new(pool.clone())
        .list_by_season(seasons[0].id)
        .await
        .unwrap();
    assert_eq!(episodes.len(), 2);
    let ep1 = &episodes[0];
    assert_eq!(ep1.tmdb_id, Some(3025471));
    assert_eq!(ep1.title.as_deref(), Some("Good News About Hell"));
    assert_eq!(ep1.air_date.as_deref(), Some("2022-02-18"));
    assert_eq!(
        ep1.still_url.as_deref().unwrap(),
        format!("/api/episodes/{}/still", ep1.id)
    );

    // Episode still on disk
    let still = posters
        .path()
        .join("stills")
        .join(format!("{}.jpg", ep1.id));
    assert_eq!(std::fs::read(&still).unwrap(), b"JPEGDATA");
}

#[tokio::test]
async fn rescan_does_not_re_enrich_already_enriched_series() {
    let media = TempDir::new().unwrap();
    touch(
        &media
            .path()
            .join("Severance/Season 01/Severance.S01E01.mkv"),
    );

    let posters = TempDir::new().unwrap();
    let (api_base, image_base) = spawn_mock_tv_tmdb().await;
    let client = TmdbClient::new(TmdbConfig {
        api_key: "fake".to_string(),
        api_base,
        image_base,
        poster_size: "w500".to_string(),
        posters_dir: posters.path().to_path_buf(),
    });

    let pool = fresh_pool().await;
    let lib = library(&pool, LibraryKind::Shows, media.path()).await;

    let first = scan_library(&pool, &lib, Some(&client)).await;
    assert!(first.enriched >= 1);

    let second = scan_library(&pool, &lib, Some(&client)).await;
    assert_eq!(
        second.enriched, 0,
        "already-enriched series should be skipped on rescan"
    );
}
