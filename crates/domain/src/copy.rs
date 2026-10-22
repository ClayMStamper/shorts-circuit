//! Copy that travels with a video: reading it from the source (sidecar files next to the
//! original, hashtags at the end of a TikTok caption) and fitting it to each destination's
//! limits so a workflow can publish without anyone re-typing or trimming captions.

use serde::Deserialize;
use serde_json::Value;

use crate::platform::{Platform, full_caption};
use crate::youtube;

/// A caption plus hashtags, as a workflow hands them to a new publication.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PostCopy {
    pub caption: String,
    /// Without `#`, unique (ignoring case), in source order.
    pub hashtags: Vec<String>,
}

fn is_hashtag_word(word: &str) -> bool {
    word.strip_prefix('#').is_some_and(|rest| {
        !rest.is_empty() && rest.chars().all(|c| c.is_alphanumeric() || c == '_')
    })
}

fn push_unique(tags: &mut Vec<String>, raw: &str) {
    let tag = raw.trim().trim_start_matches('#');
    if tag.is_empty() || tag.chars().any(|c| c.is_whitespace() || c == '#') {
        return;
    }
    if !tags.iter().any(|t| t.eq_ignore_ascii_case(tag)) {
        tags.push(tag.to_owned());
    }
}

/// Moves the run of `#hashtags` at the end of `text` into the tag list ("Crumb shot #bread
/// #sourdough" → "Crumb shot" + [bread, sourdough]). Hashtags inside a sentence stay where they
/// are, since removing them would change its meaning.
pub fn split_trailing_hashtags(text: &str) -> PostCopy {
    let words: Vec<&str> = text.split_whitespace().collect();
    let keep = words
        .iter()
        .rposition(|w| !is_hashtag_word(w))
        .map_or(0, |i| i + 1);
    let mut hashtags = Vec::new();
    for w in &words[keep..] {
        push_unique(&mut hashtags, w);
    }
    let caption = if keep == words.len() {
        text.trim().to_owned()
    } else {
        // Cut the original text just before the first trailing tag, keeping its line breaks.
        let mut rest = text.trim_end();
        for _ in keep..words.len() {
            rest = rest.trim_end();
            let at = rest.rfind(char::is_whitespace).map_or(0, |i| i + 1);
            rest = &rest[..at];
        }
        rest.trim().to_owned()
    };
    PostCopy { caption, hashtags }
}

/// The source caption and extra hashtags merged: trailing hashtags of the caption first, then
/// `extra`, without duplicates.
pub fn from_source(caption: &str, extra: &[String]) -> PostCopy {
    let mut copy = split_trailing_hashtags(caption);
    for tag in extra {
        push_unique(&mut copy.hashtags, tag);
    }
    copy
}

/// Fits copy to the destination's limits so review has nothing to flag: drops characters the
/// platform refuses, keeps at most the allowed number of hashtags, then drops hashtags from the
/// end and finally shortens the text (on a word boundary, with "…") until it fits.
pub fn fit(platform: Platform, copy: &PostCopy) -> PostCopy {
    let req = platform.requirements();
    let mut caption = copy.caption.trim().to_owned();
    let mut hashtags: Vec<String> = Vec::new();
    for tag in &copy.hashtags {
        push_unique(&mut hashtags, tag);
    }
    if platform == Platform::YoutubeShorts {
        caption = caption.replace(['<', '>'], "");
        hashtags.retain(|t| !youtube::forbidden_chars(t));
    }
    hashtags.truncate(req.max_hashtags);
    // YouTube counts the description in bytes; the others in characters.
    let size = |text: &str| match platform {
        Platform::YoutubeShorts => text.len(),
        _ => text.chars().count(),
    };
    let max = req.max_caption_chars;
    while size(&full_caption(&caption, &hashtags)) > max && !hashtags.is_empty() {
        hashtags.pop();
    }
    if size(&caption) > max {
        caption = shorten(&caption, max, size);
    }
    PostCopy { caption, hashtags }
}

/// Cuts `text` so that it plus "…" is at most `max` (as measured by `size`), preferring a word
/// boundary in the last fifth of the allowance.
fn shorten(text: &str, max: usize, size: impl Fn(&str) -> usize) -> String {
    const ELLIPSIS: &str = "…";
    if max <= size(ELLIPSIS) {
        return String::new();
    }
    let budget = max - size(ELLIPSIS);
    let mut end = 0;
    for (i, c) in text.char_indices() {
        let next = i + c.len_utf8();
        if size(&text[..next]) > budget {
            break;
        }
        end = next;
    }
    let cut = &text[..end];
    let word_end = cut
        .rfind(char::is_whitespace)
        .filter(|&i| size(&cut[..i]) * 5 >= budget * 4)
        .map_or(cut, |i| &cut[..i]);
    format!("{}{ELLIPSIS}", word_end.trim_end())
}

/// Caption and hashtags read from a file next to the original, as exported by editors and
/// schedulers: `<video name>.txt` (the caption, hashtags inline) or `<video name>.json`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Sidecar {
    pub caption: String,
    pub hashtags: Vec<String>,
}

impl Sidecar {
    /// Longest sidecar read; anything bigger isn't a caption.
    pub const MAX_BYTES: u64 = 64 * 1024;

    /// A plain-text sidecar: the whole file is the caption.
    pub fn from_text(text: &str) -> Option<Self> {
        let caption = text.trim_start_matches('\u{feff}').trim();
        (!caption.is_empty()).then(|| Self {
            caption: caption.to_owned(),
            hashtags: Vec::new(),
        })
    }

    /// A JSON sidecar. The caption is the first of `caption`, `description`, `text` or `title`
    /// that is a non-empty string; hashtags come from `hashtags` or `tags`, as an array of
    /// strings or one string of words. Anything else is ignored.
    pub fn from_json(text: &str) -> Option<Self> {
        #[derive(Deserialize)]
        struct Raw {
            #[serde(flatten)]
            fields: serde_json::Map<String, Value>,
        }
        let raw: Raw = serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()?;
        let caption = ["caption", "description", "text", "title"]
            .iter()
            .find_map(|k| raw.fields.get(*k).and_then(Value::as_str))
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .unwrap_or_default()
            .to_owned();
        let mut hashtags = Vec::new();
        for key in ["hashtags", "tags"] {
            match raw.fields.get(key) {
                Some(Value::Array(items)) => {
                    for item in items.iter().filter_map(Value::as_str) {
                        push_unique(&mut hashtags, item);
                    }
                }
                Some(Value::String(words)) => {
                    for word in words.split([' ', ',', '\n', '\t']) {
                        push_unique(&mut hashtags, word);
                    }
                }
                _ => {}
            }
        }
        (!caption.is_empty() || !hashtags.is_empty()).then_some(Self { caption, hashtags })
    }

    /// The caption as the library keeps it: text, then the hashtags as `#words` on their own
    /// line (the same shape as a TikTok caption, so `{caption}` works for both).
    pub fn caption_with_hashtags(&self) -> String {
        full_caption(&self.caption, &self.hashtags)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(t: &[&str]) -> Vec<String> {
        t.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn trailing_hashtags_are_split_off() {
        let c = split_trailing_hashtags("Crumb shot #bread\nso good #Sourdough #bread #fyp");
        assert_eq!(c.caption, "Crumb shot #bread\nso good");
        assert_eq!(c.hashtags, tags(&["Sourdough", "bread", "fyp"]));
        let c = split_trailing_hashtags("#only #tags");
        assert_eq!(c.caption, "");
        assert_eq!(c.hashtags, tags(&["only", "tags"]));
        let c = split_trailing_hashtags("No tags here.");
        assert_eq!(c.caption, "No tags here.");
        assert!(c.hashtags.is_empty());
        // "#" alone or "#a-b" isn't a hashtag word.
        assert_eq!(split_trailing_hashtags("A #").caption, "A #");
    }

    #[test]
    fn extra_hashtags_follow_the_source_ones() {
        let c = from_source("Hi #a", &tags(&["#b", "A", "c d"]));
        assert_eq!(c.caption, "Hi");
        assert_eq!(c.hashtags, tags(&["a", "b"]));
    }

    #[test]
    fn spotlight_copy_fits_160_characters() {
        let copy = PostCopy {
            caption: "word ".repeat(30).trim().to_owned(),
            hashtags: tags(&["one", "two", "three"]),
        };
        let fitted = fit(Platform::SnapchatSpotlight, &copy);
        let full = full_caption(&fitted.caption, &fitted.hashtags);
        assert!(full.chars().count() <= 160, "{full}");
        // 149 characters of text, a blank line and "#one #two" make exactly 160.
        assert_eq!(fitted.hashtags, tags(&["one", "two"]));
        assert_eq!(fitted.caption, copy.caption);

        let long = PostCopy {
            caption: "x".repeat(150) + " end of a long sentence",
            hashtags: tags(&["tag"]),
        };
        let fitted = fit(Platform::SnapchatSpotlight, &long);
        assert!(fitted.hashtags.is_empty());
        assert!(fitted.caption.ends_with('…'));
        assert!(fitted.caption.chars().count() <= 160);
        assert!(
            Platform::SnapchatSpotlight
                .requirements()
                .check(
                    &crate::MediaProperties {
                        duration_ms: 10_000,
                        width: 1080,
                        height: 1920,
                        ..Default::default()
                    },
                    1,
                    &fitted.caption,
                    &fitted.hashtags
                )
                .is_empty()
        );
    }

    #[test]
    fn youtube_drops_forbidden_characters_and_counts_bytes() {
        let fitted = fit(
            Platform::YoutubeShorts,
            &PostCopy {
                caption: "a <b> c".into(),
                hashtags: tags(&["ok"]),
            },
        );
        assert_eq!(fitted.caption, "a b c");
        let wide = PostCopy {
            caption: "é".repeat(3_000),
            hashtags: vec![],
        };
        let fitted = fit(Platform::YoutubeShorts, &wide);
        assert!(fitted.caption.len() <= youtube::DESCRIPTION_MAX_BYTES);
    }

    #[test]
    fn instagram_keeps_at_most_thirty_hashtags() {
        let many: Vec<String> = (0..40).map(|i| format!("t{i}")).collect();
        let fitted = fit(
            Platform::InstagramReels,
            &PostCopy {
                caption: "Hi".into(),
                hashtags: many,
            },
        );
        assert_eq!(fitted.hashtags.len(), 30);
    }

    #[test]
    fn sidecars_are_read_from_text_and_json() {
        assert_eq!(Sidecar::from_text("  \n"), None);
        assert_eq!(
            Sidecar::from_text("\u{feff}Hello #a\n").unwrap().caption,
            "Hello #a"
        );
        let s = Sidecar::from_json(r##"{"description":"Bread day","hashtags":["#bread","fyp"]}"##)
            .unwrap();
        assert_eq!(s.caption, "Bread day");
        assert_eq!(s.hashtags, tags(&["bread", "fyp"]));
        assert_eq!(s.caption_with_hashtags(), "Bread day\n\n#bread #fyp");
        let s = Sidecar::from_json(r#"{"caption":"","tags":"a, #b c"}"#).unwrap();
        assert_eq!(s.hashtags, tags(&["a", "b", "c"]));
        assert_eq!(Sidecar::from_json("{}"), None);
        assert_eq!(Sidecar::from_json("not json"), None);
    }
}
