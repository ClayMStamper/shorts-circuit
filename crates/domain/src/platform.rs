//! Destination platforms and their (indicative) media and copy requirements.
//!
//! The limits below are meant for early warnings in review; each adapter re-validates against
//! the provider's current rules. YouTube's come from [`crate::youtube`], checked against
//! Google's docs in October 2026, Instagram's from [`crate::instagram`] and LinkedIn's from
//! [`crate::linkedin`], and Snapchat's from [`crate::snapchat`]; all but YouTube's still need
//! confirming against a real post (see `docs/human-scope.md`).

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::instagram;
use crate::linkedin;
use crate::media::MediaProperties;
use crate::snapchat;
use crate::youtube;

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
                min_duration_ms: youtube::MIN_DURATION_MS,
                max_duration_ms: youtube::SHORTS_MAX_DURATION_MS,
                max_file_bytes: youtube::MAX_FILE_BYTES,
                require_vertical_or_square: true,
                max_caption_chars: youtube::DESCRIPTION_MAX_BYTES,
                max_hashtags: youtube::MAX_HASHTAGS,
            },
            Self::InstagramReels => MediaRequirements {
                min_duration_ms: instagram::MIN_DURATION_MS,
                max_duration_ms: instagram::MAX_DURATION_MS,
                max_file_bytes: instagram::MAX_FILE_BYTES,
                require_vertical_or_square: false,
                max_caption_chars: instagram::CAPTION_MAX_CHARS,
                max_hashtags: instagram::MAX_HASHTAGS,
            },
            Self::SnapchatSpotlight => MediaRequirements {
                min_duration_ms: snapchat::MIN_DURATION_MS,
                max_duration_ms: snapchat::MAX_DURATION_MS,
                max_file_bytes: snapchat::MAX_FILE_BYTES,
                require_vertical_or_square: true,
                max_caption_chars: snapchat::DESCRIPTION_MAX_CHARS,
                max_hashtags: snapchat::MAX_HASHTAGS,
            },
            Self::Linkedin => MediaRequirements {
                min_duration_ms: linkedin::MIN_DURATION_MS,
                max_duration_ms: linkedin::MAX_DURATION_MS,
                max_file_bytes: linkedin::MAX_FILE_BYTES,
                require_vertical_or_square: false,
                max_caption_chars: linkedin::COMMENTARY_MAX_CHARS,
                max_hashtags: linkedin::MAX_HASHTAGS,
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
    TooShort {
        duration_ms: u64,
        min_ms: u64,
    },
    TooLong {
        duration_ms: u64,
        max_ms: u64,
    },
    FileTooLarge {
        size_bytes: u64,
        max_bytes: u64,
    },
    NotVertical {
        width: u32,
        height: u32,
    },
    CaptionTooLong {
        chars: usize,
        max: usize,
    },
    TooManyHashtags {
        count: usize,
        max: usize,
    },
    /// Smaller than the platform's minimum resolution (Snapchat Spotlight: 540x960).
    ResolutionTooLow {
        width: u32,
        height: u32,
        min_width: u32,
        min_height: u32,
    },
    /// More `@name` mentions than the platform allows (Instagram: 20).
    TooManyMentions {
        count: usize,
        max: usize,
    },
    NotRendered,
    AudioPermissionMissing,
    VideoPermissionMissing,
    /// The platform doesn't accept this container.
    UnsupportedFormat {
        extension: String,
    },
    /// The platform refuses some characters in this field (YouTube: `<` and `>`).
    ForbiddenCharacters {
        field: String,
    },
    /// The copy is longer than the platform allows, counted in bytes.
    TooManyBytes {
        field: String,
        bytes: usize,
        max: usize,
    },
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
            Self::ResolutionTooLow {
                width,
                height,
                min_width,
                min_height,
            } => write!(
                f,
                "video is {width}x{height}; the minimum is {min_width}x{min_height}"
            ),
            Self::TooManyMentions { count, max } => {
                write!(f, "{count} @mentions; the maximum is {max}")
            }
            Self::NotRendered => f.write_str("the variant has not been rendered yet"),
            Self::AudioPermissionMissing => {
                f.write_str("audio permission is not recorded as granted; mute or replace it")
            }
            Self::VideoPermissionMissing => {
                f.write_str("video permission is not recorded as granted")
            }
            Self::UnsupportedFormat { extension } if extension.is_empty() => {
                f.write_str("the file has no extension, so its format is unknown")
            }
            Self::UnsupportedFormat { extension } => {
                write!(f, ".{extension} files are not accepted")
            }
            Self::ForbiddenCharacters { field } => {
                write!(f, "the {field} contains < or >, which the platform refuses")
            }
            Self::TooManyBytes { field, bytes, max } => {
                write!(f, "the {field} is {bytes} bytes; the maximum is {max}")
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
            req.check(&vertical(5_000), 1, "", &[])[..],
            [ValidationIssue::TooShort { .. }]
        ));
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
