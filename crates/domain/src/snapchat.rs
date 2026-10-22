//! Snapchat Spotlight rules, kept in one place so validation, the export package and the API
//! adapter agree.
//!
//! From Snap's Public Profile API docs ("Profile Asset Management", read 17 October 2026);
//! confirm against a real post during the first smoke test:
//!
//! - Spotlight videos are `.mp4`, 6 to 60 seconds, at least 540x960.
//! - The description is at most 160 characters and may include hashtags.
//! - Uploads are encrypted with AES-256-CBC and sent in parts of up to 32 MB (at most 35 parts,
//!   1 GB in total). A media object can be used for 24 hours after it is created.
//! - Access tokens last an hour and are refreshed with the OAuth app's client secret.

pub const MIN_DURATION_MS: u64 = 6_000;
pub const MAX_DURATION_MS: u64 = 60_000;
pub const MIN_WIDTH: u32 = 540;
pub const MIN_HEIGHT: u32 = 960;
pub const MAX_FILE_BYTES: u64 = 1024 * 1024 * 1024;
pub const PART_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_PARTS: usize = 35;
pub const DESCRIPTION_MAX_CHARS: usize = 160;
/// Snap sets no number; the 160-character description is the real limit.
pub const MAX_HASHTAGS: usize = 10;
pub const FILE_EXTENSIONS: &[&str] = &["mp4"];
/// How long a media object stays usable after it is created, in hours.
pub const MEDIA_LIFETIME_HOURS: i64 = 24;
pub const DEFAULT_LOCALE: &str = "en_US";
