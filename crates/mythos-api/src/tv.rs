//! TV browse + playback surface: series → seasons → episodes.
//!
//! Browse endpoints are read-only; episode playback (`/stream`,
//! `/progress`) routes through the same helpers as the movie surface.
//! HLS and play-decision endpoints for episodes live in `hls.rs` and
//! `play.rs` respectively so the kind-aware logic stays colocated
//! with the movie versions. All endpoints require an authenticated
//! user (not admin).

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use mythos_auth::AuthUser;
use mythos_core::{Episode, EpisodeProgress, MediaFile, Season, Series, SubtitleTrack};
use mythos_db::{
    EpisodeProgressRepo, EpisodeRepo, MediaFileRepo, SeasonRepo, SeriesRepo, SubtitleRepo,
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::PostersDir;
use crate::error::{ApiError, ApiResult};
use crate::stream::stream_file;

const DEFAULT_LIMIT: i64 = 60;
const MAX_LIMIT: i64 = 200;

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

fn default_limit() -> i64 {
    DEFAULT_LIMIT
}

#[derive(Debug, Serialize)]
pub struct SeriesPage {
    pub items: Vec<Series>,
    pub total: i64,
    pub limit: i64,
    pub offset: i64,
}

#[derive(Debug, Serialize)]
pub struct SeriesDetail {
    pub series: Series,
    pub seasons: Vec<Season>,
}

#[derive(Debug, Serialize)]
pub struct SeasonDetail {
    pub series: Series,
    pub season: Season,
    pub episodes: Vec<Episode>,
}

#[derive(Debug, Serialize)]
pub struct EpisodeDetail {
    pub episode: Episode,
    pub season: Season,
    pub series: Series,
    pub file: MediaFile,
    pub subtitles: Vec<SubtitleTrack>,
    /// Previous episode in the series — `None` if this is the first
    /// episode in season 1 (or however the operator's content
    /// orders).
    pub prev: Option<Episode>,
    /// Next episode in the series — `None` at the last episode.
    pub next: Option<Episode>,
    /// Per-user resume point. `None` when the requesting user has not
    /// played this episode yet.
    pub progress: Option<EpisodeProgress>,
}

#[derive(Debug, Deserialize)]
pub struct ProgressUpdate {
    pub position_seconds: f64,
    pub duration_seconds: f64,
}

pub async fn list_series(
    State(pool): State<SqlitePool>,
    _user: AuthUser,
    Path(library_id): Path<Uuid>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<SeriesPage>> {
    let limit = q.limit.clamp(1, MAX_LIMIT);
    let offset = q.offset.max(0);

    let repo = SeriesRepo::new(pool);
    let items = repo.list_by_library(library_id, limit, offset).await?;
    let total = repo.count_by_library(library_id).await?;

    Ok(Json(SeriesPage {
        items,
        total,
        limit,
        offset,
    }))
}

pub async fn get_series(
    State(pool): State<SqlitePool>,
    _user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<SeriesDetail>> {
    let series = SeriesRepo::new(pool.clone())
        .find_by_id(id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found"))?;

    let seasons = SeasonRepo::new(pool).list_by_series(id).await?;

    Ok(Json(SeriesDetail { series, seasons }))
}

pub async fn series_poster(
    State(posters): State<PostersDir>,
    _user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    serve_image(posters.0.join(format!("{id}.jpg"))).await
}

pub async fn get_season(
    State(pool): State<SqlitePool>,
    _user: AuthUser,
    Path((series_id, season_number)): Path<(Uuid, i64)>,
) -> ApiResult<Json<SeasonDetail>> {
    let series = SeriesRepo::new(pool.clone())
        .find_by_id(series_id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found"))?;

    let season = SeasonRepo::new(pool.clone())
        .find_by_series_and_number(series_id, season_number)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found"))?;

    let episodes = EpisodeRepo::new(pool).list_by_season(season.id).await?;

    Ok(Json(SeasonDetail {
        series,
        season,
        episodes,
    }))
}

pub async fn get_episode(
    State(pool): State<SqlitePool>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<EpisodeDetail>> {
    let episodes = EpisodeRepo::new(pool.clone());
    let episode = episodes
        .find_by_id(id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found"))?;

    let season = SeasonRepo::new(pool.clone())
        .find_by_id(episode.season_id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "season_missing"))?;

    let series = SeriesRepo::new(pool.clone())
        .find_by_id(season.series_id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "series_missing"))?;

    let file = MediaFileRepo::new(pool.clone())
        .find_by_id(episode.file_id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "file_missing"))?;

    let subtitles = SubtitleRepo::new(pool.clone())
        .list_by_file(file.id)
        .await?;
    let (prev, next) = episodes.find_neighbors(id).await?;
    let progress = EpisodeProgressRepo::new(pool).find(user.id, id).await?;

    Ok(Json(EpisodeDetail {
        episode,
        season,
        series,
        file,
        subtitles,
        prev,
        next,
        progress,
    }))
}

pub async fn episode_stream(
    State(pool): State<SqlitePool>,
    _user: AuthUser,
    Path(episode_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let episode = EpisodeRepo::new(pool.clone())
        .find_by_id(episode_id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found"))?;
    stream_file(&pool, episode.file_id, &headers).await
}

pub async fn episode_put_progress(
    State(pool): State<SqlitePool>,
    user: AuthUser,
    Path(episode_id): Path<Uuid>,
    Json(update): Json<ProgressUpdate>,
) -> ApiResult<StatusCode> {
    if !update.position_seconds.is_finite()
        || !update.duration_seconds.is_finite()
        || update.position_seconds < 0.0
        || update.duration_seconds <= 0.0
    {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "invalid_progress"));
    }

    // Sharper 404 than letting the FK reject the insert.
    let exists = EpisodeRepo::new(pool.clone())
        .find_by_id(episode_id)
        .await?
        .is_some();
    if !exists {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "not_found"));
    }

    EpisodeProgressRepo::new(pool)
        .upsert(
            user.id,
            episode_id,
            update.position_seconds,
            update.duration_seconds,
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn episode_still(
    State(posters): State<PostersDir>,
    _user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    serve_image(posters.0.join("stills").join(format!("{id}.jpg"))).await
}

async fn serve_image(path: std::path::PathBuf) -> ApiResult<Response> {
    match tokio::fs::read(&path).await {
        Ok(bytes) => {
            let mut res = (StatusCode::OK, bytes).into_response();
            res.headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static("image/jpeg"));
            res.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=3600"),
            );
            Ok(res)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            Err(ApiError::new(StatusCode::NOT_FOUND, "not_found"))
        }
        Err(err) => {
            tracing::error!(?err, path = %path.display(), "failed to read image");
            Err(ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal"))
        }
    }
}
