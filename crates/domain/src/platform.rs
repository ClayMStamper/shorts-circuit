//! Destination platforms and their (indicative) media and copy requirements.
//!
//! The limits below are as understood in mid-2026 and are meant for early warnings in review.
//! Each adapter re-validates against the provider's current rules during Increment 3, and the
//! numbers here must be confirmed during Increment 1 (see `docs/human-scope.md`).

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::media::MediaProperties;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    YoutubeShorts,
    InstagramReels,
    SnapchatSpotlight,
    Linkedin,
}

impl Platform {
    pub const ALL: [Platform; 4] = [
        Platform::YoutubeShorts,
        Platform::InstagramReels,
        Platform::SnapchatSpotlight,
        Platform::Linkedin,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::YoutubeShorts => "youtube_shorts",
            Self::InstagramReels => "instagram_reels",
            Self::SnapchatSpotlight => "snapchat_spotlight",
            Self::Linkedin => "linkedin",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.as_str() == s)
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::YoutubeShorts => "YouTube Shorts",
            Self::InstagramReels => "Instagram Reels",
            Self::SnapchatSpotlight => "Snapchat Spotlight",
            Self::Linkedin => "LinkedIn",
        }
    }

    pub fn requirements(self) -> MediaRequirements {
        match self {
            Self::YoutubeShorts => MediaRequirements {
                min_duration_ms: 1_000,
                max_duration_ms: 180_000,
                max_file_bytes: 256 * 1024 * 1024 * 1024,
                require_vertical_or_square: true,
                max_caption_chars: 5_000,
                max_hashtags: 15,
            },
            Self::InstagramReels => MediaRequirements {
                min_duration_ms: 3_000,
                max_duration_ms: 15 * 60_000,
                max_file_bytes: 300 * 1024 * 1024,
                require_vertical_or_square: false,
                max_caption_chars: 2_200,
                max_hashtags: 30,
            },
            Self::SnapchatSpotlight => MediaRequirements {
                min_duration_ms: 5_000,
                max_duration_ms: 60_000,
                max_file_bytes: 1024 * 1024 * 1024,
                require_vertical_or_square: true,
                max_caption_chars: 160,
                max_hashtags: 10,
            },
            Self::Linkedin => MediaRequirements {
                min_duration_ms: 3_000,
                max_duration_ms: 30 * 60_000,
                max_file_bytes: 500 * 1024 * 1024,
                require_vertical_or_square: false,
                max_caption_chars: 3_000,
                max_hashtags: 30,
            },
        }
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.display_name())
    }
}

/// Indicative per-platform limits used for early validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaRequirements {
    pub min_duration_ms: u64,
    pub max_duration_ms: u64,
    pub max_file_bytes: u64,
    pub require_vertical_or_square: bool,
    pub max_caption_chars: usize,
    pub max_hashtags: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ValidationIssue {
    TooShort { duration_ms: u64, min_ms: u64 },
    TooLong { duration_ms: u64, max_ms: u64 },
    FileTooLarge { size_bytes: u64, max_bytes: u64 },
    NotVertical { width: u32, height: u32 },
    CaptionTooLong { chars: usize, max: usize },
    TooManyHashtags { count: usize, max: usize },
    NotRendered,
    AudioPermissionMissing,
    VideoPermissionMissing,
}

impl fmt::Display for ValidationIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort {
                duration_ms,
                min_ms,
            } => write!(
                f,
                "video is {:.1}s; the minimum is {:.1}s",
                *duration_ms as f64 / 1000.0,
                *min_ms as f64 / 1000.0
            ),
            Self::TooLong {
                duration_ms,
                max_ms,
            } => write!(
                f,
                "video is {:.1}s; the maximum is {:.1}s",
                *duration_ms as f64 / 1000.0,
                *max_ms as f64 / 1000.0
            ),
            Self::FileTooLarge {
                size_bytes,
                max_bytes,
            } => write!(f, "file is {size_bytes} bytes; the maximum is {max_bytes}"),
            Self::NotVertical { width, height } => {
                write!(
                    f,
                    "video is {width}x{height}; it must be vertical or square"
                )
            }
            Self::CaptionTooLong { chars, max } => {
                write!(f, "caption is {chars} characters; the maximum is {max}")
            }
            Self::TooManyHashtags { count, max } => {
                write!(f, "{count} hashtags; the maximum is {max}")
            }
            Self::NotRendered => f.write_str("the variant has not been rendered yet"),
            Self::AudioPermissionMissing => {
                f.write_str("audio permission is not recorded as granted; mute or replace it")
            }
            Self::VideoPermissionMissing => {
                f.write_str("video permission is not recorded as granted")
            }
        }
    }
}

impl MediaRequirements {
    /// Checks rendered media plus the copy that will accompany it.
    pub fn check(
        &self,
        media: &MediaProperties,
        size_bytes: u64,
        caption: &str,
        hashtags: &[String],
    ) -> Vec<ValidationIssue> {
        let mut issues = Vec::new();
        if media.duration_ms < self.min_duration_ms {
            issues.push(ValidationIssue::TooShort {
                duration_ms: media.duration_ms,
                min_ms: self.min_duration_ms,
            });
        }
        if media.duration_ms > self.max_duration_ms {
            issues.push(ValidationIssue::TooLong {
                duration_ms: media.duration_ms,
                max_ms: self.max_duration_ms,
            });
        }
        if size_bytes > self.max_file_bytes {
            issues.push(ValidationIssue::FileTooLarge {
                size_bytes,
                max_bytes: self.max_file_bytes,
            });
        }
        let (w, h) = media.display_dimensions();
        if self.require_vertical_or_square && w > h {
            issues.push(ValidationIssue::NotVertical {
                width: w,
                height: h,
            });
        }
        let chars = full_caption(caption, hashtags).chars().count();
        if chars > self.max_caption_chars {
            issues.push(ValidationIssue::CaptionTooLong {
                chars,
                max: self.max_caption_chars,
            });
        }
        if hashtags.len() > self.max_hashtags {
            issues.push(ValidationIssue::TooManyHashtags {
                count: hashtags.len(),
                max: self.max_hashtags,
            });
        }
        issues
    }
}

/// The caption as posted: text followed by `#tag` words.
pub fn full_caption(caption: &str, hashtags: &[String]) -> String {
    let tags = hashtags
        .iter()
        .map(|t| format!("#{t}"))
        .collect::<Vec<_>>()
        .join(" ");
    match (caption.trim().is_empty(), tags.is_empty()) {
        (true, _) => tags,
        (false, true) => caption.trim().to_owned(),
        (false, false) => format!("{}\n\n{tags}", caption.trim()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vertical(duration_ms: u64) -> MediaProperties {
        MediaProperties {
            duration_ms,
            width: 1080,
            height: 1920,
            ..Default::default()
        }
    }

    #[test]
    fn platform_names_round_trip() {
        for p in Platform::ALL {
            assert_eq!(Platform::parse(p.as_str()), Some(p));
        }
        assert_eq!(Platform::parse("tiktok"), None);
    }

    #[test]
    fn spotlight_rejects_long_and_horizontal_videos() {
        let req = Platform::SnapchatSpotlight.requirements();
        assert!(req.check(&vertical(30_000), 1, "", &[]).is_empty());
        assert!(matches!(
            req.check(&vertical(90_000), 1, "", &[])[..],
            [ValidationIssue::TooLong { .. }]
        ));
        let horizontal = MediaProperties {
            duration_ms: 30_000,
            width: 1920,
            height: 1080,
            ..Default::default()
        };
        assert!(matches!(
            req.check(&horizontal, 1, "", &[])[..],
            [ValidationIssue::NotVertical { .. }]
        ));
    }

    #[test]
    fn caption_length_includes_hashtags() {
        let req = Platform::SnapchatSpotlight.requirements();
        let caption = "x".repeat(150);
        let tags = vec!["abcdefgh".to_owned(), "ijklmnop".to_owned()];
        let issues = req.check(&vertical(30_000), 1, &caption, &tags);
        assert!(matches!(
            issues[..],
            [ValidationIssue::CaptionTooLong { .. }]
        ));
    }

    #[test]
    fn full_caption_joins_text_and_tags() {
        let tags = vec!["a".to_owned(), "b".to_owned()];
        assert_eq!(full_caption(" Hi ", &tags), "Hi\n\n#a #b");
        assert_eq!(full_caption("", &tags), "#a #b");
        assert_eq!(full_caption("Hi", &[]), "Hi");
    }
}
