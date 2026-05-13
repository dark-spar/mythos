//! Library repository: CRUD against the `libraries` table.
//!
//! Mappings:
//! - `id` is stored as TEXT (UUID v7); parsed at this boundary.
//! - `kind` is stored as TEXT constrained by the migration's CHECK; mapped
//!   to / from [`LibraryKind`] via its string form.
//! - `root_path` is stored as TEXT; must be valid UTF-8. Insert rejects
//!   non-UTF-8 paths up front rather than silently lossy-converting.
//! - Timestamps are stored as RFC3339 TEXT and parsed via chrono.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use mythos_core::{Library, LibraryKind, NewLibrary};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::{DbError, Result};

#[derive(sqlx::FromRow)]
struct LibraryRow {
    id: String,
    name: String,
    kind: String,
    root_path: String,
    created_at: String,
    updated_at: String,
}

impl LibraryRow {
    fn into_library(self) -> Result<Library> {
        Ok(Library {
            id: Uuid::parse_str(&self.id)
                .map_err(|err| DbError::Decode(format!("invalid library id: {err}")))?,
            name: self.name,
            kind: LibraryKind::parse(&self.kind).ok_or_else(|| {
                DbError::Decode(format!("unknown library kind in db: {}", self.kind))
            })?,
            root_path: PathBuf::from(self.root_path),
            created_at: parse_ts(&self.created_at)?,
            updated_at: parse_ts(&self.updated_at)?,
        })
    }
}

fn parse_ts(s: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|err| DbError::Decode(format!("invalid timestamp in db: {err}")))
}

fn path_str(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| DbError::Decode(format!("path is not valid UTF-8: {}", path.display())))
}

#[derive(Debug, Clone)]
pub struct LibraryRepo {
    pool: SqlitePool,
}

impl LibraryRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn list(&self) -> Result<Vec<Library>> {
        let rows: Vec<LibraryRow> = sqlx::query_as(
            "SELECT id, name, kind, root_path, created_at, updated_at \
             FROM libraries ORDER BY name COLLATE NOCASE ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(LibraryRow::into_library).collect()
    }

    pub async fn find_by_id(&self, id: Uuid) -> Result<Option<Library>> {
        let row: Option<LibraryRow> = sqlx::query_as(
            "SELECT id, name, kind, root_path, created_at, updated_at \
             FROM libraries WHERE id = ?",
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await?;
        row.map(LibraryRow::into_library).transpose()
    }

    pub async fn insert(&self, new: NewLibrary) -> Result<Library> {
        let id = Uuid::now_v7();
        let path = path_str(&new.root_path)?;
        let result =
            sqlx::query("INSERT INTO libraries (id, name, kind, root_path) VALUES (?, ?, ?, ?)")
                .bind(id.to_string())
                .bind(&new.name)
                .bind(new.kind.as_str())
                .bind(path)
                .execute(&self.pool)
                .await;

        match result {
            Ok(_) => self
                .find_by_id(id)
                .await?
                .ok_or_else(|| DbError::Decode("library disappeared after insert".into())),
            Err(sqlx::Error::Database(db)) if db.is_unique_violation() => {
                Err(DbError::RootPathTaken)
            }
            Err(e) => Err(DbError::Sqlx(e)),
        }
    }

    /// Returns `true` if a row was removed.
    pub async fn delete(&self, id: Uuid) -> Result<bool> {
        let result = sqlx::query("DELETE FROM libraries WHERE id = ?")
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }
}
