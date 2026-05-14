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
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::error::ApiResult;
use crate::{PostersDir, TmdbHandle, build_tmdb_client, resolve_tmdb_api_key};

#[derive(Debug, Serialize)]
pub struct SettingsResponse {
    pub tmdb: TmdbSettings,
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
}

pub async fn get_settings(
    State(pool): State<SqlitePool>,
    _user: AdminUser,
) -> ApiResult<Json<SettingsResponse>> {
    let env_set = std::env::var("MYTHOS_TMDB_API_KEY")
        .ok()
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false);
    let db_value = SettingsRepo::new(pool.clone())
        .get(setting_keys::TMDB_API_KEY)
        .await?;
    let db_set = db_value
        .as_deref()
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false);

    let (configured, source) = match (env_set, db_set) {
        (true, _) => (true, TmdbSource::Env),
        (false, true) => (true, TmdbSource::Db),
        (false, false) => (false, TmdbSource::None),
    };

    Ok(Json(SettingsResponse {
        tmdb: TmdbSettings {
            configured,
            source,
            value: db_value.filter(|v| !v.trim().is_empty()),
        },
    }))
}

pub async fn put_settings(
    State(pool): State<SqlitePool>,
    State(tmdb): State<TmdbHandle>,
    State(posters): State<PostersDir>,
    _user: AdminUser,
    Json(update): Json<SettingsUpdate>,
) -> ApiResult<Json<SettingsResponse>> {
    if let Some(value) = update.tmdb_api_key.as_deref() {
        SettingsRepo::new(pool.clone())
            .set(setting_keys::TMDB_API_KEY, value.trim())
            .await?;
    }

    // Rebuild the in-memory client from whatever the new active
    // value is (env or DB), so the swap takes effect immediately.
    let active = resolve_tmdb_api_key(&pool).await;
    let client = active.map(|key| build_tmdb_client(&key, posters.0.clone()));
    tmdb.replace(client).await;

    get_settings(State(pool), _user).await
}
