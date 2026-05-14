use figment::Figment;
use figment::providers::{Env, Format, Serialized, Toml};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub listen: SocketAddr,
    pub data_dir: PathBuf,
    pub log_filter: String,
    /// Set the `Secure` flag on auth cookies. Defaults to `false` in debug
    /// builds (so plaintext localhost works) and `true` in release builds.
    /// Override in mythos.toml or via `MYTHOS_COOKIE_SECURE` for reverse
    /// proxy setups where TLS terminates upstream.
    #[serde(default = "default_cookie_secure")]
    pub cookie_secure: bool,
    /// Lifetime of issued JWTs, in days.
    #[serde(default = "default_token_ttl_days")]
    pub token_ttl_days: u64,
    /// TMDb v3 API key. None disables metadata enrichment — scans still
    /// index files, just without titles/posters/overviews.
    #[serde(default)]
    pub tmdb_api_key: Option<String>,
}

fn default_cookie_secure() -> bool {
    !cfg!(debug_assertions)
}

fn default_token_ttl_days() -> u64 {
    30
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:8080".parse().unwrap(),
            data_dir: PathBuf::from("./data"),
            log_filter: "info,mythos=debug,sqlx=warn".to_string(),
            cookie_secure: default_cookie_secure(),
            token_ttl_days: default_token_ttl_days(),
            tmdb_api_key: None,
        }
    }
}

impl Config {
    /// Load from (in order of priority):
    ///   1. `MYTHOS_*` environment variables
    ///   2. `MYTHOS_CONFIG` path (TOML), or `./mythos.toml` if it exists
    ///   3. built-in defaults
    pub fn load() -> Result<Self, Box<figment::Error>> {
        let mut figment = Figment::from(Serialized::defaults(Self::default()));

        let path = std::env::var("MYTHOS_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("mythos.toml"));
        if path.exists() {
            figment = figment.merge(Toml::file(&path));
        }

        figment = figment.merge(Env::prefixed("MYTHOS_").split("__"));

        figment.extract().map_err(Box::new)
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("mythos.db")
    }

    pub fn posters_dir(&self) -> PathBuf {
        self.data_dir.join("posters")
    }

    pub fn transcode_dir(&self) -> PathBuf {
        self.data_dir.join("transcode")
    }

    pub fn subtitles_dir(&self) -> PathBuf {
        self.data_dir.join("subtitles")
    }
}
