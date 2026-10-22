use chrono::{NaiveDate, Utc};
use sqlx::SqlitePool;

use crate::StoreResult;
use crate::codec::ts;

/// Daily provider quota spend, by bucket (e.g. `youtube:uploads`) and quota day.
pub struct Quota<'a> {
    pool: &'a SqlitePool,
}

fn day_key(day: NaiveDate) -> String {
    day.format("%Y-%m-%d").to_string()
}

impl<'a> Quota<'a> {
    pub(crate) fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }

    /// Units spent in `bucket` on `day`.
    pub async fn used(&self, bucket: &str, day: NaiveDate) -> StoreResult<u32> {
        let units: Option<i64> =
            sqlx::query_scalar("SELECT units FROM quota_usage WHERE bucket = ? AND day = ?")
                .bind(bucket)
                .bind(day_key(day))
                .fetch_optional(self.pool)
                .await?;
        Ok(units.map_or(0, |u| u32::try_from(u.max(0)).unwrap_or(u32::MAX)))
    }

    /// Records `units` more spent and returns the day's total.
    pub async fn spend(&self, bucket: &str, day: NaiveDate, units: u32) -> StoreResult<u32> {
        let total: i64 = sqlx::query_scalar(
            "INSERT INTO quota_usage (bucket, day, units, updated_at) VALUES (?, ?, ?, ?) \
             ON CONFLICT (bucket, day) DO UPDATE SET units = units + excluded.units, \
             updated_at = excluded.updated_at RETURNING units",
        )
        .bind(bucket)
        .bind(day_key(day))
        .bind(i64::from(units))
        .bind(ts(Utc::now()))
        .fetch_one(self.pool)
        .await?;
        Ok(u32::try_from(total.max(0)).unwrap_or(u32::MAX))
    }

    /// The provider said the quota is gone: count the day as fully used.
    pub async fn exhaust(&self, bucket: &str, day: NaiveDate, limit: u32) -> StoreResult<()> {
        sqlx::query(
            "INSERT INTO quota_usage (bucket, day, units, updated_at) VALUES (?, ?, ?, ?) \
             ON CONFLICT (bucket, day) DO UPDATE SET units = max(units, excluded.units), \
             updated_at = excluded.updated_at",
        )
        .bind(bucket)
        .bind(day_key(day))
        .bind(i64::from(limit))
        .bind(ts(Utc::now()))
        .execute(self.pool)
        .await?;
        Ok(())
    }

    /// Drops records for days before `day`.
    pub async fn prune_before(&self, day: NaiveDate) -> StoreResult<u64> {
        Ok(sqlx::query("DELETE FROM quota_usage WHERE day < ?")
            .bind(day_key(day))
            .execute(self.pool)
            .await?
            .rows_affected())
    }
}
