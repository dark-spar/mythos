//! Integration tests for `/api/movies/:id/stream` and
//! `/api/movies/:id/progress`.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use mythos_api::{ApiState, CookieConfig, HlsHandle, PostersDir, ScanTracker, TmdbHandle};
use mythos_auth::{TokenConfig, token};
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tempfile::TempDir;
use tower::ServiceExt;
use uuid::Uuid;

const TEST_SECRET: &[u8] = b"test-secret-must-be-32-bytes-or-more-here";

async fn setup() -> (Router, SqlitePool) {
    let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    sqlx::migrate!("../../migrations").run(&pool).await.unwrap();
    let token = TokenConfig::new(Arc::<[u8]>::from(TEST_SECRET), Duration::from_secs(60 * 60));
    let cookies = CookieConfig { secure: false };
    let router = mythos_api::router(ApiState {
        db: pool.clone(),
        token,
        cookies,
        scans: ScanTracker::new(),
        tmdb: TmdbHandle::default(),
        posters_dir: PostersDir(std::env::temp_dir()),
        hls: HlsHandle::default(),
    });
    (router, pool)
}

fn json_post(uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn auth_put(uri: &str, bearer: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method("PUT")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
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

fn auth_get_with_range(uri: &str, bearer: &str, range: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
        .header(header::RANGE, range)
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

async fn body_bytes(res: axum::response::Response) -> Vec<u8> {
    res.into_body().collect().await.unwrap().to_bytes().to_vec()
}

async fn body_json(res: axum::response::Response) -> Value {
    let bytes = body_bytes(res).await;
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
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
    body_json(res).await["token"].as_str().unwrap().to_string()
}

async fn insert_user(pool: &SqlitePool, username: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO users (id, username, password_hash, is_admin) \
         VALUES (?, ?, '$dummy', 0)",
    )
    .bind(id.to_string())
    .bind(username)
    .execute(pool)
    .await
    .unwrap();
    id
}

fn bearer_for(user_id: Uuid) -> String {
    let cfg = TokenConfig::new(Arc::<[u8]>::from(TEST_SECRET), Duration::from_secs(60 * 60));
    token::issue(&cfg, user_id, 0).unwrap()
}

/// Build a movie + media_file + library directly in the DB pointing at
/// a real file on disk. Returns the movie id and the TempDir guard —
/// the caller must keep the guard alive for the duration of the test
/// or the file will vanish.
async fn create_movie_on_disk(pool: &SqlitePool, content: &[u8]) -> (Uuid, TempDir) {
    let dir = TempDir::new().unwrap();
    let filename = "movie.mp4";
    fs::write(dir.path().join(filename), content).unwrap();

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
        "INSERT INTO media_files (id, library_id, path, size_bytes, mtime) \
         VALUES (?, ?, ?, ?, '2026-01-01T00:00:00.000Z')",
    )
    .bind(file_id.to_string())
    .bind(library_id.to_string())
    .bind(filename)
    .bind(content.len() as i64)
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

fn content_1k() -> Vec<u8> {
    (0..1000u16).map(|i| (i & 0xff) as u8).collect()
}

#[tokio::test]
async fn stream_requires_auth() {
    let (router, pool) = setup().await;
    let (movie_id, _dir) = create_movie_on_disk(&pool, &content_1k()).await;

    let res = router
        .oneshot(anon_get(&format!("/api/movies/{movie_id}/stream")))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn stream_unknown_movie_returns_404() {
    let (router, _pool) = setup().await;
    let bearer = admin_bearer(&router).await;
    let missing = Uuid::now_v7();

    let res = router
        .oneshot(auth_get(&format!("/api/movies/{missing}/stream"), &bearer))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn stream_file_gone_from_disk_returns_404() {
    let (router, pool) = setup().await;
    let bearer = admin_bearer(&router).await;
    let (movie_id, dir) = create_movie_on_disk(&pool, &content_1k()).await;

    // Nuke the file but leave the DB row.
    fs::remove_file(PathBuf::from(dir.path()).join("movie.mp4")).unwrap();

    let res = router
        .oneshot(auth_get(&format!("/api/movies/{movie_id}/stream"), &bearer))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    assert_eq!(body_json(res).await, json!({ "error": "file_missing" }));
}

#[tokio::test]
async fn stream_without_range_returns_full_file() {
    let (router, pool) = setup().await;
    let bearer = admin_bearer(&router).await;
    let content = content_1k();
    let (movie_id, _dir) = create_movie_on_disk(&pool, &content).await;

    let res = router
        .oneshot(auth_get(&format!("/api/movies/{movie_id}/stream"), &bearer))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok()),
        Some(content.len().to_string().as_str())
    );
    assert_eq!(
        res.headers()
            .get(header::ACCEPT_RANGES)
            .and_then(|v| v.to_str().ok()),
        Some("bytes")
    );
    let body = body_bytes(res).await;
    assert_eq!(body, content);
}

#[tokio::test]
async fn stream_returns_partial_content_for_byte_range() {
    let (router, pool) = setup().await;
    let bearer = admin_bearer(&router).await;
    let content = content_1k();
    let (movie_id, _dir) = create_movie_on_disk(&pool, &content).await;

    let res = router
        .oneshot(auth_get_with_range(
            &format!("/api/movies/{movie_id}/stream"),
            &bearer,
            "bytes=100-199",
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        res.headers()
            .get(header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok()),
        Some("bytes 100-199/1000")
    );
    assert_eq!(
        res.headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok()),
        Some("100")
    );
    let body = body_bytes(res).await;
    assert_eq!(body, &content[100..200]);
}

#[tokio::test]
async fn stream_open_ended_range_serves_to_end() {
    let (router, pool) = setup().await;
    let bearer = admin_bearer(&router).await;
    let content = content_1k();
    let (movie_id, _dir) = create_movie_on_disk(&pool, &content).await;

    let res = router
        .oneshot(auth_get_with_range(
            &format!("/api/movies/{movie_id}/stream"),
            &bearer,
            "bytes=900-",
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
    let body = body_bytes(res).await;
    assert_eq!(body, &content[900..]);
}

#[tokio::test]
async fn stream_suffix_range_returns_last_n_bytes() {
    let (router, pool) = setup().await;
    let bearer = admin_bearer(&router).await;
    let content = content_1k();
    let (movie_id, _dir) = create_movie_on_disk(&pool, &content).await;

    let res = router
        .oneshot(auth_get_with_range(
            &format!("/api/movies/{movie_id}/stream"),
            &bearer,
            "bytes=-50",
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        res.headers()
            .get(header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok()),
        Some("bytes 950-999/1000")
    );
}

#[tokio::test]
async fn stream_returns_416_for_out_of_bounds_range() {
    let (router, pool) = setup().await;
    let bearer = admin_bearer(&router).await;
    let (movie_id, _dir) = create_movie_on_disk(&pool, &content_1k()).await;

    let res = router
        .oneshot(auth_get_with_range(
            &format!("/api/movies/{movie_id}/stream"),
            &bearer,
            "bytes=9999-",
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(
        res.headers()
            .get(header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok()),
        Some("bytes */1000")
    );
}

#[tokio::test]
async fn progress_round_trip_via_movie_detail() {
    let (router, pool) = setup().await;
    let bearer = admin_bearer(&router).await;
    let (movie_id, _dir) = create_movie_on_disk(&pool, &content_1k()).await;

    // Initially: no progress in the detail response.
    let res = router
        .clone()
        .oneshot(auth_get(&format!("/api/movies/{movie_id}"), &bearer))
        .await
        .unwrap();
    let body = body_json(res).await;
    assert!(body["progress"].is_null(), "got {body}");

    // PUT progress.
    let res = router
        .clone()
        .oneshot(auth_put(
            &format!("/api/movies/{movie_id}/progress"),
            &bearer,
            json!({ "position_seconds": 123.5, "duration_seconds": 600.0 }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);

    // Detail now includes it.
    let res = router
        .oneshot(auth_get(&format!("/api/movies/{movie_id}"), &bearer))
        .await
        .unwrap();
    let body = body_json(res).await;
    assert_eq!(body["progress"]["position_seconds"], 123.5);
    assert_eq!(body["progress"]["duration_seconds"], 600.0);
    assert!(body["progress"]["updated_at"].is_string());
}

#[tokio::test]
async fn progress_is_per_user() {
    let (router, pool) = setup().await;
    let _admin = admin_bearer(&router).await;
    let alice_id = insert_user(&pool, "alice").await;
    let bob_id = insert_user(&pool, "bob").await;
    let alice = bearer_for(alice_id);
    let bob = bearer_for(bob_id);
    let (movie_id, _dir) = create_movie_on_disk(&pool, &content_1k()).await;

    // Alice writes 50s, Bob writes 200s.
    router
        .clone()
        .oneshot(auth_put(
            &format!("/api/movies/{movie_id}/progress"),
            &alice,
            json!({ "position_seconds": 50.0, "duration_seconds": 600.0 }),
        ))
        .await
        .unwrap();
    router
        .clone()
        .oneshot(auth_put(
            &format!("/api/movies/{movie_id}/progress"),
            &bob,
            json!({ "position_seconds": 200.0, "duration_seconds": 600.0 }),
        ))
        .await
        .unwrap();

    let alice_detail = body_json(
        router
            .clone()
            .oneshot(auth_get(&format!("/api/movies/{movie_id}"), &alice))
            .await
            .unwrap(),
    )
    .await;
    let bob_detail = body_json(
        router
            .oneshot(auth_get(&format!("/api/movies/{movie_id}"), &bob))
            .await
            .unwrap(),
    )
    .await;

    assert_eq!(alice_detail["progress"]["position_seconds"], 50.0);
    assert_eq!(bob_detail["progress"]["position_seconds"], 200.0);
}

#[tokio::test]
async fn progress_rejects_invalid_payload() {
    let (router, pool) = setup().await;
    let bearer = admin_bearer(&router).await;
    let (movie_id, _dir) = create_movie_on_disk(&pool, &content_1k()).await;

    let res = router
        .clone()
        .oneshot(auth_put(
            &format!("/api/movies/{movie_id}/progress"),
            &bearer,
            json!({ "position_seconds": -1.0, "duration_seconds": 600.0 }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let res = router
        .oneshot(auth_put(
            &format!("/api/movies/{movie_id}/progress"),
            &bearer,
            json!({ "position_seconds": 0.0, "duration_seconds": 0.0 }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn progress_unknown_movie_returns_404() {
    let (router, _pool) = setup().await;
    let bearer = admin_bearer(&router).await;
    let missing = Uuid::now_v7();

    let res = router
        .oneshot(auth_put(
            &format!("/api/movies/{missing}/progress"),
            &bearer,
            json!({ "position_seconds": 1.0, "duration_seconds": 60.0 }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}
