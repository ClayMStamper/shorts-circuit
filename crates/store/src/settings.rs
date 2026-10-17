use chrono::Utc;
use serde::Serialize;
use serde::de::DeserializeOwned;
use sqlx::SqlitePool;

use crate::StoreResult;
use crate::codec::{json, parse_json, ts};

/// Small key/value store for user preferences (timezone, overdue policy, watch folder).
pub struct Settings<'a> {
    pool: &'a SqlitePool,
}

impl<'a> Settings<'a> {
    pub(crate) fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn get<T: DeserializeOwned>(&self, key: &str) -> StoreResult<Option<T>> {
        let raw: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
            .bind(key)
            .fetch_optional(self.pool)
            .await?;
        raw.as_deref().map(|r| parse_json(r, key)).transpose()
    }

    pub async fn set<T: Serialize>(&self, key: &str, value: &T) -> StoreResult<()> {
        sqlx::query(
            "INSERT INTO settings (key, value, updated_at) VALUES (?, ?, ?) \
             ON CONFLICT (key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        )
        .bind(key)
        .bind(json(value))
        .bind(ts(Utc::now()))
        .execute(self.pool)
        .await?;
        Ok(())
    }

    pub async fn remove(&self, key: &str) -> StoreResult<()> {
        sqlx::query("DELETE FROM settings WHERE key = ?")
            .bind(key)
            .execute(self.pool)
            .await?;
        Ok(())
    }
}
