//! `media_files` repository: idempotent upserts keyed on
//! `(library_id, path)`, plus the "prune anything not touched this scan"
//! helper used at the end of a scan run.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use mythos_core::{MediaFile, NewMediaFile, Probe};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::{DbError, Result};

#[derive(sqlx::FromRow)]
struct MediaFileRow {
    id: String,
    library_id: String,
    path: String,
    size_bytes: i64,
    mtime: String,
    container: Option<String>,
    video_codec: Option<String>,
    audio_codec: Option<String>,
    duration_seconds: Option<f64>,
    width: Option<i64>,
    height: Option<i64>,
    color_primaries: Option<String>,
    color_transfer: Option<String>,
    color_space: Option<String>,
    scanned_at: String,
}

impl MediaFileRow {
    fn into_media_file(self) -> Result<MediaFile> {
        Ok(MediaFile {
            id: parse_uuid("media_file id", &self.id)?,
            library_id: parse_uuid("media_file library_id", &self.library_id)?,
            path: PathBuf::from(self.path),
            size_bytes: self.size_bytes,
            mtime: parse_ts("media_file mtime", &self.mtime)?,
            probe: Probe {
                container: self.container,
                video_codec: self.video_codec,
                audio_codec: self.audio_codec,
                duration_seconds: self.duration_seconds,
                width: self.width,
                height: self.height,
                color_primaries: self.color_primaries,
                color_transfer: self.color_transfer,
                color_space: self.color_space,
                // Subtitles aren't carried alongside the MediaFile row;
                // SubtitleRepo fetches them per-file when needed.
                subtitles: Vec::new(),
            },
            scanned_at: parse_ts("media_file scanned_at", &self.scanned_at)?,
        })
    }
}

fn parse_uuid(label: &str, s: &str) -> Result<Uuid> {
    Uuid::parse_str(s).map_err(|err| DbError::Decode(format!("invalid {label}: {err}")))
}

fn parse_ts(label: &str, s: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|err| DbError::Decode(format!("invalid {label}: {err}")))
}

fn ts_string(dt: DateTime<Utc>) -> String {
    dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn path_to_str(path: &std::path::Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| DbError::Decode(format!("path is not valid UTF-8: {}", path.display())))
}

#[derive(Debug, Clone)]
pub struct MediaFileRepo {
    pool: SqlitePool,
}

impl MediaFileRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Insert or refresh a media file. Returns whether this call inserted
    /// a new row (`true`) or updated an existing one (`false`), along
    /// with the resulting row.
    ///
    /// Subtitle rows for the file are replaced (delete-then-insert) in
    /// the same transaction so a rescan that finds different tracks
    /// (or none) doesn't leave stale ones behind.
    pub async fn upsert(&self, new: NewMediaFile) -> Result<(MediaFile, bool)> {
        let new_id = Uuid::now_v7();
        let path = path_to_str(&new.path)?;
        let mtime = ts_string(new.mtime);

        let mut tx = self.pool.begin().await?;

        let row: MediaFileRow = sqlx::query_as(
            "INSERT INTO media_files \
               (id, library_id, path, size_bytes, mtime, container, video_codec, \
                audio_codec, duration_seconds, width, height, \
                color_primaries, color_transfer, color_space) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (library_id, path) DO UPDATE SET \
               size_bytes       = excluded.size_bytes, \
               mtime            = excluded.mtime, \
               container        = excluded.container, \
               video_codec      = excluded.video_codec, \
               audio_codec      = excluded.audio_codec, \
               duration_seconds = excluded.duration_seconds, \
               width            = excluded.width, \
               height           = excluded.height, \
               color_primaries  = excluded.color_primaries, \
               color_transfer   = excluded.color_transfer, \
               color_space      = excluded.color_space, \
               scanned_at       = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
             RETURNING id, library_id, path, size_bytes, mtime, container, \
               video_codec, audio_codec, duration_seconds, width, height, \
               color_primaries, color_transfer, color_space, \
               scanned_at",
        )
        .bind(new_id.to_string())
        .bind(new.library_id.to_string())
        .bind(path)
        .bind(new.size_bytes)
        .bind(&mtime)
        .bind(&new.probe.container)
        .bind(&new.probe.video_codec)
        .bind(&new.probe.audio_codec)
        .bind(new.probe.duration_seconds)
        .bind(new.probe.width)
        .bind(new.probe.height)
        .bind(&new.probe.color_primaries)
        .bind(&new.probe.color_transfer)
        .bind(&new.probe.color_space)
        .fetch_one(&mut *tx)
        .await?;

        sqlx::query("DELETE FROM media_subtitles WHERE file_id = ?")
            .bind(&row.id)
            .execute(&mut *tx)
            .await?;

        for sub in &new.probe.subtitles {
            sqlx::query(
                "INSERT INTO media_subtitles \
                   (id, file_id, stream_index, codec, language, title, \
                    is_image, is_default, is_forced) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(Uuid::now_v7().to_string())
            .bind(&row.id)
            .bind(sub.stream_index)
            .bind(&sub.codec)
            .bind(sub.language.as_deref())
            .bind(sub.title.as_deref())
            .bind(i64::from(sub.is_image))
            .bind(i64::from(sub.is_default))
            .bind(i64::from(sub.is_forced))
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;

        let inserted = row.id == new_id.to_string();
        Ok((row.into_media_file()?, inserted))
    }

    pub async fn list_by_library(&self, library_id: Uuid) -> Result<Vec<MediaFile>> {
        let rows: Vec<MediaFileRow> = sqlx::query_as(
            "SELECT id, library_id, path, size_bytes, mtime, container, \
                    video_codec, audio_codec, duration_seconds, width, height, \
                    color_primaries, color_transfer, color_space, \
                    scanned_at \
             FROM media_files WHERE library_id = ? ORDER BY path",
        )
        .bind(library_id.to_string())
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(MediaFileRow::into_media_file)
            .collect()
    }

    pub async fn find_by_id(&self, id: Uuid) -> Result<Option<MediaFile>> {
        let row: Option<MediaFileRow> = sqlx::query_as(
            "SELECT id, library_id, path, size_bytes, mtime, container, \
                    video_codec, audio_codec, duration_seconds, width, height, \
                    color_primaries, color_transfer, color_space, \
                    scanned_at \
             FROM media_files WHERE id = ?",
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await?;
        row.map(MediaFileRow::into_media_file).transpose()
    }

    /// Remove media_files in `library_id` whose `path` is not in the
    /// supplied set. Called at the end of a scan to drop rows for files
    /// that no longer exist on disk. Cascade-deletes the associated
    /// movies row.
    ///
    /// Note: SQLite caps bind parameters at ~32k. For libraries that
    /// large we'd want batched pruning; deferred until a real-world
    /// library hits that limit.
    pub async fn prune_unseen(&self, library_id: Uuid, seen_paths: &[String]) -> Result<u64> {
        let id_str = library_id.to_string();

        if seen_paths.is_empty() {
            let result = sqlx::query("DELETE FROM media_files WHERE library_id = ?")
                .bind(id_str)
                .execute(&self.pool)
                .await?;
            return Ok(result.rows_affected());
        }

        let placeholders = vec!["?"; seen_paths.len()].join(",");
        let sql = format!(
            "DELETE FROM media_files WHERE library_id = ? AND path NOT IN ({placeholders})"
        );
        let mut q = sqlx::query(&sql).bind(id_str);
        for p in seen_paths {
            q = q.bind(p.as_str());
        }
        let result = q.execute(&self.pool).await?;
        Ok(result.rows_affected())
    }
}
