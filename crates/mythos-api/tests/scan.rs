//! Integration tests for `/api/libraries/:id/scan` and the movies
//! browse endpoints. Scans run in background tasks against an
//! in-memory pool; we poll the status endpoint until completion.

use std::fs;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use mythos_api::{ApiState, CookieConfig, HlsHandle, PostersDir, ScanTracker, TmdbHandle, SubtitlesDir};
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
        subtitles_dir: SubtitlesDir(std::env::temp_dir()),
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

fn auth_post(uri: &str, bearer: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn auth_post_empty(uri: &str, bearer: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
        .body(Body::empty())
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

async fn body_json(res: axum::response::Response) -> Value {
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
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

async fn non_admin_bearer(pool: &SqlitePool) -> String {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO users (id, username, password_hash, is_admin) \
         VALUES (?, 'viewer', '$dummy', 0)",
    )
    .bind(id.to_string())
    .execute(pool)
    .await
    .unwrap();

    let cfg = TokenConfig::new(Arc::<[u8]>::from(TEST_SECRET), Duration::from_secs(60 * 60));
    token::issue(&cfg, id, 0).unwrap()
}

async fn create_library(router: &Router, bearer: &str, root: &std::path::Path) -> String {
    let res = router
        .clone()
        .oneshot(auth_post(
            "/api/libraries",
            bearer,
            json!({
                "name": "Movies",
                "kind": "movies",
                "root_path": root.to_str().unwrap(),
            }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    body_json(res).await["id"].as_str().unwrap().to_string()
}

async fn wait_for_completed(router: &Router, library_id: &str, bearer: &str) -> Value {
    for _ in 0..100 {
        let res = router
            .clone()
            .oneshot(auth_get(
                &format!("/api/libraries/{library_id}/scan"),
                bearer,
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_json(res).await;
        if body["state"] == "completed" {
            return body;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("scan did not complete in time");
}

#[tokio::test]
async fn scan_requires_admin() {
    let (router, pool) = setup().await;
    let _ = admin_bearer(&router).await;
    let viewer = non_admin_bearer(&pool).await;
    let dir = TempDir::new().unwrap();
    let id = create_library(
        &router,
        &admin_bearer_after_bootstrap(&router, &pool).await,
        dir.path(),
    )
    .await;

    let res = router
        .oneshot(auth_post_empty(
            &format!("/api/libraries/{id}/scan"),
            &viewer,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

// Helper for tests where admin has already bootstrapped: returns a
// bearer for an additional admin user (just re-uses the existing one).
async fn admin_bearer_after_bootstrap(router: &Router, _pool: &SqlitePool) -> String {
    let res = router
        .clone()
        .oneshot(json_post(
            "/api/auth/login",
            json!({ "username": "admin", "password": "hunter2hunter2" }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    body_json(res).await["token"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn scan_unknown_library_returns_404() {
    let (router, _pool) = setup().await;
    let bearer = admin_bearer(&router).await;
    let missing = Uuid::now_v7();

    let res = router
        .oneshot(auth_post_empty(
            &format!("/api/libraries/{missing}/scan"),
            &bearer,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn status_starts_idle() {
    let (router, _pool) = setup().await;
    let bearer = admin_bearer(&router).await;
    let dir = TempDir::new().unwrap();
    let id = create_library(&router, &bearer, dir.path()).await;

    let res = router
        .oneshot(auth_get(&format!("/api/libraries/{id}/scan"), &bearer))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(body_json(res).await, json!({ "state": "idle" }));
}

#[tokio::test]
async fn status_requires_auth() {
    let (router, _pool) = setup().await;
    let bearer = admin_bearer(&router).await;
    let dir = TempDir::new().unwrap();
    let id = create_library(&router, &bearer, dir.path()).await;

    let res = router
        .oneshot(anon_get(&format!("/api/libraries/{id}/scan")))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn scan_then_lists_movies() {
    let (router, _pool) = setup().await;
    let bearer = admin_bearer(&router).await;
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("The.Matrix.1999.mkv"), b"").unwrap();
    fs::write(dir.path().join("Inception 2010.mp4"), b"").unwrap();
    let id = create_library(&router, &bearer, dir.path()).await;

    let res = router
        .clone()
        .oneshot(auth_post_empty(
            &format!("/api/libraries/{id}/scan"),
            &bearer,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    assert_eq!(body_json(res).await["state"], "running");

    let final_state = wait_for_completed(&router, &id, &bearer).await;
    assert_eq!(final_state["added"], 2);
    assert_eq!(final_state["updated"], 0);
    assert_eq!(final_state["removed"], 0);

    // Browse the freshly-scanned library.
    let res = router
        .clone()
        .oneshot(auth_get(&format!("/api/libraries/{id}/movies"), &bearer))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_json(res).await;
    assert_eq!(body["total"], 2);
    let titles: Vec<&str> = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["title"].as_str().unwrap())
        .collect();
    assert!(titles.contains(&"Inception"));
    assert!(titles.contains(&"The Matrix"));

    // Fetch one detail.
    let movie_id = body["items"][0]["id"].as_str().unwrap().to_string();
    let res = router
        .oneshot(auth_get(&format!("/api/movies/{movie_id}"), &bearer))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let detail = body_json(res).await;
    assert_eq!(detail["movie"]["id"], movie_id);
    assert!(detail["file"]["path"].is_string());
}

#[tokio::test]
async fn movie_detail_unknown_id_returns_404() {
    let (router, _pool) = setup().await;
    let bearer = admin_bearer(&router).await;
    let missing = Uuid::now_v7();

    let res = router
        .oneshot(auth_get(&format!("/api/movies/{missing}"), &bearer))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn movies_list_requires_auth() {
    let (router, _pool) = setup().await;
    let bearer = admin_bearer(&router).await;
    let dir = TempDir::new().unwrap();
    let id = create_library(&router, &bearer, dir.path()).await;

    let res = router
        .oneshot(anon_get(&format!("/api/libraries/{id}/movies")))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn second_scan_marks_files_as_updated() {
    let (router, _pool) = setup().await;
    let bearer = admin_bearer(&router).await;
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("The.Matrix.1999.mkv"), b"").unwrap();
    let id = create_library(&router, &bearer, dir.path()).await;

    // First scan.
    router
        .clone()
        .oneshot(auth_post_empty(
            &format!("/api/libraries/{id}/scan"),
            &bearer,
        ))
        .await
        .unwrap();
    wait_for_completed(&router, &id, &bearer).await;

    // Second scan should mark the row as updated, not added.
    router
        .clone()
        .oneshot(auth_post_empty(
            &format!("/api/libraries/{id}/scan"),
            &bearer,
        ))
        .await
        .unwrap();
    let second = wait_for_completed(&router, &id, &bearer).await;
    assert_eq!(second["added"], 0);
    assert_eq!(second["updated"], 1);
}
