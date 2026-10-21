//! Originals downloaded from TikTok.
//!
//! TikTok lets apps read a creator's post list but never hands over the video files, while its
//! own web player offers "Download video" for the creator's posts. The owner keeps that click;
//! the app does the rest: "Get original" opens the post and remembers that its video is on the
//! way, and a video that then appears in the downloads folder is attached to the post's waiting
//! item (its caption, link and workflows). Only a video that fits a pending post is imported;
//! everything else in the downloads folder is left alone.

use std::path::Path;

use chrono::Duration;
use shorts_domain::source::EXPECT_ORIGINAL_MINUTES;
use shorts_domain::{AssetId, AssetStatus, pick_post_for_download};
use tracing::{debug, info, warn};

use crate::ImportOutcome;
use crate::app::App;
use crate::error::{AppError, AppResult};
use crate::events::AppEvent;
use crate::library::is_video_file;

impl App {
    /// "Get original": the owner is about to download this TikTok post's video. Returns the
    /// post's link to open; a matching video arriving in the downloads folder in the next
    /// half hour is attached to this post first.
    pub async fn expect_original(&self, asset_id: AssetId) -> AppResult<String> {
        let asset = self.store.assets().require(asset_id).await?;
        if asset.status != AssetStatus::AwaitingMedia {
            return Err(AppError::Invalid("this item already has its video".into()));
        }
        let post = self.store.tiktok_posts().for_asset(asset_id).await?;
        let url = post
            .as_ref()
            .and_then(|p| p.share_url.clone())
            .or_else(|| asset.source_url.clone())
            .filter(|u| u.starts_with("https://"))
            .ok_or_else(|| {
                AppError::Invalid("this item has no link to its original post".into())
            })?;
        if let Some(post) = post {
            let now = self.now();
            let mut expected = self.expected_originals.lock().expect("expected lock");
            expected.retain(|_, at| now - *at <= Duration::minutes(EXPECT_ORIGINAL_MINUTES));
            expected.insert(post.video_id, now);
        }
        Ok(url)
    }

    /// Considers a video that appeared in the downloads folder. Returns the item it was
    /// attached to, or `None` when it isn't the original of any pending TikTok post (it is then
    /// left alone, not imported).
    pub async fn import_download(&self, path: &Path) -> AppResult<Option<AssetId>> {
        if !is_video_file(path) {
            return Ok(None);
        }
        let pending = self.store.tiktok_posts().pending().await?;
        if pending.is_empty() {
            return Ok(None);
        }
        let Ok(properties) = self.media.probe(path).await else {
            debug!(path = %path.display(), "download isn't a readable video; left alone");
            return Ok(None);
        };
        let now = self.now();
        let expected: Vec<_> = self
            .expected_originals
            .lock()
            .expect("expected lock")
            .iter()
            .map(|(id, at)| (id.clone(), *at))
            .collect();
        let Some(post) =
            pick_post_for_download(&pending, &expected, properties.duration_ms, now).cloned()
        else {
            debug!(path = %path.display(), "download doesn't fit a pending TikTok post; left alone");
            return Ok(None);
        };
        let asset_id = match self.import_file(path, Some(post.asset_id)).await? {
            ImportOutcome::Imported { asset_id } => asset_id,
            // Already in the library (e.g. also saved to the watch folder): match that item.
            ImportOutcome::Duplicate { asset_id } => {
                if let Err(e) = self.confirm_match(&post.video_id, asset_id).await {
                    warn!(error = %e, "downloaded video is already in the library and couldn't be matched");
                    return Ok(None);
                }
                asset_id
            }
        };
        if let Err(e) = self
            .store
            .tiktok_posts()
            .mark_matched(&post.video_id, now)
            .await
        {
            warn!(error = %e, "could not record the TikTok match");
        }
        self.expected_originals
            .lock()
            .expect("expected lock")
            .remove(&post.video_id);
        info!(asset = %asset_id, "downloaded original attached to its TikTok post");
        self.emit(AppEvent::LibraryChanged);
        Ok(Some(asset_id))
    }
}
