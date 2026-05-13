//! TMDb (The Movie Database) client.
//!
//! Covers the two endpoints the scanner needs:
//! - `GET /3/search/movie?query=...&year=...` → top-result match for a
//!   filename-identified movie.
//! - `GET https://image.tmdb.org/t/p/{size}/{path}` → poster image,
//!   streamed to disk.
//!
//! Rate-limited with a single-permit semaphore + ~250ms min interval
//! (≈ 4 RPS, well below TMDb's free-tier 40-requests-per-10s ceiling).
//! The scanner runs requests sequentially anyway, so the limit exists
//! mostly as a safety net.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tracing::debug;

const DEFAULT_API_BASE: &str = "https://api.themoviedb.org/3";
const DEFAULT_IMAGE_BASE: &str = "https://image.tmdb.org/t/p";
const DEFAULT_POSTER_SIZE: &str = "w500";
const MIN_REQUEST_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Debug, Error)]
pub enum TmdbError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("unexpected status {0}")]
    Status(reqwest::StatusCode),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid api response: {0}")]
    Decode(String),
}

/// Server-wide TMDb configuration. Cloned into the scanner per scan run.
#[derive(Debug, Clone)]
pub struct TmdbConfig {
    pub api_key: String,
    /// Base URL for the JSON API. Override only for testing.
    pub api_base: String,
    /// Base URL for poster images. Override only for testing.
    pub image_base: String,
    /// Poster size token, e.g. `w500`, `w342`, `original`.
    pub poster_size: String,
    /// Directory under which `{movie_id}.jpg` poster files are written.
    pub posters_dir: PathBuf,
}

impl TmdbConfig {
    pub fn new(api_key: impl Into<String>, posters_dir: impl Into<PathBuf>) -> Self {
        Self {
            api_key: api_key.into(),
            api_base: DEFAULT_API_BASE.to_string(),
            image_base: DEFAULT_IMAGE_BASE.to_string(),
            poster_size: DEFAULT_POSTER_SIZE.to_string(),
            posters_dir: posters_dir.into(),
        }
    }
}

/// Top-result match from a TMDb search, normalized to the fields the
/// scanner persists.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TmdbMatch {
    pub tmdb_id: i64,
    pub title: String,
    pub overview: Option<String>,
    pub release_year: Option<i64>,
    pub poster_path: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TmdbClient {
    cfg: TmdbConfig,
    http: reqwest::Client,
    rate: Arc<Mutex<Instant>>,
}

impl TmdbClient {
    pub fn new(cfg: TmdbConfig) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(concat!("mythos/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(15))
            .build()
            .expect("reqwest client construction");
        // Seed the rate gate so the first request runs without waiting.
        let rate = Arc::new(Mutex::new(
            Instant::now()
                .checked_sub(MIN_REQUEST_INTERVAL)
                .unwrap_or_else(Instant::now),
        ));
        Self { cfg, http, rate }
    }

    pub fn config(&self) -> &TmdbConfig {
        &self.cfg
    }

    async fn wait_for_slot(&self) {
        let mut last = self.rate.lock().await;
        let elapsed = last.elapsed();
        if elapsed < MIN_REQUEST_INTERVAL {
            tokio::time::sleep(MIN_REQUEST_INTERVAL - elapsed).await;
        }
        *last = Instant::now();
    }

    /// Search TMDb for a movie by title (+ optional year hint) and
    /// return the top result, normalized. `Ok(None)` means TMDb has no
    /// hits — not an error.
    pub async fn search_movie(
        &self,
        title: &str,
        year: Option<i64>,
    ) -> Result<Option<TmdbMatch>, TmdbError> {
        self.wait_for_slot().await;

        let mut params: Vec<(&str, String)> = vec![
            ("api_key", self.cfg.api_key.clone()),
            ("query", title.to_string()),
            ("include_adult", "false".to_string()),
            ("language", "en-US".to_string()),
        ];
        if let Some(y) = year {
            params.push(("year", y.to_string()));
        }

        let url = format!("{}/search/movie", self.cfg.api_base);
        let res = self.http.get(&url).query(&params).send().await?;
        let status = res.status();
        if !status.is_success() {
            return Err(TmdbError::Status(status));
        }

        let body: SearchResponse = res.json().await?;
        let Some(first) = body.results.into_iter().next() else {
            debug!(title, year, "tmdb search returned no results");
            return Ok(None);
        };

        let release_year = first
            .release_date
            .as_deref()
            .and_then(|s| s.get(..4))
            .and_then(|y| y.parse::<i64>().ok());

        Ok(Some(TmdbMatch {
            tmdb_id: first.id,
            title: first.title,
            overview: first.overview.filter(|s| !s.is_empty()),
            release_year,
            poster_path: first.poster_path,
        }))
    }

    /// Download the poster image associated with `poster_path` to
    /// `{posters_dir}/{movie_id}.jpg`. Streams the body to disk rather
    /// than buffering. Returns the relative file name (e.g.
    /// `"abc123.jpg"`); callers prepend the API mount point.
    pub async fn download_poster(
        &self,
        poster_path: &str,
        movie_id: &str,
    ) -> Result<String, TmdbError> {
        self.wait_for_slot().await;

        let url = format!(
            "{}/{}{}",
            self.cfg.image_base, self.cfg.poster_size, poster_path
        );
        let res = self.http.get(&url).send().await?;
        let status = res.status();
        if !status.is_success() {
            return Err(TmdbError::Status(status));
        }

        let filename = format!("{movie_id}.jpg");
        let dest = self.cfg.posters_dir.join(&filename);
        // Make sure the parent exists. mythos-server creates this on
        // startup too, but make download_poster safe to call standalone.
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let tmp = dest.with_extension("jpg.tmp");
        let mut file = tokio::fs::File::create(&tmp).await?;
        let mut stream = res.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let bytes = chunk?;
            file.write_all(&bytes).await?;
        }
        file.flush().await?;
        drop(file);
        tokio::fs::rename(&tmp, &dest).await?;

        Ok(filename)
    }
}

pub fn poster_file_path(posters_dir: &Path, movie_id: &str) -> PathBuf {
    posters_dir.join(format!("{movie_id}.jpg"))
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    #[serde(default)]
    results: Vec<SearchResult>,
}

#[derive(Debug, Deserialize)]
struct SearchResult {
    id: i64,
    title: String,
    #[serde(default)]
    overview: Option<String>,
    #[serde(default)]
    release_date: Option<String>,
    #[serde(default)]
    poster_path: Option<String>,
}
