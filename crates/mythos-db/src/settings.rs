//! Generic key/value settings store. Used by the admin settings
//! API for things the operator wants to configure from the browser
//! rather than the environment (TMDb API key today, more later).

use sqlx::SqlitePool;

use crate::Result;

#[derive(Debug, Clone)]
pub struct SettingsRepo {
    pool: SqlitePool,
}

impl SettingsRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn get(&self, key: &str) -> Result<Option<String>> {
        let row: Option<(String,)> = sqlx::query_as("SELECT value FROM settings WHERE key = ?")
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|(v,)| v))
    }

    /// Upsert. An empty string clears the setting (delete row) so a
    /// caller doesn't have to think about which of set/clear they
    /// want when the user submits an empty form field.
    pub async fn set(&self, key: &str, value: &str) -> Result<()> {
        if value.is_empty() {
            sqlx::query("DELETE FROM settings WHERE key = ?")
                .bind(key)
                .execute(&self.pool)
                .await?;
        } else {
            sqlx::query(
                "INSERT INTO settings (key, value) VALUES (?, ?) \
                 ON CONFLICT (key) DO UPDATE SET \
                   value = excluded.value, \
                   updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
            )
            .bind(key)
            .bind(value)
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }
}

/// Key constants. Centralized so a typo doesn't silently miss the
/// row, and so we have a single place to enumerate what's
/// configurable from the UI.
pub mod keys {
    pub const TMDB_API_KEY: &str = "tmdb_api_key";
}
