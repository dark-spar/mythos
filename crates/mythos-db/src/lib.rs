//! Database access layer: connection pool, migrations, and query helpers.

pub mod library;
pub mod media_file;
pub mod movie;
pub mod progress;
pub mod subtitle;

pub use library::LibraryRepo;
pub use media_file::MediaFileRepo;
pub use movie::{MovieRepo, UnenrichedMovie};
pub use progress::ProgressRepo;
pub use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use std::path::Path;
use std::str::FromStr;
pub use subtitle::SubtitleRepo;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("sqlx error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    /// A row could not be turned into its domain type (corrupt id, bad
    /// enum value, malformed timestamp, non-UTF-8 path). Means schema and
    /// code have drifted — not a user error.
    #[error("decode error: {0}")]
    Decode(String),
    /// Insert hit the `idx_libraries_root_path` unique index.
    #[error("a library with that root path already exists")]
    RootPathTaken,
}

pub type Result<T> = std::result::Result<T, DbError>;

/// Open a SQLite pool against the given path, applying recommended pragmas
/// for a self-hosted media server (WAL, NORMAL sync, foreign keys).
pub async fn open(path: &Path) -> Result<SqlitePool> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).ok();
    }

    let url = format!("sqlite://{}", path.display());
    let options = SqliteConnectOptions::from_str(&url)?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true)
        .busy_timeout(std::time::Duration::from_secs(5));

    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(options)
        .await?;

    Ok(pool)
}

/// Run all embedded migrations against the given pool.
pub async fn migrate(pool: &SqlitePool) -> Result<()> {
    sqlx::migrate!("../../migrations").run(pool).await?;
    Ok(())
}
