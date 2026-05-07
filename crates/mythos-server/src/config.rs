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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:8080".parse().unwrap(),
            data_dir: PathBuf::from("./data"),
            log_filter: "info,mythos=debug,sqlx=warn".to_string(),
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
}
