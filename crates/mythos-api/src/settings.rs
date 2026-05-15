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
use mythos_stream::{HwAccel, TonemapAlgorithm, TonemapPipeline, TonemapSupport};
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
    /// Operator's stored pipeline pick — `software` / `vaapi` /
    /// `opencl` / `cuda`. Matches
    /// [`mythos_stream::TonemapPipeline::as_str`]. The HLS handler
    /// coerces invalid picks down to `software` at request time
    /// without rewriting this value.
    pub pipeline: String,
    /// Slug of the active hardware encoder (`vaapi` / `nvenc` /
    /// `qsv` / `videotoolbox` / `cpu`). The UI uses this to decide
    /// which pipeline radio options to render — e.g. `cuda` is
    /// only offered on NVENC, `vaapi`/`opencl` only on VAAPI.
    pub encoder: String,
    /// Pipelines that make sense for the active encoder, paired
    /// with whether this ffmpeg build can actually run each one.
    /// The UI renders an "(unavailable)" hint on any entry where
    /// `available = false` so the operator knows why a GPU option
    /// is greyed out (e.g. `tonemap_opencl` requires
    /// `intel-compute-runtime`).
    pub pipeline_options: Vec<PipelineOption>,
}

#[derive(Debug, Serialize)]
pub struct PipelineOption {
    /// One of the [`mythos_stream::TonemapPipeline`] slugs.
    pub value: String,
    /// `false` when the named filter isn't compiled into the active
    /// ffmpeg. `Software` is always `true` (it's the CPU chain and
    /// has no external dependency).
    pub available: bool,
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
    /// Tonemap pipeline — `software` / `vaapi` / `opencl` / `cuda`.
    /// Unknown values (including the legacy `hardware`) coerce to
    /// `software` on the way in.
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

    // Manager-less mode (transcoding disabled) reports CPU as the
    // active encoder and only the Software pipeline as valid, so the
    // admin UI doesn't offer HW options that wouldn't run anyway.
    let (active_accel, support) = hls
        .0
        .as_ref()
        .map(|m| (m.hwaccel(), m.tonemap_support()))
        .unwrap_or((HwAccel::Cpu, TonemapSupport::default()));
    let pipeline_options = TonemapSupport::valid_for(active_accel)
        .iter()
        .map(|p| PipelineOption {
            value: p.as_str().to_string(),
            available: support.supports(*p),
        })
        .collect();

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
            encoder: active_accel.as_str().to_string(),
            pipeline_options,
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
