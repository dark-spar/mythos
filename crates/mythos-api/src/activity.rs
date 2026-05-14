//! User-activity endpoints — continue-watching today, watched-history
//! / recently-added later. Authenticated; results are always scoped
//! to the requesting user.

use axum::Json;
use axum::extract::{Query, State};
use mythos_auth::AuthUser;
use mythos_core::ContinueWatchingItem;
use mythos_db::ActivityRepo;
use serde::Deserialize;
use sqlx::SqlitePool;

use crate::error::ApiResult;

const DEFAULT_LIMIT: i64 = 24;
const MAX_LIMIT: i64 = 100;

#[derive(Debug, Deserialize)]
pub struct LimitQuery {
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_limit() -> i64 {
    DEFAULT_LIMIT
}

pub async fn continue_watching(
    State(pool): State<SqlitePool>,
    user: AuthUser,
    Query(q): Query<LimitQuery>,
) -> ApiResult<Json<Vec<ContinueWatchingItem>>> {
    let limit = q.limit.clamp(1, MAX_LIMIT);
    let items = ActivityRepo::new(pool)
        .continue_watching(user.id, limit)
        .await?;
    Ok(Json(items))
}
