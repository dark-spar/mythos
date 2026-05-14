//! Integration tests for the TMDb client against a local axum mock so
//! we don't hit the real TMDb API during cargo test.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Json;
use axum::Router;
use axum::extract::Query;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;
use mythos_meta::{TmdbClient, TmdbConfig};
use serde::Deserialize;
use serde_json::json;
use tempfile::TempDir;
use tokio::net::TcpListener;

#[derive(Clone, Default)]
struct MockState {
    /// Number of /search/movie calls served. Used to verify rate
    /// limiting / idempotency.
    search_calls: Arc<AtomicUsize>,
}

#[derive(Debug, Deserialize)]
struct SearchQuery {
    #[serde(default)]
    api_key: Option<String>,
    query: String,
    #[serde(default)]
    year: Option<i64>,
}

async fn spawn_mock() -> (String, String, MockState) {
    let state = MockState::default();
    let state_for_router = state.clone();

    let app = Router::new()
        .route(
            "/search/movie",
            get(
                |axum::extract::State(s): axum::extract::State<MockState>,
                 headers: axum::http::HeaderMap,
                 Query(q): Query<SearchQuery>| async move {
                    s.search_calls.fetch_add(1, Ordering::SeqCst);
                    let bearer = headers
                        .get(axum::http::header::AUTHORIZATION)
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.strip_prefix("Bearer "));
                    let has_query_key = q.api_key.as_deref().is_some_and(|s| !s.is_empty());
                    assert!(
                        bearer.is_some() || has_query_key,
                        "request must authenticate via Bearer or api_key"
                    );
                    if q.query.eq_ignore_ascii_case("Inception") && q.year == Some(2010) {
                        Json(json!({
                            "results": [{
                                "id": 27205,
                                "title": "Inception",
                                "overview": "A thief who steals corporate secrets...",
                                "release_date": "2010-07-15",
                                "poster_path": "/inception.jpg"
                            }]
                        }))
                        .into_response()
                    } else if q.query.eq_ignore_ascii_case("Inception") {
                        // Same title without year hint also matches.
                        Json(json!({
                            "results": [{
                                "id": 27205,
                                "title": "Inception",
                                "overview": "A thief who steals corporate secrets...",
                                "release_date": "2010-07-15",
                                "poster_path": "/inception.jpg"
                            }]
                        }))
                        .into_response()
                    } else {
                        Json(json!({ "results": [] })).into_response()
                    }
                },
            ),
        )
        .route(
            "/t/p/{size}/{*path}",
            get(|| async { ([(header::CONTENT_TYPE, "image/jpeg")], b"JPEGDATA".to_vec()) }),
        )
        .with_state(state_for_router);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let api_base = format!("http://{addr}");
    let image_base = format!("http://{addr}/t/p");
    (api_base, image_base, state)
}

fn client_for(api_base: String, image_base: String, posters_dir: &std::path::Path) -> TmdbClient {
    TmdbClient::new(TmdbConfig {
        api_key: "fake-key".to_string(),
        api_base,
        image_base,
        poster_size: "w500".to_string(),
        posters_dir: posters_dir.to_path_buf(),
    })
}

#[tokio::test]
async fn v4_jwt_token_authenticates_via_bearer_header() {
    use axum::http::header::AUTHORIZATION;
    use std::sync::atomic::AtomicUsize;

    let saw_bearer = Arc::new(AtomicUsize::new(0));
    let saw_query = Arc::new(AtomicUsize::new(0));
    let bearer_clone = saw_bearer.clone();
    let query_clone = saw_query.clone();

    let app = Router::new().route(
        "/search/movie",
        get(
            move |headers: axum::http::HeaderMap, Query(q): Query<SearchQuery>| {
                let bearer_clone = bearer_clone.clone();
                let query_clone = query_clone.clone();
                async move {
                    if headers
                        .get(AUTHORIZATION)
                        .and_then(|v| v.to_str().ok())
                        .is_some_and(|v| v.starts_with("Bearer "))
                    {
                        bearer_clone.fetch_add(1, Ordering::SeqCst);
                    }
                    if q.api_key.is_some() {
                        query_clone.fetch_add(1, Ordering::SeqCst);
                    }
                    Json(json!({ "results": [] }))
                }
            },
        ),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let dir = TempDir::new().unwrap();
    let client = TmdbClient::new(TmdbConfig {
        api_key: "eyJhbGciOi.JIUzI1NiJ9.PBydlpFlf".to_string(),
        api_base: format!("http://{addr}"),
        image_base: format!("http://{addr}/t/p"),
        poster_size: "w500".to_string(),
        posters_dir: dir.path().to_path_buf(),
    });
    let _ = client.search_movie("anything", None).await;

    assert_eq!(
        saw_bearer.load(Ordering::SeqCst),
        1,
        "v4 JWT token should be sent as Bearer auth"
    );
    assert_eq!(
        saw_query.load(Ordering::SeqCst),
        0,
        "v4 JWT token must not also appear in the api_key query param"
    );
}

#[tokio::test]
async fn search_returns_top_match() {
    let dir = TempDir::new().unwrap();
    let (api, img, _) = spawn_mock().await;
    let client = client_for(api, img, dir.path());

    let m = client
        .search_movie("Inception", Some(2010))
        .await
        .expect("search")
        .expect("a match");
    assert_eq!(m.tmdb_id, 27205);
    assert_eq!(m.title, "Inception");
    assert_eq!(m.release_year, Some(2010));
    assert!(m.overview.as_deref().unwrap().contains("thief"));
    assert_eq!(m.poster_path.as_deref(), Some("/inception.jpg"));
}

#[tokio::test]
async fn search_returns_none_when_tmdb_has_no_results() {
    let dir = TempDir::new().unwrap();
    let (api, img, _) = spawn_mock().await;
    let client = client_for(api, img, dir.path());

    let m = client
        .search_movie("Definitely Not A Real Movie", None)
        .await
        .expect("search");
    assert!(m.is_none());
}

#[tokio::test]
async fn download_poster_writes_file_to_posters_dir() {
    let dir = TempDir::new().unwrap();
    let (api, img, _) = spawn_mock().await;
    let client = client_for(api, img, dir.path());

    let filename = client
        .download_poster("/inception.jpg", "movie-abc")
        .await
        .expect("download");
    assert_eq!(filename, "movie-abc.jpg");

    let written = std::fs::read(dir.path().join("movie-abc.jpg")).unwrap();
    assert_eq!(written, b"JPEGDATA");
}

#[tokio::test]
async fn rate_limiter_delays_subsequent_requests() {
    let dir = TempDir::new().unwrap();
    let (api, img, state) = spawn_mock().await;
    let client = client_for(api, img, dir.path());

    let start = std::time::Instant::now();
    let _ = client.search_movie("Inception", Some(2010)).await.unwrap();
    let _ = client.search_movie("Inception", Some(2010)).await.unwrap();
    let elapsed = start.elapsed();

    assert!(
        elapsed >= std::time::Duration::from_millis(250),
        "second request should have waited the rate-limit interval; elapsed = {elapsed:?}"
    );
    assert_eq!(state.search_calls.load(Ordering::SeqCst), 2);
}
