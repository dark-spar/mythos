//! Cross-cutting "user activity" types: continue-watching, watched
//! history, and similar aggregations that span media kinds.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One in-progress item in the user's continue-watching row. The
/// payload differs by kind (movie vs episode), so it's an externally
/// tagged enum with a `kind` discriminator + flattened payload, which
/// is what TypeScript expects out of a discriminated union.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContinueWatchingItem {
    Movie(ContinueWatchingMovie),
    Episode(ContinueWatchingEpisode),
}

impl ContinueWatchingItem {
    /// The timestamp the UI sorts by ("most recent first"). Lifted
    /// out of the variant so callers can sort the merged list without
    /// pattern-matching.
    pub fn updated_at(&self) -> DateTime<Utc> {
        match self {
            ContinueWatchingItem::Movie(m) => m.updated_at,
            ContinueWatchingItem::Episode(e) => e.updated_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContinueWatchingMovie {
    pub id: Uuid,
    pub library_id: Uuid,
    pub title: String,
    pub year: Option<i64>,
    pub poster_url: Option<String>,
    pub position_seconds: f64,
    pub duration_seconds: f64,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContinueWatchingEpisode {
    /// Episode id (target of the `/episodes/:id` link).
    pub id: Uuid,
    pub library_id: Uuid,
    pub series_id: Uuid,
    pub series_title: String,
    pub season_number: i64,
    pub episode_number: i64,
    pub episode_title: Option<String>,
    /// Series poster — preferred for the tile (2:3 aspect matches
    /// movies). May be `None` before TMDb enrichment.
    pub poster_url: Option<String>,
    /// Episode still — 16:9 thumbnail. Available when TMDb has it
    /// for the specific episode.
    pub still_url: Option<String>,
    pub position_seconds: f64,
    pub duration_seconds: f64,
    pub updated_at: DateTime<Utc>,
}
