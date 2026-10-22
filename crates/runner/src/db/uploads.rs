//! Upload bookkeeping. The bytes live on disk ([`crate::media::MediaDir`]); the row says how
//! many of them arrived, and advances only from the offset the chunk was written at.

use chrono::{DateTime, Utc};
use shorts_cloud::UploadStatus;
use sqlx::Row;
use uuid::Uuid;

use super::{DbError, DbResult, PgStore, to_i64, to_u64};

fn decode(row: &sqlx::postgres::PgRow) -> DbResult<UploadStatus> {
    Ok(UploadStatus {
        sha256: row.try_get("sha256")?,
        size_bytes: to_u64(row.try_get("size_bytes")?, "size")?,
        received_bytes: to_u64(row.try_get("received_bytes")?, "received bytes")?,
        complete: row.try_get("complete")?,
    })
}

impl PgStore {
    /// Starts an upload, or returns the existing one for the same file.
    pub async fn start_upload(
        &self,
        installation: Uuid,
        sha256: &str,
        size_bytes: u64,
        now: DateTime<Utc>,
    ) -> DbResult<UploadStatus> {
        sqlx::query(
            "INSERT INTO uploads (installation_id, sha256, size_bytes, received_bytes, complete, \
             created_at, updated_at) VALUES ($1, $2, $3, 0, $4, $5, $5) \
             ON CONFLICT (installation_id, sha256) DO NOTHING",
        )
        .bind(installation)
        .bind(sha256)
        .bind(to_i64(size_bytes))
        // An empty file is complete at once (and checked like any other).
        .bind(false)
        .bind(now)
        .execute(&self.pool)
        .await?;
        let status = self
            .upload(installation, sha256)
            .await?
            .ok_or_else(|| DbError::not_found("upload", sha256))?;
        if status.size_bytes != size_bytes {
            return Err(DbError::Refused(format!(
                "this file was announced as {} bytes before",
                status.size_bytes
            )));
        }
        Ok(status)
    }

    pub async fn upload(&self, installation: Uuid, sha256: &str) -> DbResult<Option<UploadStatus>> {
        sqlx::query(
            "SELECT sha256, size_bytes, received_bytes, complete FROM uploads \
             WHERE installation_id = $1 AND sha256 = $2",
        )
        .bind(installation)
        .bind(sha256)
        .fetch_optional(&self.pool)
        .await?
        .as_ref()
        .map(decode)
        .transpose()
    }

    /// Records that the bytes from `offset` up to `received` are on disk. Refused with
    /// [`DbError::OffsetMismatch`] unless the upload was at `offset`.
    pub async fn advance_upload(
        &self,
        installation: Uuid,
        sha256: &str,
        offset: u64,
        received: u64,
        now: DateTime<Utc>,
    ) -> DbResult<UploadStatus> {
        let row = sqlx::query(
            "UPDATE uploads SET received_bytes = $4, updated_at = $5 \
             WHERE installation_id = $1 AND sha256 = $2 AND received_bytes = $3 \
             AND NOT complete AND $4 <= size_bytes \
             RETURNING sha256, size_bytes, received_bytes, complete",
        )
        .bind(installation)
        .bind(sha256)
        .bind(to_i64(offset))
        .bind(to_i64(received))
        .bind(now)
        .fetch_optional(&self.pool)
        .await?;
        match row {
            Some(row) => decode(&row),
            None => {
                let current = self
                    .upload(installation, sha256)
                    .await?
                    .ok_or_else(|| DbError::not_found("upload", sha256))?;
                Err(DbError::OffsetMismatch {
                    received: current.received_bytes,
                })
            }
        }
    }

    pub async fn complete_upload(
        &self,
        installation: Uuid,
        sha256: &str,
        now: DateTime<Utc>,
    ) -> DbResult<UploadStatus> {
        let row = sqlx::query(
            "UPDATE uploads SET complete = TRUE, updated_at = $3 \
             WHERE installation_id = $1 AND sha256 = $2 AND received_bytes = size_bytes \
             RETURNING sha256, size_bytes, received_bytes, complete",
        )
        .bind(installation)
        .bind(sha256)
        .bind(now)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| DbError::not_found("upload", sha256))?;
        decode(&row)
    }

    /// Starts an upload over (after a checksum mismatch).
    pub async fn reset_upload(
        &self,
        installation: Uuid,
        sha256: &str,
        now: DateTime<Utc>,
    ) -> DbResult<()> {
        sqlx::query(
            "UPDATE uploads SET received_bytes = 0, complete = FALSE, updated_at = $3 \
             WHERE installation_id = $1 AND sha256 = $2",
        )
        .bind(installation)
        .bind(sha256)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Uploads untouched since `before` that no cloud-owned, unpublished publication needs:
    /// published, taken-back and abandoned media. Renders are regenerable on the desktop.
    pub async fn expired_uploads(&self, before: DateTime<Utc>) -> DbResult<Vec<(Uuid, String)>> {
        let rows = sqlx::query(
            "SELECT u.installation_id, u.sha256 FROM uploads u WHERE u.updated_at < $1 \
             AND NOT EXISTS (SELECT 1 FROM publications p \
             WHERE p.installation_id = u.installation_id AND p.media_sha256 = u.sha256 \
             AND p.owner = 'cloud' AND p.state <> 'published')",
        )
        .bind(before)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| Ok((r.try_get("installation_id")?, r.try_get("sha256")?)))
            .collect()
    }

    pub async fn delete_upload(&self, installation: Uuid, sha256: &str) -> DbResult<()> {
        sqlx::query("DELETE FROM uploads WHERE installation_id = $1 AND sha256 = $2")
            .bind(installation)
            .bind(sha256)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
