//! `media_subtitles` repository.
//!
//! Subtitle rows are owned by their `media_files` row; the upsert path
//! in [`crate::MediaFileRepo`] replaces them on every rescan inside
//! the same transaction. This repo handles the read side.

use mythos_core::SubtitleTrack;
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::{DbError, Result};

#[derive(sqlx::FromRow)]
struct SubtitleRow {
    id: String,
    file_id: String,
    stream_index: i64,
    codec: String,
    language: Option<String>,
    title: Option<String>,
    is_image: i64,
    is_default: i64,
    is_forced: i64,
}

impl SubtitleRow {
    fn into_track(self) -> Result<SubtitleTrack> {
        Ok(SubtitleTrack {
            id: Uuid::parse_str(&self.id)
                .map_err(|err| DbError::Decode(format!("subtitle id: {err}")))?,
            file_id: Uuid::parse_str(&self.file_id)
                .map_err(|err| DbError::Decode(format!("subtitle file_id: {err}")))?,
            stream_index: self.stream_index,
            codec: self.codec,
            language: self.language,
            title: self.title,
            is_image: self.is_image != 0,
            is_default: self.is_default != 0,
            is_forced: self.is_forced != 0,
        })
    }
}

#[derive(Debug, Clone)]
pub struct SubtitleRepo {
    pool: SqlitePool,
}

impl SubtitleRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn list_by_file(&self, file_id: Uuid) -> Result<Vec<SubtitleTrack>> {
        let rows: Vec<SubtitleRow> = sqlx::query_as(
            "SELECT id, file_id, stream_index, codec, language, title, \
                    is_image, is_default, is_forced \
             FROM media_subtitles \
             WHERE file_id = ? \
             ORDER BY stream_index",
        )
        .bind(file_id.to_string())
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(SubtitleRow::into_track).collect()
    }

    pub async fn find_by_id(&self, id: Uuid) -> Result<Option<SubtitleTrack>> {
        let row: Option<SubtitleRow> = sqlx::query_as(
            "SELECT id, file_id, stream_index, codec, language, title, \
                    is_image, is_default, is_forced \
             FROM media_subtitles WHERE id = ?",
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await?;
        row.map(SubtitleRow::into_track).transpose()
    }
}
