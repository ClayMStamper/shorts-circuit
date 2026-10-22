//! TikTok posts in the library and matching them with watch-folder files.
//!
//! A detected post is an item awaiting media. When an imported file has about the same
//! duration and arrived near the post time, the pair is offered as a suggestion; the user
//! confirms (the file's asset takes over the post's details and collections) or dismisses it.

use std::collections::HashMap;

use chrono::{Duration, Utc};
use shorts_adapters::tiktok::TikTokVideo;
use shorts_domain::source::MATCH_WINDOW_HOURS;
use shorts_domain::{Asset, AssetId, AssetStatus, MatchScore, TikTokPost, match_score, post_title};
use tracing::info;

use crate::app::App;
use crate::error::{AppError, AppResult};
use crate::events::AppEvent;
use crate::views::{AssetSummary, MatchCandidateView, MatchSuggestion, SourcePostView};

/// Suggestions offered per pending post.
const CANDIDATES_PER_POST: usize = 3;

impl From<&TikTokPost> for SourcePostView {
    fn from(p: &TikTokPost) -> Self {
        Self {
            video_id: p.video_id.clone(),
            title: p.title.clone(),
            caption: p.caption.clone(),
            posted_at: p.posted_at,
            duration_ms: p.duration_ms,
            share_url: p.share_url.clone(),
            cover_path: p.cover_path.clone(),
            matched: p.matched_at.is_some(),
        }
    }
}

impl App {
    /// Adds a newly noticed post as an item awaiting its original file. Returns `None` when the
    /// post was already known.
    pub(crate) async fn record_tiktok_post(
        &self,
        video: &TikTokVideo,
        creator: &str,
    ) -> AppResult<Option<AssetId>> {
        if video.id.is_empty() || video.id.len() > 64 {
            return Err(AppError::Invalid(
                "TikTok returned a post without an id".into(),
            ));
        }
        if self.store.tiktok_posts().get(&video.id).await?.is_some() {
            return Ok(None);
        }
        let now = self.now();
        let posted_at = chrono::DateTime::from_timestamp(video.create_time, 0).unwrap_or(now);
        let title = post_title(
            video.title.as_deref(),
            video.video_description.as_deref(),
            posted_at,
        );
        let caption = video.video_description.clone().unwrap_or_default();
        let mut asset = Asset::awaiting_media(&title, video.share_url.as_deref(), now)?;
        asset.creator = Some(creator.trim().to_owned()).filter(|c| !c.is_empty());
        asset.notes.clone_from(&caption);
        let post = TikTokPost {
            video_id: video.id.clone(),
            asset_id: asset.id,
            posted_at,
            duration_ms: video.duration.map(|s| u64::from(s) * 1_000),
            title,
            caption,
            share_url: video.share_url.clone(),
            cover_url: video.cover_image_url.clone(),
            cover_path: None,
            discovered_at: now,
            matched_at: None,
        };
        if !self.store.record_tiktok_post(&asset, &post).await? {
            return Ok(None);
        }
        info!(asset = %asset.id, "new TikTok post added as awaiting media");
        self.emit(AppEvent::LibraryChanged);
        Ok(Some(asset.id))
    }

    /// Files that could be the original of a pending TikTok post, best first per post.
    pub async fn match_suggestions(&self) -> AppResult<Vec<MatchSuggestion>> {
        let pending = self.store.tiktok_posts().pending().await?;
        if pending.is_empty() {
            return Ok(Vec::new());
        }
        let linked = self.store.tiktok_posts().linked_assets().await?;
        let dismissed = self.store.tiktok_posts().dismissed().await?;
        let window = Duration::hours(MATCH_WINDOW_HOURS);
        let earliest = pending
            .iter()
            .map(|p| p.posted_at)
            .min()
            .unwrap_or_else(Utc::now);
        let latest = pending
            .iter()
            .map(|p| p.posted_at)
            .max()
            .unwrap_or_else(Utc::now);
        let files = self
            .store
            .media_files()
            .imported_between(earliest - window, latest + window)
            .await?;
        let mut assets: HashMap<AssetId, Asset> = HashMap::new();
        let mut out = Vec::new();
        for post in &pending {
            let mut scored: Vec<(MatchScore, &shorts_domain::MediaFile)> = files
                .iter()
                .filter(|f| !linked.contains(&f.asset_id))
                .filter(|f| !dismissed.contains(&(post.video_id.clone(), f.asset_id)))
                .filter_map(|f| {
                    match_score(
                        post.posted_at,
                        post.duration_ms,
                        f.imported_at,
                        f.properties.duration_ms,
                    )
                    .map(|s| (s, f))
                })
                .collect();
            scored.sort_by_key(|(s, _)| *s);
            for (score, file) in scored.into_iter().take(CANDIDATES_PER_POST) {
                let asset = match assets.get(&file.asset_id) {
                    Some(a) => a.clone(),
                    None => {
                        let a = self.store.assets().require(file.asset_id).await?;
                        assets.insert(a.id, a.clone());
                        a
                    }
                };
                out.push(MatchSuggestion {
                    post_asset_id: post.asset_id.to_string(),
                    post: SourcePostView::from(post),
                    candidate: MatchCandidateView {
                        asset_id: asset.id.to_string(),
                        title: asset.title,
                        original_name: file.original_name.clone(),
                        imported_at: file.imported_at,
                        duration_ms: file.properties.duration_ms,
                    },
                    duration_diff_ms: score.duration_diff_ms,
                    hours_apart: score.seconds_apart as f64 / 3600.0,
                });
            }
        }
        Ok(out)
    }

    /// Tells the UI when a newly imported file could be a pending TikTok post.
    pub(crate) async fn suggest_matches_for(&self, asset_id: AssetId) {
        match self.match_suggestions().await {
            Ok(s)
                if s.iter()
                    .any(|m| m.candidate.asset_id == asset_id.to_string()) =>
            {
                self.emit(AppEvent::MatchSuggested {
                    asset_id: Some(asset_id.to_string()),
                });
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "could not compute match suggestions"),
        }
    }

    /// The user confirmed that `asset_id` holds the original of the TikTok post. The asset
    /// takes the post's title, caption, link and collections (whose rules then run), and the
    /// awaiting item is removed.
    pub async fn confirm_match(
        &self,
        video_id: &str,
        asset_id: AssetId,
    ) -> AppResult<AssetSummary> {
        let post = self.store.tiktok_posts().require(video_id).await?;
        let awaiting = self.store.assets().require(post.asset_id).await?;
        if awaiting.status != AssetStatus::AwaitingMedia {
            return Err(AppError::Invalid(
                "this TikTok post is already matched".into(),
            ));
        }
        let mut target = self.store.assets().require(asset_id).await?;
        if target.status != AssetStatus::Ready {
            return Err(AppError::Invalid(
                "pick an item that has a video file".into(),
            ));
        }
        if self
            .store
            .tiktok_posts()
            .for_asset(asset_id)
            .await?
            .is_some()
        {
            return Err(AppError::Invalid(
                "that file is already matched to another TikTok post".into(),
            ));
        }
        let now = self.now();
        target.rename(&awaiting.title, now)?;
        if target.source_url.is_none() {
            target.source_url.clone_from(&awaiting.source_url);
        }
        if target.creator.is_none() {
            target.creator.clone_from(&awaiting.creator);
        }
        // The post's caption wins over a sidecar or a note: it is what went out on TikTok.
        if target.notes.trim().is_empty() || !awaiting.notes.trim().is_empty() {
            target.notes.clone_from(&awaiting.notes);
        }
        for tag in &awaiting.tags {
            target.add_tag(tag.clone(), now);
        }
        if target.rating.is_none() {
            target.rating = awaiting.rating;
        }
        target.updated_at = now;
        self.store
            .merge_tiktok_match(video_id, awaiting.id, &target, now)
            .await?;
        info!(asset = %asset_id, "TikTok post matched to its original file");
        self.emit(AppEvent::LibraryChanged);
        self.emit(AppEvent::CollectionsChanged);
        // It may have joined collections with rules.
        self.apply_rules_quietly(asset_id).await;
        self.summary(self.store.assets().require(asset_id).await?)
            .await
    }

    /// Confirms matches nobody needs to choose between: a pending post with exactly one
    /// candidate file that is a candidate for no other post. Only once a workflow exists (the
    /// workflow flow has no one to confirm them); otherwise suggestions wait for the user.
    pub(crate) async fn auto_match(&self) {
        match self.store.workflows().list().await {
            Ok(w) if !w.is_empty() => {}
            _ => return,
        }
        let suggestions = match self.match_suggestions().await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "could not compute match suggestions");
                return;
            }
        };
        let mut per_post: HashMap<&str, usize> = HashMap::new();
        let mut per_file: HashMap<&str, usize> = HashMap::new();
        for s in &suggestions {
            *per_post.entry(&s.post.video_id).or_default() += 1;
            *per_file.entry(&s.candidate.asset_id).or_default() += 1;
        }
        for s in &suggestions {
            if per_post[s.post.video_id.as_str()] != 1
                || per_file[s.candidate.asset_id.as_str()] != 1
            {
                continue;
            }
            let Ok(asset_id) = s.candidate.asset_id.parse::<AssetId>() else {
                continue;
            };
            match self.confirm_match(&s.post.video_id, asset_id).await {
                Ok(_) => info!(asset = %asset_id, "matched to its TikTok post automatically"),
                Err(e) => tracing::warn!(asset = %asset_id, error = %e, "automatic match failed"),
            }
        }
    }

    pub async fn dismiss_match(&self, video_id: &str, asset_id: AssetId) -> AppResult<()> {
        self.store.tiktok_posts().require(video_id).await?;
        self.store
            .tiktok_posts()
            .dismiss(video_id, asset_id, self.now())
            .await?;
        self.emit(AppEvent::LibraryChanged);
        Ok(())
    }
}
