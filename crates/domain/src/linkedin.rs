//! LinkedIn video post rules, kept in one place so validation, the export package and the API
//! adapter agree.
//!
//! As understood in October 2026 from LinkedIn's Marketing API docs (Videos API, Posts API,
//! "little text" commentary format); confirm against a real post during the first smoke test:
//!
//! - Videos are MP4, 3 seconds to 30 minutes, 75 KB to 500 MB.
//! - Post commentary is at most 3,000 characters, in LinkedIn's "little text" format: the
//!   characters `\ | { } @ [ ] ( ) < > # * _ ~` are escaped with a backslash, and hashtags are
//!   written as `{hashtag|\#|name}`.
//! - Organization posts need the Community Management API (`w_organization_social`); member
//!   access tokens from the 3-legged flow last 60 days.

pub const MIN_DURATION_MS: u64 = 3_000;
pub const MAX_DURATION_MS: u64 = 30 * 60_000;
pub const MIN_FILE_BYTES: u64 = 75 * 1024;
pub const MAX_FILE_BYTES: u64 = 500 * 1024 * 1024;
pub const COMMENTARY_MAX_CHARS: usize = 3_000;
/// LinkedIn has no hard hashtag limit; more than a handful reads as spam.
pub const MAX_HASHTAGS: usize = 30;
pub const FILE_EXTENSIONS: &[&str] = &["mp4"];
/// How long an access token from the 3-legged flow lasts.
pub const TOKEN_LIFETIME_DAYS: i64 = 60;
/// Video titles shown on the post.
pub const TITLE_MAX_CHARS: usize = 200;

const RESERVED: &[char] = &[
    '\\', '|', '{', '}', '@', '[', ']', '(', ')', '<', '>', '#', '*', '_', '~',
];

/// Escapes text for LinkedIn's "little text" format.
pub fn escape_little_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if RESERVED.contains(&c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// A hashtag in "little text": `{hashtag|\#|name}`. Characters other than letters, digits
/// and underscores are dropped, since LinkedIn hashtags can't contain them.
pub fn hashtag(name: &str) -> Option<String> {
    let clean: String = name
        .trim_start_matches('#')
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!clean.is_empty()).then(|| format!("{{hashtag|\\#|{}}}", escape_little_text(&clean)))
}

/// The post commentary: the escaped caption, then the hashtags on their own line.
pub fn commentary(caption: &str, hashtags: &[String]) -> String {
    let tags: Vec<String> = hashtags.iter().filter_map(|t| hashtag(t)).collect();
    let text = escape_little_text(caption.trim());
    match (text.is_empty(), tags.is_empty()) {
        (true, _) => tags.join(" "),
        (false, true) => text,
        (false, false) => format!("{text}\n\n{}", tags.join(" ")),
    }
}

/// The video title: the first non-empty caption line, truncated. Falls back to `"Video"`.
pub fn title(caption: &str) -> String {
    let first = caption
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("Video");
    first.chars().take(TITLE_MAX_CHARS).collect()
}

/// `https://www.linkedin.com/feed/update/<urn>/` for a post URN.
pub fn post_url(urn: &str) -> String {
    format!("https://www.linkedin.com/feed/update/{urn}/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_characters_are_escaped() {
        assert_eq!(
            escape_little_text("Hi (all) @you #1 a_b ~x"),
            r"Hi \(all\) \@you \#1 a\_b \~x"
        );
        assert_eq!(escape_little_text("plain text."), "plain text.");
    }

    #[test]
    fn hashtags_use_the_template() {
        assert_eq!(
            hashtag("#indie-dev").as_deref(),
            Some(r"{hashtag|\#|indiedev}")
        );
        assert_eq!(hashtag("rpg").as_deref(), Some(r"{hashtag|\#|rpg}"));
        assert_eq!(hashtag("#!!"), None);
    }

    #[test]
    fn commentary_joins_caption_and_tags() {
        let tags = vec!["gamedev".to_owned(), "rpg".to_owned()];
        assert_eq!(
            commentary(" New patch (beta) ", &tags),
            "New patch \\(beta\\)\n\n{hashtag|\\#|gamedev} {hashtag|\\#|rpg}"
        );
        assert_eq!(
            commentary("", &tags),
            "{hashtag|\\#|gamedev} {hashtag|\\#|rpg}"
        );
        assert_eq!(commentary("Hi", &[]), "Hi");
    }

    #[test]
    fn titles_and_urls() {
        assert_eq!(title("\n  First line \nsecond"), "First line");
        assert_eq!(title(""), "Video");
        assert_eq!(
            post_url("urn:li:share:7"),
            "https://www.linkedin.com/feed/update/urn:li:share:7/"
        );
    }
}
