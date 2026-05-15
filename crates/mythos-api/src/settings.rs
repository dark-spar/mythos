//! `/api/settings` — admin-only configuration the operator can
//! manage from the browser without having to touch env vars or a
//! config file. Today: TMDb API key. Designed so additional fields
//! plug into the same shape later.
//!
//! Env vars always win. The endpoint reports `source = "env"` in
//! that case so the UI can warn the admin that anything they
//! submit here will be ignored until the env var is unset.

use axum::Json;
use axum::extract::State;
use mythos_auth::AdminUser;
use mythos_db::SettingsRepo;
use mythos_db::settings::keys as setting_keys;
use mythos_stream::{TonemapAlgorithm, TonemapPipeline};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::error::ApiResult;
use crate::{HlsHandle, PostersDir, TmdbHandle, build_tmdb_client, resolve_tmdb_api_key};

#[derive(Debug, Serialize)]
pub struct SettingsResponse {
    pub tmdb: TmdbSettings,
    pub tonemap: TonemapSettings,
}

#[derive(Debug, Serialize)]
pub struct TonemapSettings {
    /// Whether HDR→SDR tonemapping is enabled. Defaults to `true`
    /// when nothing is stored — an HDR source served straight to
    /// SDR clients without tonemap looks washed out, and that's
    /// the surprising-default footgun this setting exists to dodge.
    pub enabled: bool,
    /// Lowercase algorithm slug — `hable` / `mobius` / `reinhard` /
    /// `bt2390`. Matches [`mythos_stream::TonemapAlgorithm::as_str`].
    pub algorithm: String,
    /// `hardware` or `software`. Matches
    /// [`mythos_stream::TonemapPipeline::as_str`].
    pub pipeline: String,
    /// `false` when the active ffmpeg build doesn't include the
    /// GPU tonemap filter for the active encoder
    /// (`tonemap_cuda` / `tonemap_vaapi`). The HLS handler
    /// auto-downgrades to the Software pipeline in that case; the
    /// UI uses this to surface "Hardware unavailable — falling
    /// back to Software" rather than letting the operator wonder
    /// why their CPU is still pinned.
    pub hardware_supported: bool,
}

#[derive(Debug, Serialize)]
pub struct TmdbSettings {
    pub configured: bool,
    /// `"env"` when MYTHOS_TMDB_API_KEY is winning the precedence
    /// race, `"db"` when the stored value is active, `"none"`
    /// otherwise. The UI uses this to render a "you can't change
    /// this from here" hint when env-locked.
    pub source: TmdbSource,
    /// The DB-stored value, if any. Returned even when env wins
    /// (so an admin can see what they previously saved). Never
    /// includes the env-var value — that's the operator's
    /// responsibility, not something the UI mirrors back.
    /// Admin-only endpoint, so the exposure is bounded.
    pub value: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TmdbSource {
    Env,
    Db,
    None,
}

#[derive(Debug, Deserialize)]
pub struct SettingsUpdate {
    /// Trimmed before storage. Empty string clears the stored
    /// value.
    pub tmdb_api_key: Option<String>,
    /// HDR→SDR tonemapping master switch. `None` leaves the
    /// existing value untouched.
    pub tonemap_enabled: Option<bool>,
    /// Tonemap operator. Unknown values are coerced to the default
    /// (Hable) on the way in.
    pub tonemap_algorithm: Option<String>,
    /// Tonemap pipeline — `hardware` or `software`. Unknown values
    /// are coerced to `hardware` on the way in.
    pub tonemap_pipeline: Option<String>,
}

pub async fn get_settings(
    State(pool): State<SqlitePool>,
    State(hls): State<HlsHandle>,
    _user: AdminUser,
) -> ApiResult<Json<SettingsResponse>> {
    let env_set = std::env::var("MYTHOS_TMDB_API_KEY")
        .ok()
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false);
    let repo = SettingsRepo::new(pool.clone());
    let db_value = repo.get(setting_keys::TMDB_API_KEY).await?;
    let db_set = db_value
        .as_deref()
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false);

    let (configured, source) = match (env_set, db_set) {
        (true, _) => (true, TmdbSource::Env),
        (false, true) => (true, TmdbSource::Db),
        (false, false) => (false, TmdbSource::None),
    };

    let tonemap_enabled = repo
        .get(setting_keys::TONEMAP_ENABLED)
        .await?
        .as_deref()
        .map(|v| {
            !matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "false" | "0" | "off"
            )
        })
        .unwrap_or(true);
    let tonemap_algorithm = repo
        .get(setting_keys::TONEMAP_ALGORITHM)
        .await?
        .as_deref()
        .map(TonemapAlgorithm::from_str_or_default)
        .unwrap_or_default();
    let tonemap_pipeline = repo
        .get(setting_keys::TONEMAP_PIPELINE)
        .await?
        .as_deref()
        .map(TonemapPipeline::from_str_or_default)
        .unwrap_or_default();

    let hardware_supported = hls
        .0
        .as_ref()
        .map(|m| m.hw_tonemap_available())
        .unwrap_or(false);

    Ok(Json(SettingsResponse {
        tmdb: TmdbSettings {
            configured,
            source,
            value: db_value.filter(|v| !v.trim().is_empty()),
        },
        tonemap: TonemapSettings {
            enabled: tonemap_enabled,
            algorithm: tonemap_algorithm.as_str().to_string(),
            pipeline: tonemap_pipeline.as_str().to_string(),
            hardware_supported,
        },
    }))
}

pub async fn put_settings(
    State(pool): State<SqlitePool>,
    State(tmdb): State<TmdbHandle>,
    State(posters): State<PostersDir>,
    State(hls): State<HlsHandle>,
    _user: AdminUser,
    Json(update): Json<SettingsUpdate>,
) -> ApiResult<Json<SettingsResponse>> {
    let repo = SettingsRepo::new(pool.clone());
    if let Some(value) = update.tmdb_api_key.as_deref() {
        repo.set(setting_keys::TMDB_API_KEY, value.trim()).await?;
    }
    if let Some(enabled) = update.tonemap_enabled {
        repo.set(
            setting_keys::TONEMAP_ENABLED,
            if enabled { "true" } else { "false" },
        )
        .await?;
    }
    if let Some(algo) = update.tonemap_algorithm.as_deref() {
        // Normalize on the way in: unknown values collapse to the
        // default so the DB never grows mystery strings, and the
        // value we round-trip back to the UI is always one of the
        // four known slugs.
        let coerced = TonemapAlgorithm::from_str_or_default(algo);
        repo.set(setting_keys::TONEMAP_ALGORITHM, coerced.as_str())
            .await?;
    }
    if let Some(pipeline) = update.tonemap_pipeline.as_deref() {
        let coerced = TonemapPipeline::from_str_or_default(pipeline);
        repo.set(setting_keys::TONEMAP_PIPELINE, coerced.as_str())
            .await?;
    }

    // Rebuild the in-memory client from whatever the new active
    // value is (env or DB), so the swap takes effect immediately.
    let active = resolve_tmdb_api_key(&pool).await;
    let client = active.map(|key| build_tmdb_client(&key, posters.0.clone()));
    tmdb.replace(client).await;

    get_settings(State(pool), State(hls), _user).await
}
