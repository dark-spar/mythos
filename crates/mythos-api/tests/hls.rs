//! Integration tests for `/api/movies/:id/hls/*`.
//!
//! These spawn a real `ffmpeg` against an ffmpeg-generated input, so
//! they need `ffmpeg` on PATH.

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use mythos_api::{ApiState, CookieConfig, HlsHandle, PostersDir, ScanTracker, TmdbHandle, SubtitlesDir};
use mythos_auth::TokenConfig;
use mythos_stream::TranscodeManager;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tempfile::TempDir;
use tower::ServiceExt;
use uuid::Uuid;

const TEST_SECRET: &[u8] = b"test-secret-must-be-32-bytes-or-more-here";

async fn setup() -> (Router, SqlitePool, TempDir) {
    let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    sqlx::migrate!("../../migrations").run(&pool).await.unwrap();
    let token = TokenConfig::new(Arc::<[u8]>::from(TEST_SECRET), Duration::from_secs(60 * 60));
    let cookies = CookieConfig { secure: false };
    let transcode_dir = TempDir::new().unwrap();
    let manager = TranscodeManager::new(
        transcode_dir.path().to_path_buf(),
        mythos_stream::HwAccel::Cpu,
    );
    let router = mythos_api::router(ApiState {
        db: pool.clone(),
        token,
        cookies,
        scans: ScanTracker::new(),
        tmdb: TmdbHandle::default(),
        posters_dir: PostersDir(std::env::temp_dir()),
        subtitles_dir: SubtitlesDir(std::env::temp_dir()),
        hls: HlsHandle(Some(manager)),
    });
    (router, pool, transcode_dir)
}

fn make_test_input(dir: &std::path::Path) -> PathBuf {
    let path = dir.join("movie.mp4");
    // Synthetic video + silent audio — the ABR transcoder's
    // var_stream_map references audio for each rendition, so a
    // video-only input would fail to mux.
    let status = Command::new("ffmpeg")
        .args([
            "-y",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=red:size=160x90:duration=15:rate=10",
            "-f",
            "lavfi",
            "-i",
            "anullsrc=channel_layout=stereo:sample_rate=48000",
            "-shortest",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
        ])
        .arg(&path)
        .status()
        .expect("ffmpeg on PATH");
    assert!(status.success(), "ffmpeg failed to build test input");
    path
}

/// Build a movie row whose `media_files.duration_seconds` is set, so
/// the playlist endpoint can advertise a real timeline.
async fn create_movie_on_disk(pool: &SqlitePool, duration_seconds: Option<f64>) -> (Uuid, TempDir) {
    let dir = TempDir::new().unwrap();
    make_test_input(dir.path());

    let library_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO libraries (id, name, kind, root_path) VALUES (?, 'test', 'movies', ?)",
    )
    .bind(library_id.to_string())
    .bind(dir.path().to_str().unwrap())
    .execute(pool)
    .await
    .unwrap();

    let file_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO media_files (id, library_id, path, size_bytes, mtime, duration_seconds) \
         VALUES (?, ?, 'movie.mp4', 0, '2026-01-01T00:00:00.000Z', ?)",
    )
    .bind(file_id.to_string())
    .bind(library_id.to_string())
    .bind(duration_seconds)
    .execute(pool)
    .await
    .unwrap();

    let movie_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO movies (id, library_id, file_id, title, sort_title) \
         VALUES (?, ?, ?, 'Test', 'Test')",
    )
    .bind(movie_id.to_string())
    .bind(library_id.to_string())
    .bind(file_id.to_string())
    .execute(pool)
    .await
    .unwrap();

    (movie_id, dir)
}

fn json_post(uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn auth_get(uri: &str, bearer: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
        .body(Body::empty())
        .unwrap()
}

fn anon_get(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

async fn admin_bearer(router: &Router) -> String {
    let res = router
        .clone()
        .oneshot(json_post(
            "/api/auth/register",
            json!({ "username": "admin", "password": "hunter2hunter2" }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    v["token"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn master_requires_auth() {
    let (router, pool, _tdir) = setup().await;
    let (movie_id, _dir) = create_movie_on_disk(&pool, Some(15.0)).await;

    let res = router
        .oneshot(anon_get(&format!("/api/movies/{movie_id}/hls/master.m3u8")))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn master_unknown_movie_returns_404() {
    let (router, _pool, _tdir) = setup().await;
    let bearer = admin_bearer(&router).await;
    let missing = Uuid::now_v7();

    let res = router
        .oneshot(auth_get(
            &format!("/api/movies/{missing}/hls/master.m3u8"),
            &bearer,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn master_lists_every_rendition() {
    let (router, pool, _tdir) = setup().await;
    let bearer = admin_bearer(&router).await;
    let (movie_id, _dir) = create_movie_on_disk(&pool, Some(15.0)).await;

    let res = router
        .oneshot(auth_get(
            &format!("/api/movies/{movie_id}/hls/master.m3u8"),
            &bearer,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let text = std::str::from_utf8(&body).unwrap();
    assert!(text.contains("#EXT-X-STREAM-INF"));
    assert!(text.contains("480p/playlist.m3u8"));
    assert!(text.contains("720p/playlist.m3u8"));
    assert!(text.contains("1080p/playlist.m3u8"));
}

#[tokio::test]
async fn variant_playlist_lists_every_segment_for_full_duration() {
    let (router, pool, _tdir) = setup().await;
    let bearer = admin_bearer(&router).await;
    // 15s with 6s segments → seg-0 (6s) + seg-1 (6s) + seg-2 (3s).
    let (movie_id, _dir) = create_movie_on_disk(&pool, Some(15.0)).await;

    let res = router
        .oneshot(auth_get(
            &format!("/api/movies/{movie_id}/hls/480p/playlist.m3u8"),
            &bearer,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let text = std::str::from_utf8(&body).unwrap();
    assert!(text.contains("#EXT-X-PLAYLIST-TYPE:VOD"));
    assert!(text.contains("seg-0.ts"));
    assert!(text.contains("seg-1.ts"));
    assert!(text.contains("seg-2.ts"));
    assert!(!text.contains("seg-3.ts"));
    assert!(text.ends_with("#EXT-X-ENDLIST\n"));
}

#[tokio::test]
async fn unknown_variant_returns_404() {
    let (router, pool, _tdir) = setup().await;
    let bearer = admin_bearer(&router).await;
    let (movie_id, _dir) = create_movie_on_disk(&pool, Some(15.0)).await;

    let res = router
        .oneshot(auth_get(
            &format!("/api/movies/{movie_id}/hls/4320p/playlist.m3u8"),
            &bearer,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn master_without_known_duration_returns_422() {
    let (router, pool, _tdir) = setup().await;
    let bearer = admin_bearer(&router).await;
    let (movie_id, _dir) = create_movie_on_disk(&pool, None).await;

    let res = router
        .oneshot(auth_get(
            &format!("/api/movies/{movie_id}/hls/master.m3u8"),
            &bearer,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v, json!({ "error": "unknown_duration" }));
}

#[tokio::test]
async fn segment_request_creates_session_and_serves_ts_bytes() {
    let (router, pool, _tdir) = setup().await;
    let bearer = admin_bearer(&router).await;
    let (movie_id, _dir) = create_movie_on_disk(&pool, Some(15.0)).await;

    let res = router
        .oneshot(auth_get(
            &format!("/api/movies/{movie_id}/hls/480p/seg-0.ts"),
            &bearer,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("video/mp2t")
    );
    let body = res.into_body().collect().await.unwrap().to_bytes();
    assert!(!body.is_empty());
    assert_eq!(body[0], 0x47, "MPEG-TS sync byte");
}

#[tokio::test]
async fn segment_at_arbitrary_index_restarts_transcoder() {
    let (router, pool, _tdir) = setup().await;
    let bearer = admin_bearer(&router).await;
    let (movie_id, _dir) = create_movie_on_disk(&pool, Some(15.0)).await;

    // Jumping straight to seg-1 (offset 6s) should still work — the
    // session starts there instead of at 0.
    let res = router
        .oneshot(auth_get(
            &format!("/api/movies/{movie_id}/hls/720p/seg-1.ts"),
            &bearer,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn segment_with_traversal_filename_is_rejected() {
    let (router, pool, _tdir) = setup().await;
    let bearer = admin_bearer(&router).await;
    let (movie_id, _dir) = create_movie_on_disk(&pool, Some(15.0)).await;

    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/movies/{movie_id}/hls/480p/..%2Fetc%2Fpasswd"))
        .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
        .body(Body::empty())
        .unwrap();
    let res = router.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}
