//! Integration tests for `scan_library` against an in-memory SQLite
//! pool. These don't need ffprobe installed: ffprobe failures map to
//! empty `Probe` fields and the scan continues.

use std::fs;
use std::path::Path;

use mythos_core::{Library, LibraryKind};
use mythos_db::{LibraryRepo, MediaFileRepo, MovieRepo};
use mythos_scan::scan_library;
use sqlx::SqlitePool;
use tempfile::TempDir;
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

#[tokio::test]
async fn scan_indexes_video_files() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("The.Matrix.1999.mkv"), b"").unwrap();
    fs::write(dir.path().join("Inception 2010.mp4"), b"").unwrap();
    fs::write(dir.path().join("notes.txt"), b"ignored").unwrap();

    let pool = fresh_pool().await;
    let lib = library(&pool, LibraryKind::Movies, dir.path()).await;

    let report = scan_library(&pool, &lib).await;
    assert_eq!(report.added, 2, "two video files should be added");
    assert_eq!(report.updated, 0);
    assert_eq!(report.removed, 0);

    let movies = MovieRepo::new(pool.clone())
        .list_by_library(lib.id, 100, 0)
        .await
        .unwrap();
    let mut titles: Vec<String> = movies.iter().map(|m| m.title.clone()).collect();
    titles.sort();
    assert_eq!(
        titles,
        vec!["Inception".to_string(), "The Matrix".to_string()]
    );

    let files = MediaFileRepo::new(pool)
        .list_by_library(lib.id)
        .await
        .unwrap();
    assert_eq!(files.len(), 2);
}

#[tokio::test]
async fn rescan_marks_existing_rows_as_updated() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("The.Matrix.1999.mkv"), b"").unwrap();

    let pool = fresh_pool().await;
    let lib = library(&pool, LibraryKind::Movies, dir.path()).await;

    let first = scan_library(&pool, &lib).await;
    assert_eq!(first.added, 1);

    let second = scan_library(&pool, &lib).await;
    assert_eq!(second.added, 0, "rescan should not add the same file again");
    assert_eq!(second.updated, 1);
    assert_eq!(second.removed, 0);
}

#[tokio::test]
async fn rescan_prunes_vanished_files() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("Stays.2020.mkv"), b"").unwrap();
    let to_remove = dir.path().join("Goes.2020.mkv");
    fs::write(&to_remove, b"").unwrap();

    let pool = fresh_pool().await;
    let lib = library(&pool, LibraryKind::Movies, dir.path()).await;

    let first = scan_library(&pool, &lib).await;
    assert_eq!(first.added, 2);

    fs::remove_file(&to_remove).unwrap();

    let second = scan_library(&pool, &lib).await;
    assert_eq!(second.added, 0);
    assert_eq!(second.updated, 1);
    assert_eq!(second.removed, 1);

    let movies = MovieRepo::new(pool)
        .list_by_library(lib.id, 100, 0)
        .await
        .unwrap();
    assert_eq!(movies.len(), 1);
    assert_eq!(movies[0].title, "Stays");
}

#[tokio::test]
async fn non_movies_libraries_are_skipped() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("song.mp4"), b"").unwrap();

    let pool = fresh_pool().await;
    let lib = library(&pool, LibraryKind::Music, dir.path()).await;

    let report = scan_library(&pool, &lib).await;
    assert_eq!(report.added, 0);
    assert_eq!(report.updated, 0);
    assert_eq!(report.removed, 0);
    assert!(report.errors.is_empty());

    let files = MediaFileRepo::new(pool)
        .list_by_library(lib.id)
        .await
        .unwrap();
    assert!(files.is_empty(), "no file rows should be created");
}

#[tokio::test]
async fn empty_library_with_no_files_is_a_clean_no_op() {
    let dir = TempDir::new().unwrap();

    let pool = fresh_pool().await;
    let lib = library(&pool, LibraryKind::Movies, dir.path()).await;

    let report = scan_library(&pool, &lib).await;
    assert_eq!(report.added, 0);
    assert_eq!(report.updated, 0);
    assert_eq!(report.removed, 0);
    assert!(report.errors.is_empty());
}
