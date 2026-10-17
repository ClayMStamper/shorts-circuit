//! Assets: the logical source video, its provenance, tags, rating and permission records.

use std::collections::BTreeSet;
use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ids::AssetId;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum AssetError {
    #[error("tag must be 1-64 characters of letters, digits, '-' or '_' (got {0:?})")]
    InvalidTag(String),
    #[error("rating must be between 1 and 5 (got {0})")]
    InvalidRating(u8),
    #[error("title must not be empty")]
    EmptyTitle,
}

/// A normalized library tag: lowercase, no leading `#`, letters/digits/`-`/`_` only.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Tag(String);

impl Tag {
    pub fn parse(raw: &str) -> Result<Self, AssetError> {
        let normalized = raw.trim().trim_start_matches('#').to_lowercase();
        let valid = !normalized.is_empty()
            && normalized.chars().count() <= 64
            && normalized
                .chars()
                .all(|c| c.is_alphanumeric() || c == '-' || c == '_');
        if valid {
            Ok(Self(normalized))
        } else {
            Err(AssetError::InvalidTag(raw.to_owned()))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Tag {
    type Error = AssetError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<Tag> for String {
    fn from(tag: Tag) -> Self {
        tag.0
    }
}

impl fmt::Display for Tag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A 1-5 star rating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct Rating(u8);

impl Rating {
    pub fn new(stars: u8) -> Result<Self, AssetError> {
        if (1..=5).contains(&stars) {
            Ok(Self(stars))
        } else {
            Err(AssetError::InvalidRating(stars))
        }
    }

    pub fn stars(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for Rating {
    type Error = AssetError;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Rating> for u8 {
    fn from(rating: Rating) -> Self {
        rating.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionStatus {
    /// Nobody has recorded a decision yet.
    #[default]
    Unknown,
    Granted,
    Denied,
}

/// A permission decision with an optional note (who granted it, licence terms, ...).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Permission {
    pub status: PermissionStatus,
    pub note: Option<String>,
    pub recorded_at: Option<DateTime<Utc>>,
}

/// Whether an asset has media yet. A saved TikTok link starts as `AwaitingMedia`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetStatus {
    AwaitingMedia,
    Ready,
}

impl AssetStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AwaitingMedia => "awaiting_media",
            Self::Ready => "ready",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "awaiting_media" => Some(Self::AwaitingMedia),
            "ready" => Some(Self::Ready),
            _ => None,
        }
    }
}

/// A logical source video. Video and audio permissions are tracked separately because
/// platform sounds are often licensed for the origin platform only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asset {
    pub id: AssetId,
    pub title: String,
    pub creator: Option<String>,
    pub source_url: Option<String>,
    pub notes: String,
    pub tags: BTreeSet<Tag>,
    pub rating: Option<Rating>,
    pub video_permission: Permission,
    pub audio_permission: Permission,
    pub status: AssetStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Asset {
    /// An asset created from an imported file.
    pub fn imported(title: &str, now: DateTime<Utc>) -> Result<Self, AssetError> {
        Self::build(title, None, AssetStatus::Ready, now)
    }

    /// An asset that only has a source link so far (e.g. a saved TikTok URL).
    pub fn from_link(title: &str, url: &str, now: DateTime<Utc>) -> Result<Self, AssetError> {
        Self::build(
            title,
            Some(url.trim().to_owned()),
            AssetStatus::AwaitingMedia,
            now,
        )
    }

    fn build(
        title: &str,
        source_url: Option<String>,
        status: AssetStatus,
        now: DateTime<Utc>,
    ) -> Result<Self, AssetError> {
        let title = title.trim();
        if title.is_empty() {
            return Err(AssetError::EmptyTitle);
        }
        Ok(Self {
            id: AssetId::new(),
            title: title.to_owned(),
            creator: None,
            source_url,
            notes: String::new(),
            tags: BTreeSet::new(),
            rating: None,
            video_permission: Permission::default(),
            audio_permission: Permission::default(),
            status,
            created_at: now,
            updated_at: now,
        })
    }

    pub fn rename(&mut self, title: &str, now: DateTime<Utc>) -> Result<(), AssetError> {
        let title = title.trim();
        if title.is_empty() {
            return Err(AssetError::EmptyTitle);
        }
        self.title = title.to_owned();
        self.updated_at = now;
        Ok(())
    }

    /// Returns true when the tag was not already present.
    pub fn add_tag(&mut self, tag: Tag, now: DateTime<Utc>) -> bool {
        let added = self.tags.insert(tag);
        if added {
            self.updated_at = now;
        }
        added
    }

    pub fn remove_tag(&mut self, tag: &Tag, now: DateTime<Utc>) -> bool {
        let removed = self.tags.remove(tag);
        if removed {
            self.updated_at = now;
        }
        removed
    }

    pub fn rate(&mut self, rating: Option<Rating>, now: DateTime<Utc>) {
        self.rating = rating;
        self.updated_at = now;
    }

    pub fn record_video_permission(
        &mut self,
        status: PermissionStatus,
        note: Option<String>,
        now: DateTime<Utc>,
    ) {
        self.video_permission = Permission {
            status,
            note,
            recorded_at: Some(now),
        };
        self.updated_at = now;
    }

    pub fn record_audio_permission(
        &mut self,
        status: PermissionStatus,
        note: Option<String>,
        now: DateTime<Utc>,
    ) {
        self.audio_permission = Permission {
            status,
            note,
            recorded_at: Some(now),
        };
        self.updated_at = now;
    }

    /// Media arrived for an asset that was waiting for it.
    pub fn mark_media_attached(&mut self, now: DateTime<Utc>) {
        self.status = AssetStatus::Ready;
        self.updated_at = now;
    }

    /// Case-insensitive match against title, creator, notes, source URL and tags.
    pub fn matches(&self, query: &str) -> bool {
        let needle = query.trim().to_lowercase();
        if needle.is_empty() {
            return true;
        }
        let needle = needle.trim_start_matches('#');
        let fields = [
            Some(self.title.as_str()),
            self.creator.as_deref(),
            Some(self.notes.as_str()),
            self.source_url.as_deref(),
        ];
        fields
            .into_iter()
            .flatten()
            .any(|f| f.to_lowercase().contains(needle))
            || self.tags.iter().any(|t| t.as_str().contains(needle))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_790_000_000, 0).unwrap()
    }

    #[test]
    fn tags_are_normalized() {
        assert_eq!(Tag::parse("  #Cooking ").unwrap().as_str(), "cooking");
        assert_eq!(
            Tag::parse("behind_the-scenes").unwrap().as_str(),
            "behind_the-scenes"
        );
        assert!(Tag::parse("two words").is_err());
        assert!(Tag::parse("#").is_err());
        assert!(Tag::parse(&"x".repeat(65)).is_err());
    }

    #[test]
    fn ratings_are_bounded() {
        assert!(Rating::new(0).is_err());
        assert!(Rating::new(6).is_err());
        assert_eq!(Rating::new(5).unwrap().stars(), 5);
        assert!(serde_json::from_str::<Rating>("9").is_err());
    }

    #[test]
    fn link_assets_await_media() {
        let mut asset = Asset::from_link("Clip", "https://example.com/v/1", now()).unwrap();
        assert_eq!(asset.status, AssetStatus::AwaitingMedia);
        asset.mark_media_attached(now());
        assert_eq!(asset.status, AssetStatus::Ready);
    }

    #[test]
    fn empty_titles_are_rejected() {
        assert_eq!(Asset::imported("  ", now()), Err(AssetError::EmptyTitle));
    }

    #[test]
    fn tagging_is_idempotent() {
        let mut asset = Asset::imported("Clip", now()).unwrap();
        let tag = Tag::parse("food").unwrap();
        assert!(asset.add_tag(tag.clone(), now()));
        assert!(!asset.add_tag(tag.clone(), now()));
        assert!(asset.remove_tag(&tag, now()));
        assert!(!asset.remove_tag(&tag, now()));
    }

    #[test]
    fn search_matches_fields_and_tags() {
        let mut asset = Asset::imported("Sourdough timelapse", now()).unwrap();
        asset.creator = Some("Baker Bee".into());
        asset.add_tag(Tag::parse("bread").unwrap(), now());
        assert!(asset.matches("sourdough"));
        assert!(asset.matches("BAKER"));
        assert!(asset.matches("#bread"));
        assert!(asset.matches(""));
        assert!(!asset.matches("pizza"));
    }

    #[test]
    fn audio_and_video_permissions_are_independent() {
        let mut asset = Asset::imported("Clip", now()).unwrap();
        asset.record_video_permission(PermissionStatus::Granted, None, now());
        asset.record_audio_permission(PermissionStatus::Denied, Some("TikTok sound".into()), now());
        assert_eq!(asset.video_permission.status, PermissionStatus::Granted);
        assert_eq!(asset.audio_permission.status, PermissionStatus::Denied);
    }
}
