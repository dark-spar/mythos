//! End-to-end integration tests for the auth surface.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use mythos_api::{ApiState, CookieConfig, HlsHandle, PostersDir, ScanTracker, TmdbHandle};
use mythos_auth::{Claims, TokenConfig};
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tower::ServiceExt;
use uuid::Uuid;

const TEST_SECRET: &[u8] = b"test-secret-must-be-32-bytes-or-more-here";

async fn setup() -> (Router, SqlitePool, TokenConfig) {
    let pool = SqlitePool::connect("sqlite::memory:")
        .await
        .expect("open in-memory sqlite");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("run migrations");
    let token = TokenConfig::new(Arc::<[u8]>::from(TEST_SECRET), Duration::from_secs(60 * 60));
    let cookies = CookieConfig { secure: false };
    let router = mythos_api::router(ApiState {
        db: pool.clone(),
        token: token.clone(),
        cookies,
        scans: ScanTracker::new(),
        tmdb: TmdbHandle::default(),
        posters_dir: PostersDir(std::env::temp_dir()),
        hls: HlsHandle::default(),
    });
    (router, pool, token)
}

async fn json_body(res: axum::response::Response) -> Value {
    let bytes = res
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

fn json_post(uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn get(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

fn extract_set_cookie(res: &axum::response::Response) -> Option<String> {
    res.headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

#[tokio::test]
async fn status_flips_after_first_user() {
    let (router, _pool, _token) = setup().await;

    let res = router
        .clone()
        .oneshot(get("/api/auth/status"))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(json_body(res).await, json!({ "bootstrapped": false }));

    let res = router
        .clone()
        .oneshot(json_post(
            "/api/auth/register",
            json!({ "username": "admin", "password": "hunter2hunter2" }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = router.oneshot(get("/api/auth/status")).await.unwrap();
    assert_eq!(json_body(res).await, json!({ "bootstrapped": true }));
}

#[tokio::test]
async fn register_succeeds_once_then_403() {
    let (router, _pool, _token) = setup().await;

    let req = json_post(
        "/api/auth/register",
        json!({ "username": "admin", "password": "hunter2hunter2" }),
    );
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(
        extract_set_cookie(&res).is_some(),
        "register must set cookie"
    );
    let body = json_body(res).await;
    assert!(body["token"].is_string());
    assert_eq!(body["user"]["username"], "admin");
    assert_eq!(body["user"]["is_admin"], true);
    assert!(body["user"]["password_hash"].is_null());

    let res = router
        .oneshot(json_post(
            "/api/auth/register",
            json!({ "username": "second", "password": "anothergoodpassword" }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    assert_eq!(json_body(res).await, json!({ "error": "forbidden" }));
}

#[tokio::test]
async fn register_rejects_short_password() {
    let (router, _pool, _token) = setup().await;
    let res = router
        .oneshot(json_post(
            "/api/auth/register",
            json!({ "username": "admin", "password": "short" }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn login_success_sets_cookie_and_returns_token() {
    let (router, _pool, _token) = setup().await;
    router
        .clone()
        .oneshot(json_post(
            "/api/auth/register",
            json!({ "username": "admin", "password": "hunter2hunter2" }),
        ))
        .await
        .unwrap();

    let res = router
        .oneshot(json_post(
            "/api/auth/login",
            json!({ "username": "admin", "password": "hunter2hunter2" }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let cookie = extract_set_cookie(&res).expect("login must set cookie");
    assert!(cookie.contains("mythos_token="));
    assert!(cookie.contains("HttpOnly"));
    assert!(cookie.contains("SameSite=Lax"));
    let body = json_body(res).await;
    assert!(body["token"].is_string());
}

#[tokio::test]
async fn login_wrong_password_returns_401() {
    let (router, _pool, _token) = setup().await;
    router
        .clone()
        .oneshot(json_post(
            "/api/auth/register",
            json!({ "username": "admin", "password": "hunter2hunter2" }),
        ))
        .await
        .unwrap();

    let res = router
        .oneshot(json_post(
            "/api/auth/login",
            json!({ "username": "admin", "password": "wrongpassword" }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        json_body(res).await,
        json!({ "error": "invalid_credentials" })
    );
}

#[tokio::test]
async fn login_unknown_user_returns_401() {
    let (router, _pool, _token) = setup().await;
    let res = router
        .oneshot(json_post(
            "/api/auth/login",
            json!({ "username": "ghost", "password": "anyoldthing" }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        json_body(res).await,
        json!({ "error": "invalid_credentials" })
    );
}

async fn register_and_login(router: &Router) -> (String, String) {
    let res = router
        .clone()
        .oneshot(json_post(
            "/api/auth/register",
            json!({ "username": "admin", "password": "hunter2hunter2" }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let cookie = extract_set_cookie(&res).expect("set-cookie present");
    // Pull only the key=value portion, drop the attributes.
    let cookie_pair = cookie.split(';').next().unwrap().trim().to_string();
    let body = json_body(res).await;
    let token = body["token"].as_str().unwrap().to_string();
    (cookie_pair, token)
}

#[tokio::test]
async fn me_via_cookie() {
    let (router, _pool, _token) = setup().await;
    let (cookie, _bearer) = register_and_login(&router).await;

    let req = Request::builder()
        .method("GET")
        .uri("/api/users/me")
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap();
    let res = router.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = json_body(res).await;
    assert_eq!(body["username"], "admin");
}

#[tokio::test]
async fn me_via_bearer() {
    let (router, _pool, _token) = setup().await;
    let (_cookie, bearer) = register_and_login(&router).await;

    let req = Request::builder()
        .method("GET")
        .uri("/api/users/me")
        .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
        .body(Body::empty())
        .unwrap();
    let res = router.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = json_body(res).await;
    assert_eq!(body["username"], "admin");
}

#[tokio::test]
async fn me_without_credentials_returns_401() {
    let (router, _pool, _token) = setup().await;
    let res = router.oneshot(get("/api/users/me")).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn expired_token_returns_token_expired() {
    let (router, pool, _token) = setup().await;
    let (_cookie, _bearer) = register_and_login(&router).await;

    // Hand-roll an expired token for the freshly registered user.
    let (id_str,): (String,) = sqlx::query_as("SELECT id FROM users LIMIT 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    let user_id: Uuid = id_str.parse().unwrap();
    let now = chrono::Utc::now().timestamp();
    let claims = Claims {
        sub: user_id,
        iat: now - 7200,
        exp: now - 3600,
        jti: Uuid::now_v7(),
        ver: 0,
    };
    let expired = encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(TEST_SECRET),
    )
    .unwrap();

    let req = Request::builder()
        .method("GET")
        .uri("/api/users/me")
        .header(header::AUTHORIZATION, format!("Bearer {expired}"))
        .body(Body::empty())
        .unwrap();
    let res = router.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    let body = json_body(res).await;
    assert_eq!(body, json!({ "error": "token_expired" }));
}

#[tokio::test]
async fn logout_bumps_token_version_and_invalidates_old_tokens() {
    let (router, _pool, _token) = setup().await;
    let (cookie, bearer) = register_and_login(&router).await;

    // Sanity: bearer works.
    let req = Request::builder()
        .method("GET")
        .uri("/api/users/me")
        .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        router.clone().oneshot(req).await.unwrap().status(),
        StatusCode::OK
    );

    // Logout (using cookie credential).
    let req = Request::builder()
        .method("POST")
        .uri("/api/auth/logout")
        .header(header::COOKIE, cookie.clone())
        .body(Body::empty())
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    let clear = extract_set_cookie(&res).expect("logout clears cookie");
    assert!(clear.contains("mythos_token="));
    assert!(clear.contains("Max-Age=0"));

    // Same bearer is now invalid.
    let req = Request::builder()
        .method("GET")
        .uri("/api/users/me")
        .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
        .body(Body::empty())
        .unwrap();
    let res = router.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}
