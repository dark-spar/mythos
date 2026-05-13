//! `/api/libraries` handlers.
//!
//! `GET` (list / fetch-one) is open to any authenticated user so the
//! browse UI can list libraries it has access to. `POST` and `DELETE`
//! require `is_admin`. The admin/auth split is enforced by the
//! [`AdminUser`] / [`AuthUser`] extractors — handlers don't re-check.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use mythos_auth::{AdminUser, AuthUser};
use mythos_core::{Library, NewLibrary};
use mythos_db::LibraryRepo;
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};

pub async fn list(
    State(pool): State<SqlitePool>,
    _user: AuthUser,
) -> ApiResult<Json<Vec<Library>>> {
    let libraries = LibraryRepo::new(pool).list().await?;
    Ok(Json(libraries))
}

pub async fn get_one(
    State(pool): State<SqlitePool>,
    _user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Library>> {
    let library = LibraryRepo::new(pool)
        .find_by_id(id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found"))?;
    Ok(Json(library))
}

pub async fn create(
    State(pool): State<SqlitePool>,
    _user: AdminUser,
    Json(new): Json<NewLibrary>,
) -> ApiResult<Json<Library>> {
    validate(&new).await?;
    let library = LibraryRepo::new(pool).insert(new).await?;
    Ok(Json(library))
}

pub async fn delete(
    State(pool): State<SqlitePool>,
    _user: AdminUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let removed = LibraryRepo::new(pool).delete(id).await?;
    if !removed {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "not_found"));
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn validate(new: &NewLibrary) -> ApiResult<()> {
    if new.name.trim().is_empty() {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "name_required"));
    }
    if !new.root_path.is_absolute() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "root_path_not_absolute",
        ));
    }
    let metadata = tokio::fs::metadata(&new.root_path)
        .await
        .map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "root_path_not_found"))?;
    if !metadata.is_dir() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "root_path_not_directory",
        ));
    }
    Ok(())
}
