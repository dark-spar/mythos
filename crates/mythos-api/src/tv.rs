//! TV browse surface: series → seasons → episodes.
//!
//! Read-only in Phase 3a — streaming + progress for episodes come in
//! 3c. All endpoints require an authenticated user (not admin); they
//! mirror the movie endpoints in shape and pagination.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use mythos_auth::AuthUser;
use mythos_core::{Episode, MediaFile, Season, Series, SubtitleTrack};
use mythos_db::{EpisodeRepo, MediaFileRepo, SeasonRepo, SeriesRepo, SubtitleRepo};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::PostersDir;
use crate::error::{ApiError, ApiResult};

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
    _user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<EpisodeDetail>> {
    let episode = EpisodeRepo::new(pool.clone())
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

    let subtitles = SubtitleRepo::new(pool).list_by_file(file.id).await?;

    Ok(Json(EpisodeDetail {
        episode,
        season,
        series,
        file,
        subtitles,
    }))
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
