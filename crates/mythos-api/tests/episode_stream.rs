//! Integration tests for `/api/episodes/:id/stream`,
//! `/api/episodes/:id/progress`, and the play-decision /
//! prev-next neighbor wiring on episode detail.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use mythos_api::{
    ApiState, CookieConfig, HlsHandle, PostersDir, ScanTracker, SubtitlesDir, TmdbHandle,
};
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

fn auth_put(uri: &str, bearer: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method("PUT")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
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

/// One series with one season and a single episode pointing at a real
/// on-disk file. The TempDir guard must outlive the test.
struct EpisodeFixture {
    episode_id: Uuid,
    _series_id: Uuid,
    _season_id: Uuid,
    _file_id: Uuid,
    _dir: TempDir,
}

async fn create_episode_on_disk(pool: &SqlitePool, content: &[u8]) -> EpisodeFixture {
    let dir = TempDir::new().unwrap();
    let filename = "Show.S01E01.mp4";
    fs::write(dir.path().join(filename), content).unwrap();

    let library_id = Uuid::now_v7();
    sqlx::query("INSERT INTO libraries (id, name, kind, root_path) VALUES (?, 'test', 'shows', ?)")
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

    let series_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO series (id, library_id, title, sort_title) VALUES (?, ?, 'Show', 'Show')",
    )
    .bind(series_id.to_string())
    .bind(library_id.to_string())
    .execute(pool)
    .await
    .unwrap();

    let season_id = Uuid::now_v7();
    sqlx::query("INSERT INTO seasons (id, series_id, season_number) VALUES (?, ?, 1)")
        .bind(season_id.to_string())
        .bind(series_id.to_string())
        .execute(pool)
        .await
        .unwrap();

    let episode_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO episodes (id, season_id, file_id, episode_number) VALUES (?, ?, ?, 1)",
    )
    .bind(episode_id.to_string())
    .bind(season_id.to_string())
    .bind(file_id.to_string())
    .execute(pool)
    .await
    .unwrap();

    EpisodeFixture {
        episode_id,
        _series_id: series_id,
        _season_id: season_id,
        _file_id: file_id,
        _dir: dir,
    }
}

/// Insert a second episode in the same season as `previous`. Both
/// episodes share series + season; the caller picks the
/// `episode_number`.
async fn add_neighbor_episode(
    pool: &SqlitePool,
    previous_episode_id: Uuid,
    episode_number: i64,
) -> Uuid {
    let row: (String,) = sqlx::query_as("SELECT season_id FROM episodes WHERE id = ?")
        .bind(previous_episode_id.to_string())
        .fetch_one(pool)
        .await
        .unwrap();
    let season_id = row.0;

    let lib_row: (String,) = sqlx::query_as(
        "SELECT s.series_id FROM seasons s WHERE s.id = ? \
         LIMIT 1",
    )
    .bind(&season_id)
    .fetch_one(pool)
    .await
    .unwrap();
    let _series_id = lib_row.0;

    let file_row: (String,) = sqlx::query_as(
        "SELECT library_id FROM media_files \
         WHERE id = (SELECT file_id FROM episodes WHERE id = ?)",
    )
    .bind(previous_episode_id.to_string())
    .fetch_one(pool)
    .await
    .unwrap();
    let library_id = file_row.0;

    let file_id = Uuid::now_v7();
    let filename = format!("Show.S01E{episode_number:02}.mp4");
    sqlx::query(
        "INSERT INTO media_files (id, library_id, path, size_bytes, mtime) \
         VALUES (?, ?, ?, 100, '2026-01-01T00:00:00.000Z')",
    )
    .bind(file_id.to_string())
    .bind(&library_id)
    .bind(&filename)
    .execute(pool)
    .await
    .unwrap();

    let episode_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO episodes (id, season_id, file_id, episode_number) VALUES (?, ?, ?, ?)",
    )
    .bind(episode_id.to_string())
    .bind(&season_id)
    .bind(file_id.to_string())
    .bind(episode_number)
    .execute(pool)
    .await
    .unwrap();

    episode_id
}

fn content_1k() -> Vec<u8> {
    (0..1000u16).map(|i| (i & 0xff) as u8).collect()
}

#[tokio::test]
async fn stream_requires_auth() {
    let (router, pool) = setup().await;
    let ep = create_episode_on_disk(&pool, &content_1k()).await;

    let res = router
        .oneshot(anon_get(&format!("/api/episodes/{}/stream", ep.episode_id)))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn stream_unknown_episode_returns_404() {
    let (router, _pool) = setup().await;
    let bearer = admin_bearer(&router).await;
    let missing = Uuid::now_v7();

    let res = router
        .oneshot(auth_get(
            &format!("/api/episodes/{missing}/stream"),
            &bearer,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn stream_file_gone_from_disk_returns_404() {
    let (router, pool) = setup().await;
    let bearer = admin_bearer(&router).await;
    let ep = create_episode_on_disk(&pool, &content_1k()).await;

    fs::remove_file(PathBuf::from(ep._dir.path()).join("Show.S01E01.mp4")).unwrap();

    let res = router
        .oneshot(auth_get(
            &format!("/api/episodes/{}/stream", ep.episode_id),
            &bearer,
        ))
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
    let ep = create_episode_on_disk(&pool, &content).await;

    let res = router
        .oneshot(auth_get(
            &format!("/api/episodes/{}/stream", ep.episode_id),
            &bearer,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok()),
        Some(content.len().to_string().as_str())
    );
    let body = body_bytes(res).await;
    assert_eq!(body, content);
}

#[tokio::test]
async fn stream_returns_partial_content_for_byte_range() {
    let (router, pool) = setup().await;
    let bearer = admin_bearer(&router).await;
    let content = content_1k();
    let ep = create_episode_on_disk(&pool, &content).await;

    let res = router
        .oneshot(auth_get_with_range(
            &format!("/api/episodes/{}/stream", ep.episode_id),
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
    let body = body_bytes(res).await;
    assert_eq!(body, &content[100..200]);
}

#[tokio::test]
async fn progress_round_trip_via_episode_detail() {
    let (router, pool) = setup().await;
    let bearer = admin_bearer(&router).await;
    let ep = create_episode_on_disk(&pool, &content_1k()).await;

    let res = router
        .clone()
        .oneshot(auth_get(
            &format!("/api/episodes/{}", ep.episode_id),
            &bearer,
        ))
        .await
        .unwrap();
    let body = body_json(res).await;
    assert!(body["progress"].is_null(), "got {body}");

    let res = router
        .clone()
        .oneshot(auth_put(
            &format!("/api/episodes/{}/progress", ep.episode_id),
            &bearer,
            json!({ "position_seconds": 77.0, "duration_seconds": 1200.0 }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);

    let res = router
        .oneshot(auth_get(
            &format!("/api/episodes/{}", ep.episode_id),
            &bearer,
        ))
        .await
        .unwrap();
    let body = body_json(res).await;
    assert_eq!(body["progress"]["position_seconds"], 77.0);
    assert_eq!(body["progress"]["duration_seconds"], 1200.0);
}

#[tokio::test]
async fn progress_is_per_user() {
    let (router, pool) = setup().await;
    let _ = admin_bearer(&router).await;
    let alice = bearer_for(insert_user(&pool, "alice").await);
    let bob = bearer_for(insert_user(&pool, "bob").await);
    let ep = create_episode_on_disk(&pool, &content_1k()).await;

    router
        .clone()
        .oneshot(auth_put(
            &format!("/api/episodes/{}/progress", ep.episode_id),
            &alice,
            json!({ "position_seconds": 30.0, "duration_seconds": 1200.0 }),
        ))
        .await
        .unwrap();
    router
        .clone()
        .oneshot(auth_put(
            &format!("/api/episodes/{}/progress", ep.episode_id),
            &bob,
            json!({ "position_seconds": 90.0, "duration_seconds": 1200.0 }),
        ))
        .await
        .unwrap();

    let alice_detail = body_json(
        router
            .clone()
            .oneshot(auth_get(
                &format!("/api/episodes/{}", ep.episode_id),
                &alice,
            ))
            .await
            .unwrap(),
    )
    .await;
    let bob_detail = body_json(
        router
            .oneshot(auth_get(&format!("/api/episodes/{}", ep.episode_id), &bob))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(alice_detail["progress"]["position_seconds"], 30.0);
    assert_eq!(bob_detail["progress"]["position_seconds"], 90.0);
}

#[tokio::test]
async fn progress_rejects_invalid_payload() {
    let (router, pool) = setup().await;
    let bearer = admin_bearer(&router).await;
    let ep = create_episode_on_disk(&pool, &content_1k()).await;

    let res = router
        .clone()
        .oneshot(auth_put(
            &format!("/api/episodes/{}/progress", ep.episode_id),
            &bearer,
            json!({ "position_seconds": -1.0, "duration_seconds": 600.0 }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let res = router
        .oneshot(auth_put(
            &format!("/api/episodes/{}/progress", ep.episode_id),
            &bearer,
            json!({ "position_seconds": 0.0, "duration_seconds": 0.0 }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn progress_unknown_episode_returns_404() {
    let (router, _pool) = setup().await;
    let bearer = admin_bearer(&router).await;
    let missing = Uuid::now_v7();

    let res = router
        .oneshot(auth_put(
            &format!("/api/episodes/{missing}/progress"),
            &bearer,
            json!({ "position_seconds": 1.0, "duration_seconds": 60.0 }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn episode_detail_includes_prev_and_next() {
    let (router, pool) = setup().await;
    let bearer = admin_bearer(&router).await;
    let ep1 = create_episode_on_disk(&pool, &content_1k()).await;
    let ep2 = add_neighbor_episode(&pool, ep1.episode_id, 2).await;
    let ep3 = add_neighbor_episode(&pool, ep1.episode_id, 3).await;

    // Episode 2 should have ep1 as prev, ep3 as next.
    let body = body_json(
        router
            .oneshot(auth_get(&format!("/api/episodes/{ep2}"), &bearer))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(body["prev"]["id"], ep1.episode_id.to_string());
    assert_eq!(body["next"]["id"], ep3.to_string());
}

#[tokio::test]
async fn play_endpoint_returns_episode_prefixed_stream_url() {
    let (router, pool) = setup().await;
    let bearer = admin_bearer(&router).await;
    let ep = create_episode_on_disk(&pool, &content_1k()).await;

    // Stuff some probe metadata so the play decision can run.
    sqlx::query(
        "UPDATE media_files SET container='mp4', video_codec='h264', audio_codec='aac', \
         duration_seconds=600.0, width=1280, height=720 \
         WHERE id = (SELECT file_id FROM episodes WHERE id = ?)",
    )
    .bind(ep.episode_id.to_string())
    .execute(&pool)
    .await
    .unwrap();

    // Generous client profile so the decision is direct-play.
    let profile = json!({
        "containers": ["mp4"],
        "video_codecs": [{ "codec": "h264", "profile": null, "level": null }],
        "audio_codecs": [{ "codec": "aac", "max_channels": null }],
        "max_width": null,
        "max_height": null,
        "max_audio_channels": null
    });

    let res = router
        .oneshot(auth_post(
            &format!("/api/episodes/{}/play", ep.episode_id),
            &bearer,
            profile,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_json(res).await;
    assert_eq!(body["mode"], "direct_play");
    let url = body["stream_url"].as_str().unwrap();
    assert!(
        url.starts_with(&format!("/api/episodes/{}/", ep.episode_id)),
        "expected episode-prefixed URL, got {url}"
    );
}
