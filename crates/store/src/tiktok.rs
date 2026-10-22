use std::collections::HashSet;

use chrono::{DateTime, Utc};
use shorts_domain::{Asset, AssetId, TikTokPost};
use sqlx::sqlite::SqliteRow;
use sqlx::{SqliteConnection, SqlitePool};

use crate::codec::{RowExt, is_unique_violation, opt_ts, to_i64, ts};
use crate::{StoreError, StoreResult};

/// TikTok posts seen through the Display API and declined match suggestions.
pub struct TikTokPosts<'a> {
    pool: &'a SqlitePool,
}

const COLUMNS: &str = "video_id, asset_id, posted_at, duration_ms, title, caption, share_url, \
                       cover_url, cover_path, discovered_at, matched_at";

impl<'a> TikTokPosts<'a> {
    pub(crate) fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn get(&self, video_id: &str) -> StoreResult<Option<TikTokPost>> {
        sqlx::query(&format!(
            "SELECT {COLUMNS} FROM tiktok_posts WHERE video_id = ?"
        ))
        .bind(video_id)
        .fetch_optional(self.pool)
        .await?
        .as_ref()
        .map(decode)
        .transpose()
    }

    pub async fn require(&self, video_id: &str) -> StoreResult<TikTokPost> {
        self.get(video_id)
            .await?
            .ok_or_else(|| StoreError::not_found("TikTok post", video_id))
    }

    pub async fn for_asset(&self, asset: AssetId) -> StoreResult<Option<TikTokPost>> {
        sqlx::query(&format!(
            "SELECT {COLUMNS} FROM tiktok_posts WHERE asset_id = ? ORDER BY posted_at DESC LIMIT 1"
        ))
        .bind(asset.to_string())
        .fetch_optional(self.pool)
        .await?
        .as_ref()
        .map(decode)
        .transpose()
    }

    /// Posts whose item is still waiting for its original file, newest first.
    pub async fn pending(&self) -> StoreResult<Vec<TikTokPost>> {
        sqlx::query(&format!(
            "SELECT {} FROM tiktok_posts p JOIN assets a ON a.id = p.asset_id \
             WHERE a.status = 'awaiting_media' ORDER BY p.posted_at DESC",
            COLUMNS
                .split(", ")
                .map(|c| format!("p.{c}"))
                .collect::<Vec<_>>()
                .join(", ")
        ))
        .fetch_all(self.pool)
        .await?
        .iter()
        .map(decode)
        .collect()
    }

    /// Assets that already belong to a post (so they aren't offered for another one).
    pub async fn linked_assets(&self) -> StoreResult<HashSet<AssetId>> {
        let rows = sqlx::query("SELECT DISTINCT asset_id FROM tiktok_posts")
            .fetch_all(self.pool)
            .await?;
        rows.iter().map(|r| r.parsed("asset_id")).collect()
    }

    /// Records when the post got its original file (attached directly, e.g. from a download).
    pub async fn mark_matched(&self, video_id: &str, at: DateTime<Utc>) -> StoreResult<()> {
        sqlx::query(
            "UPDATE tiktok_posts SET matched_at = ? WHERE video_id = ? AND matched_at IS NULL",
        )
        .bind(ts(at))
        .bind(video_id)
        .execute(self.pool)
        .await?;
        Ok(())
    }

    pub async fn set_cover_path(&self, video_id: &str, path: &str) -> StoreResult<()> {
        sqlx::query("UPDATE tiktok_posts SET cover_path = ? WHERE video_id = ?")
            .bind(path)
            .bind(video_id)
            .execute(self.pool)
            .await?;
        Ok(())
    }

    pub async fn dismiss(
        &self,
        video_id: &str,
        asset: AssetId,
        at: DateTime<Utc>,
    ) -> StoreResult<()> {
        sqlx::query(
            "INSERT INTO tiktok_match_dismissals (video_id, asset_id, dismissed_at) \
             VALUES (?, ?, ?) ON CONFLICT DO NOTHING",
        )
        .bind(video_id)
        .bind(asset.to_string())
        .bind(ts(at))
        .execute(self.pool)
        .await?;
        Ok(())
    }

    pub async fn dismissed(&self) -> StoreResult<HashSet<(String, AssetId)>> {
        let rows = sqlx::query("SELECT video_id, asset_id FROM tiktok_match_dismissals")
            .fetch_all(self.pool)
            .await?;
        rows.iter()
            .map(|r| Ok((r.text("video_id")?, r.parsed("asset_id")?)))
            .collect()
    }
}

pub(crate) async fn insert_post(conn: &mut SqliteConnection, p: &TikTokPost) -> StoreResult<bool> {
    let result = sqlx::query(&format!(
        "INSERT INTO tiktok_posts ({COLUMNS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
    ))
    .bind(&p.video_id)
    .bind(p.asset_id.to_string())
    .bind(ts(p.posted_at))
    .bind(p.duration_ms.map(to_i64))
    .bind(&p.title)
    .bind(&p.caption)
    .bind(&p.share_url)
    .bind(&p.cover_url)
    .bind(&p.cover_path)
    .bind(ts(p.discovered_at))
    .bind(opt_ts(p.matched_at))
    .execute(&mut *conn)
    .await;
    match result {
        Ok(_) => Ok(true),
        Err(e) if is_unique_violation(&e) => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Merges a confirmed match: `target` (the imported asset, already updated with the post's
/// details) takes over the post and the awaiting item's collections, and the awaiting item is
/// deleted. Refuses if the awaiting item has media or publications.
pub(crate) async fn merge(
    conn: &mut SqliteConnection,
    video_id: &str,
    awaiting: AssetId,
    target: &Asset,
    at: DateTime<Utc>,
) -> StoreResult<()> {
    crate::assets::update_asset(conn, target).await?;
    sqlx::query(
        "INSERT INTO collection_members (collection_id, asset_id, added_at) \
         SELECT collection_id, ?, ? FROM collection_members WHERE asset_id = ? \
         ON CONFLICT DO NOTHING",
    )
    .bind(target.id.to_string())
    .bind(ts(at))
    .bind(awaiting.to_string())
    .execute(&mut *conn)
    .await?;
    let moved = sqlx::query(
        "UPDATE tiktok_posts SET asset_id = ?, matched_at = ? WHERE video_id = ? AND asset_id = ?",
    )
    .bind(target.id.to_string())
    .bind(ts(at))
    .bind(video_id)
    .bind(awaiting.to_string())
    .execute(&mut *conn)
    .await?;
    if moved.rows_affected() != 1 {
        return Err(StoreError::not_found("TikTok post", video_id));
    }
    let deleted = sqlx::query(
        "DELETE FROM assets WHERE id = ? AND status = 'awaiting_media' \
         AND NOT EXISTS (SELECT 1 FROM media_files m WHERE m.asset_id = assets.id) \
         AND NOT EXISTS (SELECT 1 FROM publications p WHERE p.asset_id = assets.id)",
    )
    .bind(awaiting.to_string())
    .execute(&mut *conn)
    .await?;
    if deleted.rows_affected() != 1 {
        return Err(StoreError::Corrupt(
            "the TikTok item already has media or publications".into(),
        ));
    }
    Ok(())
}

fn decode(row: &SqliteRow) -> StoreResult<TikTokPost> {
    Ok(TikTokPost {
        video_id: row.text("video_id")?,
        asset_id: row.parsed("asset_id")?,
        posted_at: row.time("posted_at")?,
        duration_ms: row
            .opt_int("duration_ms")?
            .map(|v| u64::try_from(v).map_err(|_| StoreError::Corrupt(format!("bad duration {v}"))))
            .transpose()?,
        title: row.text("title")?,
        caption: row.text("caption")?,
        share_url: row.opt_text("share_url")?,
        cover_url: row.opt_text("cover_url")?,
        cover_path: row.opt_text("cover_path")?,
        discovered_at: row.time("discovered_at")?,
        matched_at: row.opt_time("matched_at")?,
    })
}
