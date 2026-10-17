use shorts_domain::{AssetId, Checksum, MediaFile, MediaFileId};
use sqlx::sqlite::SqliteRow;
use sqlx::{SqliteConnection, SqlitePool};

use crate::codec::{RowExt, is_unique_violation, json, to_i64, ts};
use crate::{StoreError, StoreResult};

pub struct MediaFiles<'a> {
    pool: &'a SqlitePool,
}

const COLUMNS: &str =
    "id, asset_id, path, original_name, checksum, size_bytes, properties, imported_at";

impl<'a> MediaFiles<'a> {
    pub(crate) fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }

    /// Inserts an original. Fails with [`StoreError::DuplicateMedia`] if the same bytes were
    /// imported before. Media file rows can't be updated afterwards.
    /// Inserts an original. Fails with [`StoreError::DuplicateMedia`] if the same bytes were
    /// imported before. Media file rows can't be updated afterwards.
    pub async fn insert(&self, file: &MediaFile) -> StoreResult<()> {
        let mut conn = self.pool.acquire().await?;
        insert_media_file(&mut conn, file).await
    }

    pub async fn get(&self, id: MediaFileId) -> StoreResult<Option<MediaFile>> {
        sqlx::query(&format!("SELECT {COLUMNS} FROM media_files WHERE id = ?"))
            .bind(id.to_string())
            .fetch_optional(self.pool)
            .await?
            .as_ref()
            .map(decode)
            .transpose()
    }

    pub async fn require(&self, id: MediaFileId) -> StoreResult<MediaFile> {
        self.get(id)
            .await?
            .ok_or_else(|| StoreError::not_found("media file", id))
    }

    pub async fn find_by_checksum(&self, checksum: &Checksum) -> StoreResult<Option<MediaFile>> {
        sqlx::query(&format!(
            "SELECT {COLUMNS} FROM media_files WHERE checksum = ?"
        ))
        .bind(checksum.as_str())
        .fetch_optional(self.pool)
        .await?
        .as_ref()
        .map(decode)
        .transpose()
    }

    pub async fn for_asset(&self, asset_id: AssetId) -> StoreResult<Vec<MediaFile>> {
        sqlx::query(&format!(
            "SELECT {COLUMNS} FROM media_files WHERE asset_id = ? ORDER BY imported_at"
        ))
        .bind(asset_id.to_string())
        .fetch_all(self.pool)
        .await?
        .iter()
        .map(decode)
        .collect()
    }
}

pub(crate) async fn insert_media_file(
    conn: &mut SqliteConnection,
    file: &MediaFile,
) -> StoreResult<()> {
    let result = sqlx::query(&format!(
        "INSERT INTO media_files ({COLUMNS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
    ))
    .bind(file.id.to_string())
    .bind(file.asset_id.to_string())
    .bind(&file.path)
    .bind(&file.original_name)
    .bind(file.checksum.as_str())
    .bind(to_i64(file.size_bytes))
    .bind(json(&file.properties))
    .bind(ts(file.imported_at))
    .execute(&mut *conn)
    .await;
    match result {
        Ok(_) => Ok(()),
        Err(e) if is_unique_violation(&e) => Err(StoreError::DuplicateMedia {
            checksum: file.checksum.to_string(),
        }),
        Err(e) => Err(e.into()),
    }
}

fn decode(row: &SqliteRow) -> StoreResult<MediaFile> {
    Ok(MediaFile {
        id: row.parsed("id")?,
        asset_id: row.parsed("asset_id")?,
        path: row.text("path")?,
        original_name: row.text("original_name")?,
        checksum: Checksum::parse(&row.text("checksum")?)
            .map_err(|e| StoreError::Corrupt(e.to_string()))?,
        size_bytes: row.u64("size_bytes")?,
        properties: row.json("properties")?,
        imported_at: row.time("imported_at")?,
    })
}
