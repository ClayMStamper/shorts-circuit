//! Originals downloaded from TikTok.
//!
//! TikTok lets apps read a creator's post list but never hands over the video files, while its
//! own web player offers "Download video" for the creator's posts. The owner keeps that click;
//! the app does the rest: "Get original" opens the post and remembers that its video is on the
//! way, and a video that then appears in the downloads folder is attached to the post's waiting
//! item (its caption, link and workflows). Only a video that fits a pending post is imported;
//! everything else in the downloads folder is left alone. "Pick file…" attaches a file by hand.

use std::path::Path;

use chrono::{DateTime, Duration, Utc};
use shorts_domain::source::{EXPECT_ORIGINAL_MINUTES, MATCH_DURATION_TOLERANCE_MS};
use shorts_domain::{AssetId, AssetStatus, PublicationId, TikTokPost, pick_post_for_download};
use tracing::{debug, info, warn};

use crate::ImportOutcome;
use crate::app::App;
use crate::error::{AppError, AppResult};
use crate::events::AppEvent;
use crate::library::is_video_file;

fn seconds(ms: u64) -> String {
    format!("{:.0} s", ms as f64 / 1000.0)
}

impl App {
    fn expect_window() -> Duration {
        Duration::minutes(EXPECT_ORIGINAL_MINUTES)
    }

    /// Until when a download is expected for this post ("Get original"), if it still is.
    pub(crate) fn expected_until(&self, video_id: &str) -> Option<DateTime<Utc>> {
        let now = self.now();
        self.expected_originals
            .lock()
            .expect("expected lock")
            .get(video_id)
            .map(|at| *at + Self::expect_window())
            .filter(|until| *until > now)
    }

    pub(crate) fn download_note(&self, video_id: &str) -> Option<String> {
        self.download_notes
            .lock()
            .expect("notes lock")
            .get(video_id)
            .cloned()
    }

    fn forget_expectation(&self, video_id: &str) {
        self.expected_originals
            .lock()
            .expect("expected lock")
            .remove(video_id);
        self.download_notes
            .lock()
            .expect("notes lock")
            .remove(video_id);
    }

    async fn waiting_post(&self, asset_id: AssetId) -> AppResult<(String, Option<TikTokPost>)> {
        let asset = self.store.assets().require(asset_id).await?;
        if asset.status != AssetStatus::AwaitingMedia {
            return Err(AppError::Invalid("this item already has its video".into()));
        }
        let post = self.store.tiktok_posts().for_asset(asset_id).await?;
        Ok((asset.title, post))
    }

    /// "Get original": the owner is about to download this TikTok post's video. Returns the
    /// post's link to open; a matching video arriving in the downloads folder in the next
    /// half hour is attached to this post first.
    pub async fn expect_original(&self, asset_id: AssetId) -> AppResult<String> {
        let asset = self.store.assets().require(asset_id).await?;
        let (_, post) = self.waiting_post(asset_id).await?;
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
            expected.retain(|_, at| now - *at <= Self::expect_window());
            expected.insert(post.video_id.clone(), now);
            drop(expected);
            self.download_notes
                .lock()
                .expect("notes lock")
                .remove(&post.video_id);
            self.emit(AppEvent::LibraryChanged);
        }
        Ok(url)
    }

    /// Stops expecting a download for this item.
    pub async fn cancel_expect_original(&self, asset_id: AssetId) -> AppResult<()> {
        if let Some(post) = self.store.tiktok_posts().for_asset(asset_id).await? {
            self.forget_expectation(&post.video_id);
            self.emit(AppEvent::LibraryChanged);
        }
        Ok(())
    }

    /// "Pick file…": attaches a video the owner chose to a waiting item.
    pub async fn attach_original(&self, asset_id: AssetId, path: &Path) -> AppResult<AssetId> {
        let (title, post) = self.waiting_post(asset_id).await?;
        match self.import_file(path, Some(asset_id)).await? {
            ImportOutcome::Imported { asset_id } => {
                if let Some(post) = post {
                    self.finish_match(&post).await;
                }
                // Like a watch-folder file: into the workflows' collection, whose rules run.
                self.route_watched(asset_id).await;
                info!(asset = %asset_id, "original attached by hand");
                self.emit(AppEvent::OriginalAttached {
                    asset_id: asset_id.to_string(),
                    title,
                });
                Ok(asset_id)
            }
            ImportOutcome::Duplicate { .. } => Err(AppError::Invalid(
                "that video is already in the library as another item".into(),
            )),
        }
    }

    /// "Send through workflows": puts an item that has its video into the collection the watch
    /// folder feeds, so its workflows run (as for a watch-folder file). Safe to repeat: rules
    /// never create the same publication twice. Returns the publications created.
    pub async fn send_to_workflows(&self, asset_id: AssetId) -> AppResult<Vec<PublicationId>> {
        let asset = self.store.assets().require(asset_id).await?;
        if asset.status == AssetStatus::AwaitingMedia {
            return Err(AppError::Invalid(
                "this item doesn't have its video yet".into(),
            ));
        }
        let Some(collection) = self.preferences().await?.watch_collection else {
            return Err(AppError::Invalid(
                "no workflow receives videos yet; create a workflow first".into(),
            ));
        };
        self.add_to_collection(collection, asset_id).await
    }

    async fn finish_match(&self, post: &TikTokPost) {
        if let Err(e) = self
            .store
            .tiktok_posts()
            .mark_matched(&post.video_id, self.now())
            .await
        {
            warn!(error = %e, "could not record the TikTok match");
        }
        self.forget_expectation(&post.video_id);
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
            self.note_unfit_download(path, properties.duration_ms, &pending, &expected, now);
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
        self.finish_match(&post).await;
        // Like a watch-folder file: into the workflows' collection, whose rules run.
        self.route_watched(asset_id).await;
        info!(asset = %asset_id, "downloaded original attached to its TikTok post");
        self.emit(AppEvent::OriginalAttached {
            asset_id: asset_id.to_string(),
            title: post.title.clone(),
        });
        self.emit(AppEvent::LibraryChanged);
        Ok(Some(asset_id))
    }

    /// While a post is being waited for, says on it why a download wasn't taken.
    fn note_unfit_download(
        &self,
        path: &Path,
        duration_ms: u64,
        pending: &[TikTokPost],
        expected: &[(String, DateTime<Utc>)],
        now: DateTime<Utc>,
    ) {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut noted = false;
        for post in pending {
            let waiting = expected
                .iter()
                .any(|(id, at)| *id == post.video_id && now - *at <= Self::expect_window());
            let Some(post_ms) = post.duration_ms else {
                continue;
            };
            if !waiting || post_ms.abs_diff(duration_ms) <= MATCH_DURATION_TOLERANCE_MS {
                continue;
            }
            self.download_notes.lock().expect("notes lock").insert(
                post.video_id.clone(),
                format!(
                    "Saw “{name}” in your downloads, but it's {} and this post is {}. Use Pick \
                     file… if it's the right video.",
                    seconds(duration_ms),
                    seconds(post_ms)
                ),
            );
            noted = true;
        }
        if noted {
            self.emit(AppEvent::LibraryChanged);
        }
    }
}
