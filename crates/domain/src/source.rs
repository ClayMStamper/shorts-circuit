//! Posts noticed on a source platform (TikTok) and the heuristic that pairs them with files
//! arriving in the watch folder.
//!
//! The Display API gives post metadata but never the original file, so a detected post becomes
//! a library item awaiting media. When a file arrives with a similar duration around the post
//! time, the app *suggests* the pair; only the user's confirmation merges them.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::AssetId;

/// A TikTok post the app has seen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TikTokPost {
    pub video_id: String,
    /// The library item for this post: the awaiting-media item until a match is confirmed,
    /// then the imported asset that has the original file.
    pub asset_id: AssetId,
    pub posted_at: DateTime<Utc>,
    /// TikTok reports whole seconds.
    pub duration_ms: Option<u64>,
    pub title: String,
    pub caption: String,
    pub share_url: Option<String>,
    /// TikTok's signed cover link (expires).
    pub cover_url: Option<String>,
    /// A local copy of the cover image in the render cache, when one was fetched.
    pub cover_path: Option<String>,
    pub discovered_at: DateTime<Utc>,
    pub matched_at: Option<DateTime<Utc>>,
}

/// A duration this close counts as the same video (TikTok rounds to whole seconds).
pub const MATCH_DURATION_TOLERANCE_MS: u64 = 1_500;
/// The file must arrive this close to the post time, before or after.
pub const MATCH_WINDOW_HOURS: i64 = 72;
const MAX_TITLE_CHARS: usize = 80;

/// How well a file fits a post. Lower is better; durations count before arrival times.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MatchScore {
    pub duration_diff_ms: u64,
    pub seconds_apart: i64,
}

/// `None` when the file can't be this post: durations differ, it arrived too far from the post
/// time, or TikTok didn't report a duration.
pub fn match_score(
    posted_at: DateTime<Utc>,
    post_duration_ms: Option<u64>,
    arrived_at: DateTime<Utc>,
    file_duration_ms: u64,
) -> Option<MatchScore> {
    let post_duration_ms = post_duration_ms?;
    let duration_diff_ms = post_duration_ms.abs_diff(file_duration_ms);
    let apart = (arrived_at - posted_at).abs();
    (duration_diff_ms <= MATCH_DURATION_TOLERANCE_MS
        && apart <= Duration::hours(MATCH_WINDOW_HOURS))
    .then(|| MatchScore {
        duration_diff_ms,
        seconds_apart: apart.num_seconds(),
    })
}

/// A library title for a post: TikTok's title, else the caption's first line, else the date.
pub fn post_title(title: Option<&str>, caption: Option<&str>, posted_at: DateTime<Utc>) -> String {
    let pick = |s: Option<&str>| {
        s.and_then(|s| s.lines().map(str::trim).find(|l| !l.is_empty()))
            .map(str::to_owned)
    };
    let raw = pick(title)
        .or_else(|| pick(caption))
        .unwrap_or_else(|| format!("TikTok post {}", posted_at.format("%Y-%m-%d %H:%M")));
    if raw.chars().count() > MAX_TITLE_CHARS {
        let cut: String = raw.chars().take(MAX_TITLE_CHARS - 1).collect();
        format!("{}…", cut.trim_end())
    } else {
        raw
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn matching_needs_similar_duration_and_nearby_arrival() {
        let posted = at("2026-10-30T18:00:00Z");
        let close = match_score(posted, Some(23_000), at("2026-10-30T17:40:00Z"), 23_480).unwrap();
        assert_eq!(close.duration_diff_ms, 480);
        assert_eq!(close.seconds_apart, 1_200);
        // Arriving after posting (saved from the app later) works too.
        assert!(match_score(posted, Some(23_000), at("2026-11-01T09:00:00Z"), 22_000).is_some());
        assert!(match_score(posted, Some(23_000), at("2026-10-30T17:40:00Z"), 25_000).is_none());
        assert!(match_score(posted, Some(23_000), at("2026-11-03T19:00:00Z"), 23_000).is_none());
        assert!(match_score(posted, None, posted, 23_000).is_none());
    }

    #[test]
    fn closer_durations_rank_first() {
        let posted = at("2026-10-30T18:00:00Z");
        let a = match_score(posted, Some(20_000), at("2026-10-30T10:00:00Z"), 20_100).unwrap();
        let b = match_score(posted, Some(20_000), at("2026-10-30T17:59:00Z"), 20_900).unwrap();
        assert!(a < b);
    }

    #[test]
    fn titles_fall_back_to_caption_then_date() {
        let posted = at("2026-10-30T18:00:00Z");
        assert_eq!(post_title(Some("Bagels"), Some("x"), posted), "Bagels");
        assert_eq!(
            post_title(Some("  "), Some("\nBest bagels ever\n#bread"), posted),
            "Best bagels ever"
        );
        assert_eq!(
            post_title(None, None, posted),
            "TikTok post 2026-10-30 18:00"
        );
        let long = "a".repeat(200);
        assert_eq!(post_title(None, Some(&long), posted).chars().count(), 80);
    }
}
