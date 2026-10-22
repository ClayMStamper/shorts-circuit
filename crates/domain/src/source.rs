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

/// How long after "Get original" a download is attributed to that post first.
pub const EXPECT_ORIGINAL_MINUTES: i64 = 30;

/// Which pending post a video that just appeared in the downloads folder is the original of.
///
/// 1. A post the owner asked for ("Get original", within [`EXPECT_ORIGINAL_MINUTES`]) whose
///    duration fits wins; with several, the most recently asked for. A post whose duration
///    TikTok didn't report fits any file here, since the owner just asked for it.
/// 2. Otherwise the file must fit exactly one pending post by duration and post time (as the
///    watch folder's matching does); anything ambiguous or unrelated is left alone.
pub fn pick_post_for_download<'a>(
    pending: &'a [TikTokPost],
    expected: &[(String, DateTime<Utc>)],
    file_duration_ms: u64,
    now: DateTime<Utc>,
) -> Option<&'a TikTokPost> {
    let fits = |p: &TikTokPost| {
        p.duration_ms
            .is_none_or(|d| d.abs_diff(file_duration_ms) <= MATCH_DURATION_TOLERANCE_MS)
    };
    let recent = Duration::minutes(EXPECT_ORIGINAL_MINUTES);
    let asked = pending
        .iter()
        .filter_map(|p| {
            expected
                .iter()
                .filter(|(id, at)| *id == p.video_id && now - *at <= recent && *at <= now)
                .map(|(_, at)| *at)
                .max()
                .map(|at| (p, at))
        })
        .filter(|(p, _)| fits(p))
        .max_by_key(|(_, at)| *at);
    if let Some((post, _)) = asked {
        return Some(post);
    }
    let mut fitting = pending
        .iter()
        .filter(|p| match_score(p.posted_at, p.duration_ms, now, file_duration_ms).is_some());
    match (fitting.next(), fitting.next()) {
        (Some(only), None) => Some(only),
        _ => None,
    }
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

    fn post(id: &str, posted: &str, duration_ms: Option<u64>) -> TikTokPost {
        TikTokPost {
            video_id: id.into(),
            asset_id: AssetId::new(),
            posted_at: at(posted),
            duration_ms,
            title: id.into(),
            caption: String::new(),
            share_url: None,
            cover_url: None,
            cover_path: None,
            discovered_at: at(posted),
            matched_at: None,
        }
    }

    #[test]
    fn a_requested_post_wins_even_when_old() {
        let now = at("2026-11-20T10:00:00Z");
        let pending = vec![
            post("old", "2026-10-01T10:00:00Z", Some(51_000)),
            post("new", "2026-11-20T09:00:00Z", Some(51_000)),
        ];
        // Without a request the file fits only the recent post.
        assert_eq!(
            pick_post_for_download(&pending, &[], 51_400, now)
                .unwrap()
                .video_id,
            "new"
        );
        // Asking for the old one makes it win, even weeks after it was posted.
        let asked = [("old".to_owned(), now - Duration::minutes(2))];
        assert_eq!(
            pick_post_for_download(&pending, &asked, 51_400, now)
                .unwrap()
                .video_id,
            "old"
        );
        // But not with the wrong length, or once the request is stale.
        assert_eq!(
            pick_post_for_download(&pending, &asked, 20_000, now).map(|p| p.video_id.as_str()),
            None
        );
        let stale = [("old".to_owned(), now - Duration::minutes(45))];
        assert_eq!(
            pick_post_for_download(&pending, &stale, 51_400, now)
                .unwrap()
                .video_id,
            "new"
        );
    }

    #[test]
    fn the_latest_request_wins_and_unknown_durations_fit_when_asked() {
        let now = at("2026-11-20T10:00:00Z");
        let pending = vec![
            post("a", "2026-10-01T10:00:00Z", Some(30_000)),
            post("b", "2026-10-02T10:00:00Z", None),
        ];
        let asked = [
            ("a".to_owned(), now - Duration::minutes(5)),
            ("b".to_owned(), now - Duration::minutes(1)),
        ];
        assert_eq!(
            pick_post_for_download(&pending, &asked, 30_000, now)
                .unwrap()
                .video_id,
            "b"
        );
    }

    #[test]
    fn unrequested_downloads_need_exactly_one_fitting_post() {
        let now = at("2026-11-20T10:00:00Z");
        let pending = vec![
            post("a", "2026-11-20T08:00:00Z", Some(30_000)),
            post("b", "2026-11-20T09:00:00Z", Some(30_200)),
        ];
        // Two recent posts of the same length: ambiguous, left alone.
        assert!(pick_post_for_download(&pending, &[], 30_100, now).is_none());
        // An unrelated video in Downloads: left alone.
        assert!(pick_post_for_download(&pending, &[], 95_000, now).is_none());
        assert!(pick_post_for_download(&[], &[], 30_000, now).is_none());
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
