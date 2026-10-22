//! Instagram Reels rules, kept in one place so validation, the export package and the API
//! adapter agree.
//!
//! As understood in October 2026 from Meta's Instagram Platform docs (Instagram API with
//! Facebook Login, content publishing and resumable uploads); confirm against a real upload
//! during the first smoke test:
//!
//! - Reels are MP4 or MOV (H.264 or HEVC video, AAC audio), 3 seconds to 15 minutes, up to
//!   300 MB, with an aspect ratio between 0.01:1 and 10:1 (9:16 recommended).
//! - Captions are at most 2,200 characters with at most 30 hashtags and 20 @mentions.
//! - An account can publish 100 posts through the API in a rolling 24 hours.
//! - Uploading the file itself (`upload_type=resumable`) is only available to apps using
//!   Facebook Login for Business; with Instagram Login the container needs a public
//!   `video_url`. A Page access token obtained from a long-lived user token doesn't expire.

pub const MIN_DURATION_MS: u64 = 3_000;
pub const MAX_DURATION_MS: u64 = 15 * 60_000;
pub const MAX_FILE_BYTES: u64 = 300 * 1024 * 1024;
pub const CAPTION_MAX_CHARS: usize = 2_200;
pub const MAX_HASHTAGS: usize = 30;
pub const MAX_MENTIONS: usize = 20;
/// Containers Instagram accepts for Reels. Renders are always MP4.
pub const FILE_EXTENSIONS: &[&str] = &["mp4", "mov"];
/// API-published posts allowed per account in a rolling 24 hours.
pub const POSTS_PER_DAY: u32 = 100;
/// Permissions a pasted user token must carry to find the Page, its Instagram account, and
/// publish. `business_management` is asked for too; it is only needed for Pages managed in a
/// business portfolio, so its absence is reported only when no Page is found.
pub const REQUIRED_PERMISSIONS: &[&str] = &[
    "instagram_basic",
    "instagram_content_publish",
    "pages_show_list",
    "pages_read_engagement",
];
pub const BUSINESS_PERMISSION: &str = "business_management";
/// A user token with less than this left is short-lived (they last an hour or two); extended
/// ones last about 60 days, and the Page tokens derived from those don't expire.
pub const LONG_LIVED_MIN_HOURS: i64 = 24;

/// How an Instagram account is connected.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstagramLogin {
    /// Through the linked Facebook Page (Instagram API with Facebook Login): uploads the file
    /// directly and publishes automatically.
    FacebookLogin,
    /// A token from Instagram Login. Instagram only takes a public `video_url` this way, so
    /// such accounts use export packages. Records written before Facebook login existed are
    /// this kind.
    #[default]
    InstagramLogin,
}

/// Account types that can publish through the API.
pub const PUBLISHING_ACCOUNT_TYPES: &[&str] = &["BUSINESS", "MEDIA_CREATOR"];

/// `@name` words in the caption.
pub fn mention_count(caption: &str) -> usize {
    caption
        .split_whitespace()
        .filter(|w| w.len() > 1 && w.starts_with('@'))
        .count()
}

/// `https://www.instagram.com/reel/<shortcode>/` when the shortcode is known.
pub fn reel_url(shortcode: &str) -> String {
    format!("https://www.instagram.com/reel/{shortcode}/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mentions_are_counted_by_word() {
        assert_eq!(mention_count("hi @a and @b, mail me@x.com @"), 2);
        assert_eq!(mention_count(""), 0);
    }

    #[test]
    fn reel_urls_use_the_shortcode() {
        assert_eq!(reel_url("Cx1"), "https://www.instagram.com/reel/Cx1/");
    }
}
