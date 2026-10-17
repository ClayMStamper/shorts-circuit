//! Column encoding helpers shared by the repositories.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde::de::DeserializeOwned;
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::{StoreError, StoreResult};

/// Fixed-width UTC text, so string comparison in SQL matches time order.
pub(crate) fn ts(at: DateTime<Utc>) -> String {
    at.format("%Y-%m-%dT%H:%M:%S%.9fZ").to_string()
}

pub(crate) fn opt_ts(at: Option<DateTime<Utc>>) -> Option<String> {
    at.map(ts)
}

pub(crate) fn parse_ts(raw: &str) -> StoreResult<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|e| StoreError::Corrupt(format!("bad timestamp {raw:?}: {e}")))
}

pub(crate) fn json<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).expect("domain values serialize to JSON")
}

pub(crate) fn opt_json<T: Serialize>(value: Option<&T>) -> Option<String> {
    value.map(json)
}

pub(crate) fn parse_json<T: DeserializeOwned>(raw: &str, what: &str) -> StoreResult<T> {
    serde_json::from_str(raw).map_err(|e| StoreError::Corrupt(format!("bad {what}: {e}")))
}

/// Typed column access with errors mapped to [`StoreError`].
pub(crate) trait RowExt {
    fn text(&self, col: &str) -> StoreResult<String>;
    fn opt_text(&self, col: &str) -> StoreResult<Option<String>>;
    fn int(&self, col: &str) -> StoreResult<i64>;
    fn opt_int(&self, col: &str) -> StoreResult<Option<i64>>;

    fn time(&self, col: &str) -> StoreResult<DateTime<Utc>> {
        parse_ts(&self.text(col)?)
    }

    fn opt_time(&self, col: &str) -> StoreResult<Option<DateTime<Utc>>> {
        self.opt_text(col)?.as_deref().map(parse_ts).transpose()
    }

    fn json<T: DeserializeOwned>(&self, col: &str) -> StoreResult<T> {
        parse_json(&self.text(col)?, col)
    }

    fn opt_json<T: DeserializeOwned>(&self, col: &str) -> StoreResult<Option<T>> {
        self.opt_text(col)?
            .as_deref()
            .map(|raw| parse_json(raw, col))
            .transpose()
    }

    fn parsed<T: std::str::FromStr>(&self, col: &str) -> StoreResult<T> {
        let raw = self.text(col)?;
        raw.parse()
            .map_err(|_| StoreError::Corrupt(format!("bad {col}: {raw:?}")))
    }

    fn u32(&self, col: &str) -> StoreResult<u32> {
        let v = self.int(col)?;
        u32::try_from(v).map_err(|_| StoreError::Corrupt(format!("bad {col}: {v}")))
    }

    fn u64(&self, col: &str) -> StoreResult<u64> {
        let v = self.int(col)?;
        u64::try_from(v).map_err(|_| StoreError::Corrupt(format!("bad {col}: {v}")))
    }
}

impl RowExt for SqliteRow {
    fn text(&self, col: &str) -> StoreResult<String> {
        Ok(self.try_get(col)?)
    }

    fn opt_text(&self, col: &str) -> StoreResult<Option<String>> {
        Ok(self.try_get(col)?)
    }

    fn int(&self, col: &str) -> StoreResult<i64> {
        Ok(self.try_get(col)?)
    }

    fn opt_int(&self, col: &str) -> StoreResult<Option<i64>> {
        Ok(self.try_get(col)?)
    }
}

pub(crate) fn to_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// True when the error is a UNIQUE constraint violation.
pub(crate) fn is_unique_violation(err: &sqlx::Error) -> bool {
    matches!(err, sqlx::Error::Database(db) if db.is_unique_violation())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_are_fixed_width_and_round_trip() {
        let a = DateTime::from_timestamp(1_790_000_000, 5_000).unwrap();
        let b = DateTime::from_timestamp(1_790_000_001, 0).unwrap();
        assert_eq!(ts(a).len(), ts(b).len());
        assert!(ts(a) < ts(b));
        assert_eq!(parse_ts(&ts(a)).unwrap(), a);
        assert!(ts(a).ends_with('Z'));
    }
}
