//! Import and curation.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use shorts_domain::{
    Asset, AssetId, AssetStatus, MediaFile, MediaFileId, PermissionStatus, Rating, RenderRecipe,
    Tag, Variant,
};
use shorts_media::MediaError;
use shorts_store::{AssetFilter, PublicationFilter, StoreError};
use tracing::{info, warn};

use crate::app::App;
use crate::error::{AppError, AppResult};
use crate::events::AppEvent;
use crate::views::{AssetDetail, AssetSummary, MediaView, TagCount};

/// File extensions accepted by drag-drop and the watch folder.
pub const VIDEO_EXTENSIONS: [&str; 6] = ["mp4", "mov", "m4v", "webm", "mkv", "avi"];

pub const DEFAULT_VARIANT_NAME: &str = "Vertical 1080x1920";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportOutcome {
    Imported {
        asset_id: AssetId,
    },
    /// The same bytes are already in the library.
    Duplicate {
        asset_id: AssetId,
    },
}

impl ImportOutcome {
    pub fn asset_id(&self) -> AssetId {
        match self {
            Self::Imported { asset_id } | Self::Duplicate { asset_id } => *asset_id,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryQuery {
    pub text: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub min_rating: Option<u8>,
    pub awaiting_media: Option<bool>,
}

/// Fields left as `None` are unchanged.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetUpdate {
    pub title: Option<String>,
    pub creator: Option<String>,
    pub source_url: Option<String>,
    pub notes: Option<String>,
    /// `Some(0)` clears the rating.
    pub rating: Option<u8>,
    #[serde(default)]
    pub add_tags: Vec<String>,
    #[serde(default)]
    pub remove_tags: Vec<String>,
    pub video_permission: Option<PermissionInput>,
    pub audio_permission: Option<PermissionInput>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionInput {
    pub status: PermissionStatus,
    pub note: Option<String>,
}

pub(crate) fn is_video_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| VIDEO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

impl App {
    /// Imports a video file: checksums it, rejects duplicates, probes it, copies it into the
    /// managed originals folder (read-only), and creates the asset, media file and a default
    /// variant. With `into`, the media is attached to an existing asset awaiting media.
    pub async fn import_file(
        &self,
        path: &Path,
        into: Option<AssetId>,
    ) -> AppResult<ImportOutcome> {
        if !is_video_file(path) {
            return Err(AppError::Invalid(format!(
                "{} is not a supported video file ({})",
                path.display(),
                VIDEO_EXTENSIONS.join(", ")
            )));
        }
        let (checksum, size) = self.media.checksum(path).await?;
        if let Some(existing) = self.store.media_files().find_by_checksum(&checksum).await? {
            return Ok(ImportOutcome::Duplicate {
                asset_id: existing.asset_id,
            });
        }
        let properties = self.media.probe(path).await.map_err(|e| match e {
            MediaError::Failed { .. } | MediaError::Probe(_) => AppError::Invalid(format!(
                "{} could not be read as a video: {e}",
                path.display()
            )),
            other => other.into(),
        })?;

        let now = self.now();
        let (mut asset, new_asset) = match into {
            Some(id) => {
                let asset = self.store.assets().require(id).await?;
                if asset.status != AssetStatus::AwaitingMedia {
                    return Err(AppError::Invalid("this asset already has media".into()));
                }
                (asset, false)
            }
            None => (Asset::imported(&title_from(path), now)?, true),
        };
        asset.mark_media_attached(now);

        let managed = self.copy_original(path, checksum.as_str()).await?;
        let media = MediaFile {
            id: MediaFileId::new(),
            asset_id: asset.id,
            path: managed.to_string_lossy().into_owned(),
            original_name: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            checksum,
            size_bytes: size,
            properties,
            imported_at: now,
        };
        let variant = Variant::new(
            asset.id,
            media.id,
            DEFAULT_VARIANT_NAME,
            RenderRecipe::default(),
            &media.properties,
            now,
        )?;
        match self
            .store
            .save_import(&asset, new_asset, &media, &variant)
            .await
        {
            Ok(()) => {}
            // Lost a race with a concurrent import of the same file.
            Err(StoreError::DuplicateMedia { .. }) => {
                let existing = self
                    .store
                    .media_files()
                    .find_by_checksum(&media.checksum)
                    .await?
                    .ok_or_else(|| AppError::NotFound("duplicate media".into()))?;
                return Ok(ImportOutcome::Duplicate {
                    asset_id: existing.asset_id,
                });
            }
            Err(e) => return Err(e.into()),
        }
        info!(asset = %asset.id, file = %media.original_name, "imported");
        self.emit(AppEvent::Imported {
            asset_id: asset.id.to_string(),
            duplicate: false,
        });
        self.emit(AppEvent::LibraryChanged);
        if !new_asset {
            // A saved link got its media: rules of collections it is already in can fire now.
            self.apply_rules_quietly(asset.id).await;
        }
        Ok(ImportOutcome::Imported { asset_id: asset.id })
    }

    /// A watch-folder import: imports the file, then adds the asset to the collection chosen
    /// in preferences (if any), which runs that collection's rules. Duplicates are routed too;
    /// membership and rule firings are idempotent, so nothing is created twice.
    pub async fn import_watched(&self, path: &Path) -> AppResult<ImportOutcome> {
        let outcome = self.import_file(path, None).await?;
        self.route_watched(outcome.asset_id()).await;
        if let ImportOutcome::Imported { asset_id } = outcome {
            self.suggest_matches_for(asset_id).await;
        }
        Ok(outcome)
    }

    /// Adds a watch-folder import to the routed collection. Failures are reported as events,
    /// never as a failed import.
    pub(crate) async fn route_watched(&self, asset_id: AssetId) {
        let collection = match self.preferences().await {
            Ok(prefs) => prefs.watch_collection,
            Err(e) => {
                warn!(error = %e, "could not read preferences");
                return;
            }
        };
        let Some(collection) = collection else { return };
        match self.add_to_collection(collection, asset_id).await {
            Ok(_) => {}
            Err(AppError::Store(StoreError::NotFound {
                entity: "collection",
                ..
            })) => {
                warn!(%collection, "watch-folder collection is gone; routing turned off");
                if let Err(e) = self.clear_watch_collection().await {
                    warn!(error = %e, "could not turn routing off");
                }
            }
            Err(e) => {
                warn!(asset = %asset_id, error = %e, "watch-folder routing failed");
                self.emit(AppEvent::RulesFailed {
                    asset_id: asset_id.to_string(),
                    message: e.to_string(),
                });
            }
        }
    }

    /// Copies an original into the managed folder as `<checksum>.<ext>` and marks it read-only.
    async fn copy_original(&self, path: &Path, checksum: &str) -> AppResult<PathBuf> {
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_else(|| "mp4".into());
        let dest = self.paths.originals.join(format!("{checksum}.{ext}"));
        if dest.is_file() {
            return Ok(dest);
        }
        tokio::fs::create_dir_all(&self.paths.originals).await?;
        let temp = dest.with_extension(format!("{ext}.partial"));
        tokio::fs::copy(path, &temp).await?;
        let mut perms = tokio::fs::metadata(&temp).await?.permissions();
        perms.set_readonly(true);
        tokio::fs::set_permissions(&temp, perms).await?;
        tokio::fs::rename(&temp, &dest).await?;
        Ok(dest)
    }

    /// Saves a link (e.g. a TikTok URL) as an asset awaiting media.
    pub async fn save_link(&self, url: &str, title: Option<&str>) -> AppResult<AssetId> {
        let url = url.trim();
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return Err(AppError::Invalid("enter a full http(s) link".into()));
        }
        let title = title
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .unwrap_or(url);
        let asset = Asset::from_link(title, url, self.now())?;
        self.store.assets().insert(&asset).await?;
        self.emit(AppEvent::LibraryChanged);
        Ok(asset.id)
    }

    pub async fn list_assets(&self, query: &LibraryQuery) -> AppResult<Vec<AssetSummary>> {
        let tags = query
            .tags
            .iter()
            .map(|t| Tag::parse(t))
            .collect::<Result<Vec<_>, _>>()?;
        let min_rating = query
            .min_rating
            .filter(|r| *r > 0)
            .map(Rating::new)
            .transpose()?;
        let filter = AssetFilter {
            text: query.text.clone(),
            tags,
            min_rating,
            status: query.awaiting_media.map(|w| {
                if w {
                    AssetStatus::AwaitingMedia
                } else {
                    AssetStatus::Ready
                }
            }),
            limit: None,
        };
        let assets = self.store.assets().list(&filter).await?;
        let mut out = Vec::with_capacity(assets.len());
        for asset in assets {
            out.push(self.summary(asset).await?);
        }
        Ok(out)
    }

    pub(crate) async fn summary(&self, asset: Asset) -> AppResult<AssetSummary> {
        let media = self
            .store
            .media_files()
            .for_asset(asset.id)
            .await?
            .into_iter()
            .next()
            .map(|m| MediaView {
                id: m.id.to_string(),
                path: m.path,
                original_name: m.original_name,
                checksum: m.checksum.to_string(),
                size_bytes: m.size_bytes,
                properties: m.properties,
            });
        let publication_count = self
            .store
            .publications()
            .list(&PublicationFilter {
                asset_id: Some(asset.id),
                ..Default::default()
            })
            .await?
            .len();
        Ok(AssetSummary {
            id: asset.id.to_string(),
            title: asset.title,
            creator: asset.creator,
            source_url: asset.source_url,
            notes: asset.notes,
            tags: asset.tags.iter().map(|t| t.to_string()).collect(),
            rating: asset.rating.map(Rating::stars),
            status: asset.status.as_str(),
            video_permission: asset.video_permission,
            audio_permission: asset.audio_permission,
            created_at: asset.created_at,
            media,
            publication_count,
            source_post: self
                .store
                .tiktok_posts()
                .for_asset(asset.id)
                .await?
                .as_ref()
                .map(crate::views::SourcePostView::from),
        })
    }

    pub async fn asset_detail(&self, id: AssetId) -> AppResult<AssetDetail> {
        let asset = self.store.assets().require(id).await?;
        let mut variants = Vec::new();
        for v in self.store.variants().for_asset(id).await? {
            variants.push(self.variant_view(&v).await?);
        }
        let mut publications = Vec::new();
        for p in self
            .store
            .publications()
            .list(&PublicationFilter {
                asset_id: Some(id),
                ..Default::default()
            })
            .await?
        {
            publications.push(self.publication_view(&p).await?);
        }
        Ok(AssetDetail {
            collections: self.asset_collections(id).await?,
            asset: self.summary(asset).await?,
            variants,
            publications,
        })
    }

    pub async fn update_asset(&self, id: AssetId, update: AssetUpdate) -> AppResult<AssetSummary> {
        let mut asset = self.store.assets().require(id).await?;
        let now = self.now();
        if let Some(title) = &update.title {
            asset.rename(title, now)?;
        }
        if let Some(creator) = update.creator {
            asset.creator = Some(creator.trim().to_owned()).filter(|c| !c.is_empty());
        }
        if let Some(url) = update.source_url {
            asset.source_url = Some(url.trim().to_owned()).filter(|c| !c.is_empty());
        }
        if let Some(notes) = update.notes {
            asset.notes = notes;
        }
        if let Some(stars) = update.rating {
            let rating = if stars == 0 {
                None
            } else {
                Some(Rating::new(stars)?)
            };
            asset.rate(rating, now);
        }
        for raw in &update.add_tags {
            asset.add_tag(Tag::parse(raw)?, now);
        }
        for raw in &update.remove_tags {
            asset.remove_tag(&Tag::parse(raw)?, now);
        }
        if let Some(p) = update.video_permission {
            asset.record_video_permission(p.status, p.note, now);
        }
        if let Some(p) = update.audio_permission {
            asset.record_audio_permission(p.status, p.note, now);
        }
        asset.updated_at = now;
        self.store.assets().update(&asset).await?;
        self.emit(AppEvent::LibraryChanged);
        self.summary(asset).await
    }

    pub async fn tag_counts(&self) -> AppResult<Vec<TagCount>> {
        Ok(self
            .store
            .assets()
            .tag_counts()
            .await?
            .into_iter()
            .map(|(tag, count)| TagCount {
                tag: tag.to_string(),
                count,
            })
            .collect())
    }

    /// A poster frame for the asset (generated on first request); for a TikTok post still
    /// waiting for its file, the saved cover; otherwise `None`.
    pub async fn thumbnail(&self, id: AssetId) -> AppResult<Option<PathBuf>> {
        let Some(media) = self
            .store
            .media_files()
            .for_asset(id)
            .await?
            .into_iter()
            .next()
        else {
            let cover = self
                .store
                .tiktok_posts()
                .for_asset(id)
                .await?
                .and_then(|p| p.cover_path)
                .map(PathBuf::from)
                .filter(|p| p.is_file());
            return Ok(cover);
        };
        let at_ms = (media.properties.duration_ms / 10).min(1_000);
        Ok(Some(self.media.thumbnail(&media, at_ms).await?))
    }
}

fn title_from(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let cleaned = stem.replace(['_', '-'], " ");
    let cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if cleaned.is_empty() {
        "Untitled".to_owned()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_come_from_file_names() {
        assert_eq!(
            title_from(Path::new("/x/my_best-clip  v2.mp4")),
            "my best clip v2"
        );
        assert_eq!(title_from(Path::new("/x/.mp4")), ".mp4");
        assert_eq!(title_from(Path::new("___.mov")), "Untitled");
    }

    #[test]
    fn video_extensions_are_case_insensitive() {
        assert!(is_video_file(Path::new("a.MP4")));
        assert!(is_video_file(Path::new("a.mov")));
        assert!(!is_video_file(Path::new("a.txt")));
        assert!(!is_video_file(Path::new("mp4")));
    }
}
