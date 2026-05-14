//! Integration tests for `GET /api/users/me/continue-watching`.

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
    let bytes = res.into_body().collect().await.unwrap().to_bytes().to_vec();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

async fn admin_bearer(router: &Router) -> (String, Uuid) {
    let res = router
        .clone()
        .oneshot(json_post(
            "/api/auth/register",
            json!({ "username": "admin", "password": "hunter2hunter2" }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_json(res).await;
    let bearer = body["token"].as_str().unwrap().to_string();
    let user_id = Uuid::parse_str(body["user"]["id"].as_str().unwrap()).unwrap();
    (bearer, user_id)
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

async fn create_movie_library(pool: &SqlitePool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO libraries (id, name, kind, root_path) VALUES (?, 'movies-lib', 'movies', ?)",
    )
    .bind(id.to_string())
    .bind(format!("/tmp/{id}"))
    .execute(pool)
    .await
    .unwrap();
    id
}

async fn create_shows_library(pool: &SqlitePool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO libraries (id, name, kind, root_path) VALUES (?, 'shows-lib', 'shows', ?)",
    )
    .bind(id.to_string())
    .bind(format!("/tmp/{id}"))
    .execute(pool)
    .await
    .unwrap();
    id
}

async fn insert_media_file(pool: &SqlitePool, library_id: Uuid) -> Uuid {
    let file_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO media_files (id, library_id, path, size_bytes, mtime) \
         VALUES (?, ?, ?, 100, '2026-01-01T00:00:00.000Z')",
    )
    .bind(file_id.to_string())
    .bind(library_id.to_string())
    .bind(format!("file-{file_id}.mkv"))
    .execute(pool)
    .await
    .unwrap();
    file_id
}

async fn insert_movie(pool: &SqlitePool, library_id: Uuid, title: &str, year: Option<i64>) -> Uuid {
    let file_id = insert_media_file(pool, library_id).await;
    let movie_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO movies (id, library_id, file_id, title, sort_title, year, poster_url) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(movie_id.to_string())
    .bind(library_id.to_string())
    .bind(file_id.to_string())
    .bind(title)
    .bind(title)
    .bind(year)
    .bind(format!("/api/movies/{movie_id}/poster"))
    .execute(pool)
    .await
    .unwrap();
    movie_id
}

async fn insert_series(pool: &SqlitePool, library_id: Uuid, title: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO series (id, library_id, title, sort_title, poster_url) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(id.to_string())
    .bind(library_id.to_string())
    .bind(title)
    .bind(title)
    .bind(format!("/api/series/{id}/poster"))
    .execute(pool)
    .await
    .unwrap();
    id
}

async fn insert_season(pool: &SqlitePool, series_id: Uuid, season_number: i64) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO seasons (id, series_id, season_number) VALUES (?, ?, ?)")
        .bind(id.to_string())
        .bind(series_id.to_string())
        .bind(season_number)
        .execute(pool)
        .await
        .unwrap();
    id
}

async fn insert_episode(
    pool: &SqlitePool,
    season_id: Uuid,
    library_id: Uuid,
    episode_number: i64,
    title: Option<&str>,
) -> Uuid {
    let file_id = insert_media_file(pool, library_id).await;
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO episodes (id, season_id, file_id, episode_number, title, still_url) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(id.to_string())
    .bind(season_id.to_string())
    .bind(file_id.to_string())
    .bind(episode_number)
    .bind(title)
    .bind(format!("/api/episodes/{id}/still"))
    .execute(pool)
    .await
    .unwrap();
    id
}

async fn upsert_movie_progress(
    pool: &SqlitePool,
    user_id: Uuid,
    movie_id: Uuid,
    position: f64,
    duration: f64,
    updated_at: &str,
) {
    sqlx::query(
        "INSERT INTO movie_progress \
           (user_id, movie_id, position_seconds, duration_seconds, updated_at) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(user_id.to_string())
    .bind(movie_id.to_string())
    .bind(position)
    .bind(duration)
    .bind(updated_at)
    .execute(pool)
    .await
    .unwrap();
}

async fn upsert_episode_progress(
    pool: &SqlitePool,
    user_id: Uuid,
    episode_id: Uuid,
    position: f64,
    duration: f64,
    updated_at: &str,
) {
    sqlx::query(
        "INSERT INTO episode_progress \
           (user_id, episode_id, position_seconds, duration_seconds, updated_at) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(user_id.to_string())
    .bind(episode_id.to_string())
    .bind(position)
    .bind(duration)
    .bind(updated_at)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn requires_auth() {
    let (router, _pool) = setup().await;
    let res = router
        .oneshot(anon_get("/api/users/me/continue-watching"))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn empty_when_no_progress() {
    let (router, _pool) = setup().await;
    let (bearer, _) = admin_bearer(&router).await;

    let res = router
        .oneshot(auth_get("/api/users/me/continue-watching", &bearer))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(body_json(res).await, json!([]));
}

#[tokio::test]
async fn surfaces_in_progress_movie() {
    let (router, pool) = setup().await;
    let (bearer, user_id) = admin_bearer(&router).await;
    let lib = create_movie_library(&pool).await;
    let movie = insert_movie(&pool, lib, "Inception", Some(2010)).await;
    upsert_movie_progress(
        &pool,
        user_id,
        movie,
        600.0,
        7200.0,
        "2026-05-14T10:00:00.000Z",
    )
    .await;

    let res = router
        .oneshot(auth_get("/api/users/me/continue-watching", &bearer))
        .await
        .unwrap();
    let body = body_json(res).await;
    assert_eq!(body.as_array().unwrap().len(), 1);
    assert_eq!(body[0]["kind"], "movie");
    assert_eq!(body[0]["id"], movie.to_string());
    assert_eq!(body[0]["title"], "Inception");
    assert_eq!(body[0]["year"], 2010);
    assert_eq!(body[0]["position_seconds"], 600.0);
    assert_eq!(body[0]["duration_seconds"], 7200.0);
}

#[tokio::test]
async fn surfaces_in_progress_episode_with_series_metadata() {
    let (router, pool) = setup().await;
    let (bearer, user_id) = admin_bearer(&router).await;
    let lib = create_shows_library(&pool).await;
    let series = insert_series(&pool, lib, "Severance").await;
    let season = insert_season(&pool, series, 1).await;
    let episode = insert_episode(&pool, season, lib, 3, Some("In Perpetuity")).await;
    upsert_episode_progress(
        &pool,
        user_id,
        episode,
        540.0,
        3300.0,
        "2026-05-14T11:00:00.000Z",
    )
    .await;

    let res = router
        .oneshot(auth_get("/api/users/me/continue-watching", &bearer))
        .await
        .unwrap();
    let body = body_json(res).await;
    assert_eq!(body.as_array().unwrap().len(), 1);
    let item = &body[0];
    assert_eq!(item["kind"], "episode");
    assert_eq!(item["id"], episode.to_string());
    assert_eq!(item["series_id"], series.to_string());
    assert_eq!(item["series_title"], "Severance");
    assert_eq!(item["season_number"], 1);
    assert_eq!(item["episode_number"], 3);
    assert_eq!(item["episode_title"], "In Perpetuity");
    assert!(
        item["poster_url"]
            .as_str()
            .unwrap()
            .contains("/api/series/")
    );
    assert!(
        item["still_url"]
            .as_str()
            .unwrap()
            .contains("/api/episodes/")
    );
}

#[tokio::test]
async fn merges_kinds_in_recency_order() {
    let (router, pool) = setup().await;
    let (bearer, user_id) = admin_bearer(&router).await;
    let movies_lib = create_movie_library(&pool).await;
    let shows_lib = create_shows_library(&pool).await;
    let movie_a = insert_movie(&pool, movies_lib, "Movie A", None).await;
    let movie_b = insert_movie(&pool, movies_lib, "Movie B", None).await;
    let series = insert_series(&pool, shows_lib, "Show").await;
    let season = insert_season(&pool, series, 1).await;
    let ep1 = insert_episode(&pool, season, shows_lib, 1, None).await;
    let ep2 = insert_episode(&pool, season, shows_lib, 2, None).await;

    // Chronological order: movie_a oldest → ep2 newest.
    upsert_movie_progress(
        &pool,
        user_id,
        movie_a,
        100.0,
        1000.0,
        "2026-05-14T09:00:00.000Z",
    )
    .await;
    upsert_episode_progress(
        &pool,
        user_id,
        ep1,
        100.0,
        1000.0,
        "2026-05-14T10:00:00.000Z",
    )
    .await;
    upsert_movie_progress(
        &pool,
        user_id,
        movie_b,
        100.0,
        1000.0,
        "2026-05-14T11:00:00.000Z",
    )
    .await;
    upsert_episode_progress(
        &pool,
        user_id,
        ep2,
        100.0,
        1000.0,
        "2026-05-14T12:00:00.000Z",
    )
    .await;

    let res = router
        .oneshot(auth_get("/api/users/me/continue-watching", &bearer))
        .await
        .unwrap();
    let body = body_json(res).await;
    let ids: Vec<&str> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    let expected = [
        ep2.to_string(),
        movie_b.to_string(),
        ep1.to_string(),
        movie_a.to_string(),
    ];
    let expected: Vec<&str> = expected.iter().map(String::as_str).collect();
    assert_eq!(ids, expected);
}

#[tokio::test]
async fn drops_items_below_min_position() {
    let (router, pool) = setup().await;
    let (bearer, user_id) = admin_bearer(&router).await;
    let lib = create_movie_library(&pool).await;
    let movie = insert_movie(&pool, lib, "Brief Click", None).await;
    // 30 seconds < 60s threshold → excluded.
    upsert_movie_progress(
        &pool,
        user_id,
        movie,
        30.0,
        7200.0,
        "2026-05-14T10:00:00.000Z",
    )
    .await;

    let res = router
        .oneshot(auth_get("/api/users/me/continue-watching", &bearer))
        .await
        .unwrap();
    assert_eq!(body_json(res).await, json!([]));
}

#[tokio::test]
async fn drops_items_past_watched_fraction() {
    let (router, pool) = setup().await;
    let (bearer, user_id) = admin_bearer(&router).await;
    let lib = create_movie_library(&pool).await;
    let movie = insert_movie(&pool, lib, "End Credits", None).await;
    // 98% of 1000s → past the 0.95 watched cutoff.
    upsert_movie_progress(
        &pool,
        user_id,
        movie,
        980.0,
        1000.0,
        "2026-05-14T10:00:00.000Z",
    )
    .await;

    let res = router
        .oneshot(auth_get("/api/users/me/continue-watching", &bearer))
        .await
        .unwrap();
    assert_eq!(body_json(res).await, json!([]));
}

#[tokio::test]
async fn is_per_user() {
    let (router, pool) = setup().await;
    let _ = admin_bearer(&router).await;
    let alice = insert_user(&pool, "alice").await;
    let bob = insert_user(&pool, "bob").await;
    let alice_bearer = bearer_for(alice);
    let bob_bearer = bearer_for(bob);

    let lib = create_movie_library(&pool).await;
    let movie = insert_movie(&pool, lib, "Shared Movie", None).await;
    upsert_movie_progress(
        &pool,
        alice,
        movie,
        500.0,
        7200.0,
        "2026-05-14T10:00:00.000Z",
    )
    .await;

    let alice_body = body_json(
        router
            .clone()
            .oneshot(auth_get("/api/users/me/continue-watching", &alice_bearer))
            .await
            .unwrap(),
    )
    .await;
    let bob_body = body_json(
        router
            .oneshot(auth_get("/api/users/me/continue-watching", &bob_bearer))
            .await
            .unwrap(),
    )
    .await;

    assert_eq!(alice_body.as_array().unwrap().len(), 1);
    assert_eq!(bob_body.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn limit_caps_results() {
    let (router, pool) = setup().await;
    let (bearer, user_id) = admin_bearer(&router).await;
    let lib = create_movie_library(&pool).await;

    // Five in-progress movies, varying timestamps.
    for i in 0..5 {
        let movie = insert_movie(&pool, lib, &format!("M{i}"), None).await;
        let ts = format!("2026-05-14T10:0{i}:00.000Z");
        upsert_movie_progress(&pool, user_id, movie, 100.0, 1000.0, &ts).await;
    }

    let res = router
        .oneshot(auth_get("/api/users/me/continue-watching?limit=3", &bearer))
        .await
        .unwrap();
    let body = body_json(res).await;
    assert_eq!(body.as_array().unwrap().len(), 3);
}
