//! YouTube Shorts rules, kept in one place so validation, the export package and the API
//! adapter agree.
//!
//! Checked against Google's documentation on 17 October 2026 (see the YouTube connector PR for
//! which values were confirmed and which are assumptions):
//!
//! - Shorts are up to three minutes long and must be square or vertical; wider videos are
//!   classified as regular videos (YouTube Help, "Understand three-minute Shorts").
//! - `videos.insert` accepts files up to 256 GB of any `video/*` type.
//! - Titles are at most 100 characters and descriptions at most 5,000 bytes; neither may contain
//!   `<` or `>` (Data API `video` resource).
//! - Quota: uploads are charged to their own bucket (default 100 `videos.insert` calls a day,
//!   1 unit each); every other call uses the 10,000-unit daily bucket (`videos.list` 1,
//!   `channels.list` 1, `playlistItems.list` 1, `videos.update` 50). Both reset at midnight
//!   Pacific time.

/// The longest video YouTube classifies as a Short.
pub const SHORTS_MAX_DURATION_MS: u64 = 180_000;
/// Indicative minimum; YouTube rejects near-empty files during processing.
pub const MIN_DURATION_MS: u64 = 1_000;
pub const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024 * 1024;
pub const TITLE_MAX_CHARS: usize = 100;
pub const DESCRIPTION_MAX_BYTES: usize = 5_000;
/// More than this many hashtags and YouTube ignores all of them; the app stays well below.
pub const MAX_HASHTAGS: usize = 15;
/// Containers YouTube lists as supported upload formats. Renders are always MP4.
pub const FILE_EXTENSIONS: &[&str] = &[
    "mp4", "m4v", "mov", "mpg", "mpeg", "avi", "wmv", "flv", "3gp", "3gpp", "webm", "mkv",
];

/// Default daily quota of the uploads bucket (`videos.insert` calls).
pub const DEFAULT_UPLOADS_PER_DAY: u32 = 100;
/// Default daily quota of the bucket every other method is charged to.
pub const DEFAULT_UNITS_PER_DAY: u32 = 10_000;
pub const COST_INSERT: u32 = 1;
pub const COST_LIST: u32 = 1;

/// Characters YouTube refuses in titles and descriptions.
pub fn forbidden_chars(text: &str) -> bool {
    text.contains(['<', '>'])
}

/// The video title: the first non-empty caption line, without characters YouTube refuses,
/// truncated to the limit. Falls back to `"Short"`.
pub fn title(caption: &str) -> String {
    let first = caption
        .lines()
        .map(|l| l.replace(['<', '>'], "").trim().to_owned())
        .find(|l| !l.is_empty())
        .unwrap_or_default();
    let first = if first.is_empty() {
        "Short".to_owned()
    } else {
        first
    };
    first.chars().take(TITLE_MAX_CHARS).collect()
}

/// `"mp4"` for `clip.MP4`; empty without an extension.
pub fn extension(path: &str) -> String {
    std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default()
}

/// The MIME type sent with an upload.
pub fn mime_type(path: &str) -> &'static str {
    match extension(path).as_str() {
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mpg" | "mpeg" => "video/mpeg",
        "3gp" | "3gpp" => "video/3gpp",
        "avi" => "video/x-msvideo",
        "wmv" => "video/x-ms-wmv",
        "flv" => "video/x-flv",
        "mkv" => "video/x-matroska",
        _ => "application/octet-stream",
    }
}

/// `https://www.youtube.com/shorts/{id}`.
pub fn shorts_url(video_id: &str) -> String {
    format!("https://www.youtube.com/shorts/{video_id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_come_from_the_first_caption_line() {
        assert_eq!(title("\nBest bread ever\nMore text"), "Best bread ever");
        assert_eq!(title(""), "Short");
        assert_eq!(title("<>\n"), "Short");
        assert_eq!(title("a <b> c"), "a b c");
        assert_eq!(title(&"x".repeat(150)).chars().count(), 100);
    }

    #[test]
    fn formats_map_to_mime_types() {
        assert_eq!(extension("C:\\v\\clip.MP4"), "mp4");
        assert_eq!(mime_type("clip.mov"), "video/quicktime");
        assert_eq!(mime_type("clip"), "application/octet-stream");
        assert!(FILE_EXTENSIONS.contains(&"mp4"));
        assert!(forbidden_chars("a < b"));
        assert!(!forbidden_chars("a b"));
    }
}
