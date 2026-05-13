//! Integration tests for `/api/libraries`.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use mythos_api::{ApiState, CookieConfig, PostersDir, ScanTracker, TmdbHandle};
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

fn auth_get(uri: &str, bearer: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
        .body(Body::empty())
        .unwrap()
}

fn auth_delete(uri: &str, bearer: &str) -> Request<Body> {
    Request::builder()
        .method("DELETE")
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

/// Register the first user (auto-admin) and return its bearer token.
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

/// Insert a non-admin user directly (no public API creates these yet)
/// and return a bearer token for them.
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

#[tokio::test]
async fn list_requires_auth() {
    let (router, _pool) = setup().await;
    let res = router.oneshot(anon_get("/api/libraries")).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn list_empty_by_default() {
    let (router, _pool) = setup().await;
    let bearer = admin_bearer(&router).await;

    let res = router
        .oneshot(auth_get("/api/libraries", &bearer))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(body_json(res).await, json!([]));
}

#[tokio::test]
async fn create_as_admin_succeeds() {
    let (router, _pool) = setup().await;
    let bearer = admin_bearer(&router).await;
    let dir = TempDir::new().unwrap();

    let res = router
        .oneshot(auth_post(
            "/api/libraries",
            &bearer,
            json!({
                "name": "Movies",
                "kind": "movies",
                "root_path": dir.path().to_str().unwrap(),
            }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_json(res).await;
    assert_eq!(body["name"], "Movies");
    assert_eq!(body["kind"], "movies");
    assert_eq!(body["root_path"], dir.path().to_str().unwrap());
    assert!(body["id"].is_string());
}

#[tokio::test]
async fn create_as_non_admin_returns_403() {
    let (router, pool) = setup().await;
    let _ = admin_bearer(&router).await; // bootstrap so register endpoint is closed
    let viewer = non_admin_bearer(&pool).await;
    let dir = TempDir::new().unwrap();

    let res = router
        .oneshot(auth_post(
            "/api/libraries",
            &viewer,
            json!({
                "name": "Movies",
                "kind": "movies",
                "root_path": dir.path().to_str().unwrap(),
            }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    assert_eq!(body_json(res).await, json!({ "error": "forbidden" }));
}

#[tokio::test]
async fn list_visible_to_non_admin() {
    let (router, pool) = setup().await;
    let admin = admin_bearer(&router).await;
    let viewer = non_admin_bearer(&pool).await;
    let dir = TempDir::new().unwrap();

    // Admin adds a library.
    router
        .clone()
        .oneshot(auth_post(
            "/api/libraries",
            &admin,
            json!({
                "name": "Movies",
                "kind": "movies",
                "root_path": dir.path().to_str().unwrap(),
            }),
        ))
        .await
        .unwrap();

    // Viewer can see it.
    let res = router
        .oneshot(auth_get("/api/libraries", &viewer))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_json(res).await;
    assert_eq!(body.as_array().unwrap().len(), 1);
    assert_eq!(body[0]["name"], "Movies");
}

#[tokio::test]
async fn create_rejects_relative_path() {
    let (router, _pool) = setup().await;
    let bearer = admin_bearer(&router).await;

    let res = router
        .oneshot(auth_post(
            "/api/libraries",
            &bearer,
            json!({
                "name": "Movies",
                "kind": "movies",
                "root_path": "movies/",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(res).await,
        json!({ "error": "root_path_not_absolute" })
    );
}

#[tokio::test]
async fn create_rejects_nonexistent_path() {
    let (router, _pool) = setup().await;
    let bearer = admin_bearer(&router).await;

    let res = router
        .oneshot(auth_post(
            "/api/libraries",
            &bearer,
            json!({
                "name": "Movies",
                "kind": "movies",
                "root_path": "/does-not-exist-anywhere-on-this-machine-zzz",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(res).await,
        json!({ "error": "root_path_not_found" })
    );
}

#[tokio::test]
async fn create_rejects_file_not_directory() {
    let (router, _pool) = setup().await;
    let bearer = admin_bearer(&router).await;
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("not-a-dir.txt");
    std::fs::write(&file, "x").unwrap();

    let res = router
        .oneshot(auth_post(
            "/api/libraries",
            &bearer,
            json!({
                "name": "Movies",
                "kind": "movies",
                "root_path": file.to_str().unwrap(),
            }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(res).await,
        json!({ "error": "root_path_not_directory" })
    );
}

#[tokio::test]
async fn create_rejects_empty_name() {
    let (router, _pool) = setup().await;
    let bearer = admin_bearer(&router).await;
    let dir = TempDir::new().unwrap();

    let res = router
        .oneshot(auth_post(
            "/api/libraries",
            &bearer,
            json!({
                "name": "   ",
                "kind": "movies",
                "root_path": dir.path().to_str().unwrap(),
            }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert_eq!(body_json(res).await, json!({ "error": "name_required" }));
}

#[tokio::test]
async fn duplicate_root_path_returns_409() {
    let (router, _pool) = setup().await;
    let bearer = admin_bearer(&router).await;
    let dir = TempDir::new().unwrap();
    let body = json!({
        "name": "Movies",
        "kind": "movies",
        "root_path": dir.path().to_str().unwrap(),
    });

    let res = router
        .clone()
        .oneshot(auth_post("/api/libraries", &bearer, body.clone()))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = router
        .oneshot(auth_post("/api/libraries", &bearer, body))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
    assert_eq!(body_json(res).await, json!({ "error": "root_path_taken" }));
}

#[tokio::test]
async fn get_one_returns_library() {
    let (router, _pool) = setup().await;
    let bearer = admin_bearer(&router).await;
    let dir = TempDir::new().unwrap();

    let res = router
        .clone()
        .oneshot(auth_post(
            "/api/libraries",
            &bearer,
            json!({
                "name": "Movies",
                "kind": "movies",
                "root_path": dir.path().to_str().unwrap(),
            }),
        ))
        .await
        .unwrap();
    let created = body_json(res).await;
    let id = created["id"].as_str().unwrap();

    let res = router
        .oneshot(auth_get(&format!("/api/libraries/{id}"), &bearer))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_json(res).await;
    assert_eq!(body["id"], id);
    assert_eq!(body["name"], "Movies");
}

#[tokio::test]
async fn get_one_unknown_id_returns_404() {
    let (router, _pool) = setup().await;
    let bearer = admin_bearer(&router).await;
    let missing = Uuid::now_v7();

    let res = router
        .oneshot(auth_get(&format!("/api/libraries/{missing}"), &bearer))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_as_admin_removes_library() {
    let (router, _pool) = setup().await;
    let bearer = admin_bearer(&router).await;
    let dir = TempDir::new().unwrap();

    let res = router
        .clone()
        .oneshot(auth_post(
            "/api/libraries",
            &bearer,
            json!({
                "name": "Movies",
                "kind": "movies",
                "root_path": dir.path().to_str().unwrap(),
            }),
        ))
        .await
        .unwrap();
    let id = body_json(res).await["id"].as_str().unwrap().to_string();

    let res = router
        .clone()
        .oneshot(auth_delete(&format!("/api/libraries/{id}"), &bearer))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);

    let res = router
        .oneshot(auth_get(&format!("/api/libraries/{id}"), &bearer))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_unknown_id_returns_404() {
    let (router, _pool) = setup().await;
    let bearer = admin_bearer(&router).await;
    let missing = Uuid::now_v7();

    let res = router
        .oneshot(auth_delete(&format!("/api/libraries/{missing}"), &bearer))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_as_non_admin_returns_403() {
    let (router, pool) = setup().await;
    let admin = admin_bearer(&router).await;
    let viewer = non_admin_bearer(&pool).await;
    let dir = TempDir::new().unwrap();

    let res = router
        .clone()
        .oneshot(auth_post(
            "/api/libraries",
            &admin,
            json!({
                "name": "Movies",
                "kind": "movies",
                "root_path": dir.path().to_str().unwrap(),
            }),
        ))
        .await
        .unwrap();
    let id = body_json(res).await["id"].as_str().unwrap().to_string();

    let res = router
        .oneshot(auth_delete(&format!("/api/libraries/{id}"), &viewer))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}
