//! TV-specific domain types: series → seasons → episodes.
//!
//! `Episode` FKs 1:1 to a `MediaFile` exactly like [`crate::Movie`]
//! does, so subtitle / probe / streaming code in the rest of the
//! workspace stays media-type-agnostic.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Series {
    pub id: Uuid,
    pub library_id: Uuid,
    pub title: String,
    pub sort_title: String,
    pub year: Option<i64>,
    pub tmdb_id: Option<i64>,
    pub overview: Option<String>,
    pub poster_url: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewSeries {
    pub library_id: Uuid,
    pub title: String,
    pub year: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Season {
    pub id: Uuid,
    pub series_id: Uuid,
    pub season_number: i64,
    pub title: Option<String>,
    pub tmdb_id: Option<i64>,
    pub overview: Option<String>,
    pub poster_url: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewSeason {
    pub series_id: Uuid,
    pub season_number: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Episode {
    pub id: Uuid,
    pub season_id: Uuid,
    pub file_id: Uuid,
    pub episode_number: i64,
    pub title: Option<String>,
    pub tmdb_id: Option<i64>,
    pub overview: Option<String>,
    pub still_url: Option<String>,
    pub air_date: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewEpisode {
    pub season_id: Uuid,
    pub file_id: Uuid,
    pub episode_number: i64,
    pub title: Option<String>,
}

/// Per-user resume point for an episode. Same shape as
/// [`crate::WatchProgress`]; kept as a sibling type so future API
/// responses can distinguish episode progress from movie progress
/// without a discriminator field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpisodeProgress {
    pub position_seconds: f64,
    pub duration_seconds: f64,
    pub updated_at: DateTime<Utc>,
}
