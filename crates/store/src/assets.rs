use std::collections::BTreeSet;

use shorts_domain::{Asset, AssetId, AssetStatus, Rating, Tag};
use sqlx::sqlite::SqliteRow;
use sqlx::{SqliteConnection, SqlitePool};

use crate::codec::{RowExt, json, ts};
use crate::{StoreError, StoreResult};

/// Library search criteria. Empty fields match everything.
#[derive(Debug, Clone, Default)]
pub struct AssetFilter {
    /// Matches title, creator, notes, source URL and tags (case-insensitive substring).
    pub text: Option<String>,
    /// All of these tags must be present.
    pub tags: Vec<Tag>,
    pub min_rating: Option<Rating>,
    pub status: Option<AssetStatus>,
    pub limit: Option<u32>,
}

pub struct Assets<'a> {
    pool: &'a SqlitePool,
}

const COLUMNS: &str = "id, title, creator, source_url, notes, rating, video_permission, \
                       audio_permission, status, created_at, updated_at";

impl<'a> Assets<'a> {
    pub(crate) fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn insert(&self, asset: &Asset) -> StoreResult<()> {
        let mut tx = self.pool.begin().await?;
        insert_asset(&mut tx, asset).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Replaces every mutable field and the tag set.
    pub async fn update(&self, asset: &Asset) -> StoreResult<()> {
        let mut tx = self.pool.begin().await?;
        update_asset(&mut tx, asset).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn get(&self, id: AssetId) -> StoreResult<Option<Asset>> {
        let row = sqlx::query(&format!("SELECT {COLUMNS} FROM assets WHERE id = ?"))
            .bind(id.to_string())
            .fetch_optional(self.pool)
            .await?;
        match row {
            Some(row) => {
                let tags = self.tags_of(id).await?;
                Ok(Some(decode(&row, tags)?))
            }
            None => Ok(None),
        }
    }

    pub async fn require(&self, id: AssetId) -> StoreResult<Asset> {
        self.get(id)
            .await?
            .ok_or_else(|| StoreError::not_found("asset", id))
    }

    /// Newest first.
    pub async fn list(&self, filter: &AssetFilter) -> StoreResult<Vec<Asset>> {
        let mut sql = format!("SELECT {COLUMNS} FROM assets a WHERE 1 = 1");
        let mut binds: Vec<String> = Vec::new();
        if let Some(text) = filter
            .text
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            let pattern = format!(
                "%{}%",
                text.trim_start_matches('#')
                    .to_lowercase()
                    .replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_")
            );
            sql.push_str(
                " AND (lower(a.title) LIKE ?1 ESCAPE '\\' OR lower(coalesce(a.creator, '')) LIKE ?1 ESCAPE '\\' \
                 OR lower(a.notes) LIKE ?1 ESCAPE '\\' OR lower(coalesce(a.source_url, '')) LIKE ?1 ESCAPE '\\' \
                 OR EXISTS (SELECT 1 FROM asset_tags t WHERE t.asset_id = a.id AND t.tag LIKE ?1 ESCAPE '\\'))",
            );
            binds.push(pattern);
        }
        for tag in &filter.tags {
            binds.push(tag.as_str().to_owned());
            sql.push_str(&format!(
                " AND EXISTS (SELECT 1 FROM asset_tags t WHERE t.asset_id = a.id AND t.tag = ?{})",
                binds.len()
            ));
        }
        if let Some(rating) = filter.min_rating {
            sql.push_str(&format!(" AND a.rating >= {}", rating.stars()));
        }
        if let Some(status) = filter.status {
            binds.push(status.as_str().to_owned());
            sql.push_str(&format!(" AND a.status = ?{}", binds.len()));
        }
        sql.push_str(" ORDER BY a.created_at DESC, a.id DESC");
        if let Some(limit) = filter.limit {
            sql.push_str(&format!(" LIMIT {limit}"));
        }
        let mut query = sqlx::query(&sql);
        for b in &binds {
            query = query.bind(b);
        }
        let rows = query.fetch_all(self.pool).await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let id: AssetId = row.parsed("id")?;
            let tags = self.tags_of(id).await?;
            out.push(decode(&row, tags)?);
        }
        Ok(out)
    }

    /// Every tag in use with the number of assets carrying it, most used first.
    pub async fn tag_counts(&self) -> StoreResult<Vec<(Tag, u32)>> {
        let rows = sqlx::query(
            "SELECT tag, COUNT(*) AS n FROM asset_tags GROUP BY tag ORDER BY n DESC, tag",
        )
        .fetch_all(self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                let tag =
                    Tag::parse(&r.text("tag")?).map_err(|e| StoreError::Corrupt(e.to_string()))?;
                Ok((tag, r.u32("n")?))
            })
            .collect()
    }

    async fn tags_of(&self, id: AssetId) -> StoreResult<BTreeSet<Tag>> {
        let rows = sqlx::query("SELECT tag FROM asset_tags WHERE asset_id = ?")
            .bind(id.to_string())
            .fetch_all(self.pool)
            .await?;
        rows.iter()
            .map(|r| Tag::parse(&r.text("tag")?).map_err(|e| StoreError::Corrupt(e.to_string())))
            .collect()
    }
}

pub(crate) async fn insert_asset(conn: &mut SqliteConnection, asset: &Asset) -> StoreResult<()> {
    sqlx::query(&format!(
        "INSERT INTO assets ({COLUMNS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
    ))
    .bind(asset.id.to_string())
    .bind(&asset.title)
    .bind(&asset.creator)
    .bind(&asset.source_url)
    .bind(&asset.notes)
    .bind(asset.rating.map(|r| i64::from(r.stars())))
    .bind(json(&asset.video_permission))
    .bind(json(&asset.audio_permission))
    .bind(asset.status.as_str())
    .bind(ts(asset.created_at))
    .bind(ts(asset.updated_at))
    .execute(&mut *conn)
    .await?;
    write_tags(conn, asset).await
}

pub(crate) async fn update_asset(conn: &mut SqliteConnection, asset: &Asset) -> StoreResult<()> {
    let result = sqlx::query(
        "UPDATE assets SET title = ?, creator = ?, source_url = ?, notes = ?, rating = ?, \
         video_permission = ?, audio_permission = ?, status = ?, updated_at = ? WHERE id = ?",
    )
    .bind(&asset.title)
    .bind(&asset.creator)
    .bind(&asset.source_url)
    .bind(&asset.notes)
    .bind(asset.rating.map(|r| i64::from(r.stars())))
    .bind(json(&asset.video_permission))
    .bind(json(&asset.audio_permission))
    .bind(asset.status.as_str())
    .bind(ts(asset.updated_at))
    .bind(asset.id.to_string())
    .execute(&mut *conn)
    .await?;
    if result.rows_affected() == 0 {
        return Err(StoreError::not_found("asset", asset.id));
    }
    sqlx::query("DELETE FROM asset_tags WHERE asset_id = ?")
        .bind(asset.id.to_string())
        .execute(&mut *conn)
        .await?;
    write_tags(conn, asset).await
}

async fn write_tags(conn: &mut SqliteConnection, asset: &Asset) -> StoreResult<()> {
    for tag in &asset.tags {
        sqlx::query("INSERT INTO asset_tags (asset_id, tag) VALUES (?, ?)")
            .bind(asset.id.to_string())
            .bind(tag.as_str())
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

fn decode(row: &SqliteRow, tags: BTreeSet<Tag>) -> StoreResult<Asset> {
    let rating = row
        .opt_int("rating")?
        .map(|r| {
            u8::try_from(r)
                .ok()
                .and_then(|r| Rating::new(r).ok())
                .ok_or_else(|| StoreError::Corrupt(format!("bad rating {r}")))
        })
        .transpose()?;
    let status = row.text("status")?;
    Ok(Asset {
        id: row.parsed("id")?,
        title: row.text("title")?,
        creator: row.opt_text("creator")?,
        source_url: row.opt_text("source_url")?,
        notes: row.text("notes")?,
        tags,
        rating,
        video_permission: row.json("video_permission")?,
        audio_permission: row.json("audio_permission")?,
        status: AssetStatus::parse(&status)
            .ok_or_else(|| StoreError::Corrupt(format!("bad asset status {status:?}")))?,
        created_at: row.time("created_at")?,
        updated_at: row.time("updated_at")?,
    })
}
