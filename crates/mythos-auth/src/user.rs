//! SQLx-backed user repository.
//!
//! IDs and timestamps live in SQLite as `TEXT` (UUID v7 hex / RFC3339);
//! we parse on read and stringify on write at this boundary so the rest
//! of the codebase gets `Uuid` and `DateTime<Utc>`.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::error::AuthError;

#[derive(Debug, Clone, Serialize)]
pub struct User {
    pub id: Uuid,
    pub username: String,
    #[serde(skip)]
    pub password_hash: String,
    pub is_admin: bool,
    #[serde(skip)]
    pub token_version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewUser {
    pub username: String,
    pub password_hash: String,
    pub is_admin: bool,
}

#[derive(sqlx::FromRow)]
struct UserRow {
    id: String,
    username: String,
    password_hash: String,
    is_admin: i64,
    token_version: i64,
    created_at: String,
    updated_at: String,
}

impl UserRow {
    fn into_user(self) -> Result<User, AuthError> {
        Ok(User {
            id: Uuid::parse_str(&self.id)
                .map_err(|err| AuthError::Internal(format!("invalid user id in db: {err}")))?,
            username: self.username,
            password_hash: self.password_hash,
            is_admin: self.is_admin != 0,
            token_version: self.token_version,
            created_at: parse_ts(&self.created_at)?,
            updated_at: parse_ts(&self.updated_at)?,
        })
    }
}

fn parse_ts(s: &str) -> Result<DateTime<Utc>, AuthError> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|err| AuthError::Internal(format!("invalid timestamp in db: {err}")))
}

#[derive(Debug, Clone)]
pub struct UserRepo {
    pool: SqlitePool,
}

impl UserRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn count(&self) -> Result<i64, AuthError> {
        let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users")
            .fetch_one(&self.pool)
            .await?;
        Ok(count)
    }

    pub async fn find_by_id(&self, id: Uuid) -> Result<Option<User>, AuthError> {
        let row: Option<UserRow> = sqlx::query_as(
            "SELECT id, username, password_hash, is_admin, token_version, \
             created_at, updated_at FROM users WHERE id = ?",
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await?;
        row.map(UserRow::into_user).transpose()
    }

    pub async fn find_by_username(&self, username: &str) -> Result<Option<User>, AuthError> {
        let row: Option<UserRow> = sqlx::query_as(
            "SELECT id, username, password_hash, is_admin, token_version, \
             created_at, updated_at FROM users WHERE username = ?",
        )
        .bind(username)
        .fetch_optional(&self.pool)
        .await?;
        row.map(UserRow::into_user).transpose()
    }

    /// Insert a user atomically when the table is empty. Single statement
    /// `INSERT ... WHERE NOT EXISTS` so two concurrent registration
    /// requests can't both succeed.
    ///
    /// Returns `Ok(Some(user))` on success, `Ok(None)` if the table was
    /// non-empty.
    pub async fn insert_first(&self, new: NewUser) -> Result<Option<User>, AuthError> {
        let id = Uuid::now_v7();
        let result = sqlx::query(
            "INSERT INTO users (id, username, password_hash, is_admin) \
             SELECT ?, ?, ?, ? \
             WHERE NOT EXISTS (SELECT 1 FROM users)",
        )
        .bind(id.to_string())
        .bind(&new.username)
        .bind(&new.password_hash)
        .bind(i64::from(new.is_admin))
        .execute(&self.pool)
        .await?;

        if result.rows_affected() == 0 {
            return Ok(None);
        }
        self.find_by_id(id)
            .await?
            .map(Some)
            .ok_or_else(|| AuthError::Internal("user disappeared after insert_first".into()))
    }

    /// Unconditional insert (admin-driven account creation, not used yet
    /// but kept so the API surface is symmetric with `insert_first`).
    pub async fn insert(&self, new: NewUser) -> Result<User, AuthError> {
        let id = Uuid::now_v7();
        let result = sqlx::query(
            "INSERT INTO users (id, username, password_hash, is_admin) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(id.to_string())
        .bind(&new.username)
        .bind(&new.password_hash)
        .bind(i64::from(new.is_admin))
        .execute(&self.pool)
        .await;

        match result {
            Ok(_) => self
                .find_by_id(id)
                .await?
                .ok_or_else(|| AuthError::Internal("user disappeared after insert".into())),
            Err(sqlx::Error::Database(db)) if db.is_unique_violation() => {
                Err(AuthError::UsernameTaken)
            }
            Err(e) => Err(AuthError::Db(e)),
        }
    }

    /// Bump the user's `token_version`, invalidating all outstanding
    /// tokens regardless of transport.
    pub async fn bump_token_version(&self, id: Uuid) -> Result<(), AuthError> {
        sqlx::query(
            "UPDATE users SET token_version = token_version + 1, \
             updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
             WHERE id = ?",
        )
        .bind(id.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
