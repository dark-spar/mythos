//! TMDb (The Movie Database) client.
//!
//! Covers the endpoints the scanner needs for movies and TV:
//! - `GET /3/search/movie?query=...&year=...`
//! - `GET /3/search/tv?query=...&first_air_date_year=...`
//! - `GET /3/tv/{id}/season/{n}` (returns season + per-episode metadata
//!   in one round-trip)
//! - `GET https://image.tmdb.org/t/p/{size}/{path}` for poster and
//!   episode-still images, streamed to disk.
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

/// Top-result match from a TMDb TV search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TmdbTvMatch {
    pub tmdb_id: i64,
    pub name: String,
    pub overview: Option<String>,
    pub first_air_year: Option<i64>,
    pub poster_path: Option<String>,
}

/// One season's worth of metadata plus the episodes it contains. TMDb
/// returns the season and all its episodes in a single response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TmdbSeason {
    pub tmdb_id: i64,
    pub season_number: i64,
    pub name: Option<String>,
    pub overview: Option<String>,
    pub poster_path: Option<String>,
    pub episodes: Vec<TmdbEpisode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TmdbEpisode {
    pub tmdb_id: i64,
    pub episode_number: i64,
    pub name: Option<String>,
    pub overview: Option<String>,
    pub still_path: Option<String>,
    pub air_date: Option<String>,
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
            ("query", title.to_string()),
            ("include_adult", "false".to_string()),
            ("language", "en-US".to_string()),
        ];
        if let Some(y) = year {
            params.push(("year", y.to_string()));
        }

        let url = format!("{}/search/movie", self.cfg.api_base);
        let mut req = self.http.get(&url).query(&params);
        // TMDb v4 read-access tokens are JWTs (header.payload.signature)
        // and must be sent as `Authorization: Bearer <token>`. v3 API
        // keys are 32-char hex and go in the `api_key` query param.
        // Detecting on the presence of a dot keeps the contract simple
        // — neither key shape ever contains a literal dot.
        if is_v4_token(&self.cfg.api_key) {
            req = req.bearer_auth(&self.cfg.api_key);
        } else {
            req = req.query(&[("api_key", &self.cfg.api_key)]);
        }
        let res = req.send().await?;
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
    /// `{posters_dir}/{item_id}.jpg`. Streams the body to disk rather
    /// than buffering. Returns the relative file name (e.g.
    /// `"abc123.jpg"`); callers prepend the API mount point.
    ///
    /// `item_id` is the local UUID the API serves the poster under,
    /// usually a movie id or series id. Different item kinds live in
    /// the same directory because their UUIDs don't collide.
    pub async fn download_poster(
        &self,
        poster_path: &str,
        item_id: &str,
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

        let filename = format!("{item_id}.jpg");
        let dest = self.cfg.posters_dir.join(&filename);
        write_streamed_to_disk(res, &dest).await?;
        Ok(filename)
    }

    /// Search TMDb TV by title (+ optional first-air-year hint).
    pub async fn search_tv(
        &self,
        title: &str,
        year: Option<i64>,
    ) -> Result<Option<TmdbTvMatch>, TmdbError> {
        self.wait_for_slot().await;

        let mut params: Vec<(&str, String)> = vec![
            ("query", title.to_string()),
            ("include_adult", "false".to_string()),
            ("language", "en-US".to_string()),
        ];
        if let Some(y) = year {
            params.push(("first_air_date_year", y.to_string()));
        }

        let url = format!("{}/search/tv", self.cfg.api_base);
        let mut req = self.http.get(&url).query(&params);
        if is_v4_token(&self.cfg.api_key) {
            req = req.bearer_auth(&self.cfg.api_key);
        } else {
            req = req.query(&[("api_key", &self.cfg.api_key)]);
        }
        let res = req.send().await?;
        let status = res.status();
        if !status.is_success() {
            return Err(TmdbError::Status(status));
        }

        let body: TvSearchResponse = res.json().await?;
        let Some(first) = body.results.into_iter().next() else {
            debug!(title, year, "tmdb tv search returned no results");
            return Ok(None);
        };

        let first_air_year = first
            .first_air_date
            .as_deref()
            .and_then(|s| s.get(..4))
            .and_then(|y| y.parse::<i64>().ok());

        Ok(Some(TmdbTvMatch {
            tmdb_id: first.id,
            name: first.name,
            overview: first.overview.filter(|s| !s.is_empty()),
            first_air_year,
            poster_path: first.poster_path,
        }))
    }

    /// Fetch a single season + its episode list. TMDb returns both in
    /// one response, which is why the scanner enriches a whole season
    /// of episodes per HTTP call instead of one-by-one.
    pub async fn get_tv_season(
        &self,
        tv_id: i64,
        season_number: i64,
    ) -> Result<TmdbSeason, TmdbError> {
        self.wait_for_slot().await;

        let url = format!(
            "{}/tv/{}/season/{}",
            self.cfg.api_base, tv_id, season_number
        );
        let mut req = self.http.get(&url).query(&[("language", "en-US")]);
        if is_v4_token(&self.cfg.api_key) {
            req = req.bearer_auth(&self.cfg.api_key);
        } else {
            req = req.query(&[("api_key", &self.cfg.api_key)]);
        }
        let res = req.send().await?;
        let status = res.status();
        if !status.is_success() {
            return Err(TmdbError::Status(status));
        }

        let body: SeasonResponse = res.json().await?;
        let episodes = body
            .episodes
            .into_iter()
            .map(|e| TmdbEpisode {
                tmdb_id: e.id,
                episode_number: e.episode_number,
                name: e.name.filter(|s| !s.is_empty()),
                overview: e.overview.filter(|s| !s.is_empty()),
                still_path: e.still_path,
                air_date: e.air_date,
            })
            .collect();

        Ok(TmdbSeason {
            tmdb_id: body.id,
            season_number: body.season_number,
            name: body.name.filter(|s| !s.is_empty()),
            overview: body.overview.filter(|s| !s.is_empty()),
            poster_path: body.poster_path,
            episodes,
        })
    }

    /// Download an episode still to `{posters_dir}/stills/{episode_id}.jpg`.
    /// Stills are 16:9 thumbnails so they live in their own subdirectory
    /// to keep them sorted away from 2:3 posters.
    pub async fn download_still(
        &self,
        still_path: &str,
        episode_id: &str,
    ) -> Result<String, TmdbError> {
        self.wait_for_slot().await;

        let url = format!(
            "{}/{}{}",
            self.cfg.image_base, self.cfg.poster_size, still_path
        );
        let res = self.http.get(&url).send().await?;
        let status = res.status();
        if !status.is_success() {
            return Err(TmdbError::Status(status));
        }

        let filename = format!("{episode_id}.jpg");
        let dest = self.cfg.posters_dir.join("stills").join(&filename);
        write_streamed_to_disk(res, &dest).await?;
        Ok(filename)
    }
}

/// Stream an HTTP response body to a file via a `.tmp` sibling, then
/// rename atomically. Creates parent directories as needed.
async fn write_streamed_to_disk(res: reqwest::Response, dest: &Path) -> Result<(), TmdbError> {
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
    tokio::fs::rename(&tmp, dest).await?;
    Ok(())
}

pub fn poster_file_path(posters_dir: &Path, movie_id: &str) -> PathBuf {
    posters_dir.join(format!("{movie_id}.jpg"))
}

/// True if `key` looks like a TMDb v4 access token (a JWT with three
/// dot-separated parts). v3 API keys are 32-character hex with no
/// dots, so this is a reliable discriminator.
fn is_v4_token(key: &str) -> bool {
    key.contains('.')
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

#[derive(Debug, Deserialize)]
struct TvSearchResponse {
    #[serde(default)]
    results: Vec<TvSearchResult>,
}

#[derive(Debug, Deserialize)]
struct TvSearchResult {
    id: i64,
    name: String,
    #[serde(default)]
    overview: Option<String>,
    #[serde(default)]
    first_air_date: Option<String>,
    #[serde(default)]
    poster_path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SeasonResponse {
    id: i64,
    season_number: i64,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    overview: Option<String>,
    #[serde(default)]
    poster_path: Option<String>,
    #[serde(default)]
    episodes: Vec<SeasonEpisodeResponse>,
}

#[derive(Debug, Deserialize)]
struct SeasonEpisodeResponse {
    id: i64,
    episode_number: i64,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    overview: Option<String>,
    #[serde(default)]
    still_path: Option<String>,
    #[serde(default)]
    air_date: Option<String>,
}
