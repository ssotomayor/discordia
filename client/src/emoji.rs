use std::path::PathBuf;

use crate::identity::config_dir;

pub fn needs_rendering(text: &str) -> bool {
    text.chars().any(|c| {
        c == ':' || matches!(c as u32, 0x1f000..=0x1faff | 0x2600..=0x27bf | 0x20e3 | 0xfe0f)
    })
}

pub fn unicode_parts(text: &str) -> Vec<(&str, bool)> {
    use unicode_segmentation::UnicodeSegmentation;
    let mut parts: Vec<(&str, bool)> = Vec::new();
    for (offset, grapheme) in text.grapheme_indices(true) {
        let emoji = grapheme.contains('\u{20e3}')
            || grapheme.chars().any(|c| {
                matches!(c as u32, 0x1f000..=0x1faff | 0x2600..=0x27bf)
                    || (grapheme.contains('\u{fe0f}')
                        && matches!(c as u32, 0xa9 | 0xae | 0x203c..=0x3299))
            });
        if !emoji && let Some((last, false)) = parts.last_mut() {
            let start = offset - last.len();
            *last = &text[start..offset + grapheme.len()];
        } else {
            parts.push((grapheme, emoji));
        }
    }
    parts
}

#[cfg(test)]
mod unicode_tests {
    #[test]
    fn emoji_sequences_remain_whole_without_changing_text() {
        let text = "Hi 👨‍👩‍👧‍👦 👍🏽 🇦🇷 1️⃣ café";
        let parts = super::unicode_parts(text);
        assert_eq!(parts.iter().map(|(s, _)| *s).collect::<String>(), text);
        assert_eq!(parts.iter().filter(|(_, emoji)| *emoji).count(), 4);
        assert_eq!(
            super::unicode_parts("123 abc é"),
            vec![("123 abc é", false)]
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece<'a> {
    Text(&'a str),
    Shortcode(&'a str),
}

pub fn split_shortcodes(s: &str) -> Vec<Piece<'_>> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut cursor = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b':' {
            i += 1;
            continue;
        }
        match s[i + 1..].find(':') {
            Some(rel) => {
                let close = i + 1 + rel;
                let code = &s[i + 1..close];
                if crate::protocol::valid_shortcode(code) {
                    if cursor < i {
                        out.push(Piece::Text(&s[cursor..i]));
                    }
                    out.push(Piece::Shortcode(code));
                    cursor = close + 1;
                    i = close;
                } else {
                    i = close;
                }
            }
            None => break,
        }
    }
    if cursor < s.len() {
        out.push(Piece::Text(&s[cursor..]));
    }
    out
}

/// One inlined picture may not exceed this many data-URL characters, and all
/// of a message's together not `DM_INLINE_BUDGET`: the wrap must stay under
/// NIP-44's 64 KiB with room for the text.
pub const DM_INLINE_MAX: usize = 16 * 1024;
pub const DM_INLINE_BUDGET: usize = 40 * 1024;

/// NIP-30 tags for the custom emoji in `content`, each once, in order of first
/// use, with the codes that had to be left as text — a picture too large to
/// inline. An `https://` url (a blob server's) costs nothing and always rides.
pub fn dm_emoji_tags(
    content: &str,
    url_of: impl Fn(&str) -> Option<String>,
) -> (Vec<(String, String)>, Vec<String>) {
    let mut tags: Vec<(String, String)> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut spent = 0;
    for piece in split_shortcodes(content) {
        let Piece::Shortcode(code) = piece else {
            continue;
        };
        if tags.iter().any(|(c, _)| c == code) || skipped.iter().any(|c| c == code) {
            continue;
        }
        match url_of(code) {
            Some(url) if url.starts_with("https://") => tags.push((code.to_string(), url)),
            Some(url)
                if url.starts_with("data:")
                    && url.len() <= DM_INLINE_MAX
                    && spent + url.len() <= DM_INLINE_BUDGET =>
            {
                spent += url.len();
                tags.push((code.to_string(), url));
            }
            Some(_) => skipped.push(code.to_string()),
            None => {}
        }
    }
    (tags, skipped)
}

fn cache_dir() -> PathBuf {
    config_dir().join("emoji")
}

/// Saved emoji keep their own copy: the media cache is a cache, and leaving
/// the guild or a sweep must not take a picture the person chose to keep.
fn saved_dir() -> PathBuf {
    config_dir().join("emoji-saved")
}

pub fn load_saved(image: &str) -> Option<String> {
    let name = safe_name(image)?;
    std::fs::read_to_string(saved_dir().join(name))
        .ok()
        .or_else(|| load_cached(image))
}

pub fn store_saved(image: &str, data_url: &str) {
    let Some(name) = safe_name(image) else { return };
    let (path, data) = (saved_dir().join(name), data_url.to_string());
    std::thread::spawn(move || {
        if let Some(dir) = path.parent()
            && std::fs::create_dir_all(dir).is_ok()
        {
            let _ = std::fs::write(path, data);
        }
    });
}

pub fn remove_saved(image: &str) {
    let Some(name) = safe_name(image) else { return };
    let path = saved_dir().join(name);
    std::thread::spawn(move || {
        let _ = std::fs::remove_file(path);
    });
}

fn safe_name(image: &str) -> Option<&str> {
    let (hash, ext) = image.split_once('.')?;
    (hash.len() == 64
        && hash.chars().all(|c| c.is_ascii_hexdigit())
        && (1..=5).contains(&ext.len())
        && ext.chars().all(|c| c.is_ascii_alphanumeric()))
    .then_some(image)
}

pub fn load_cached(image: &str) -> Option<String> {
    let name = safe_name(image)?;
    let path = cache_dir().join(name);
    if std::fs::metadata(&path).ok()?.len() > 8 * 1024 * 1024 {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

pub fn store_cached(image: &str, data_url: &str) {
    let Some(name) = safe_name(image) else { return };
    let dir = cache_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let _ = std::fs::write(dir.join(name), data_url);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(s: &str) -> Vec<&str> {
        split_shortcodes(s)
            .into_iter()
            .filter_map(|p| match p {
                Piece::Shortcode(c) => Some(c),
                Piece::Text(_) => None,
            })
            .collect()
    }

    #[test]
    fn plain_text_is_left_alone() {
        assert_eq!(split_shortcodes("hello"), vec![Piece::Text("hello")]);
        assert!(codes("no colons here").is_empty());
    }

    #[test]
    fn finds_a_shortcode_and_keeps_surrounding_text() {
        assert_eq!(
            split_shortcodes("a:tada:b"),
            vec![Piece::Text("a"), Piece::Shortcode("tada"), Piece::Text("b")]
        );
    }

    #[test]
    fn handles_adjacent_punctuation_and_repeats() {
        assert_eq!(codes(":tada:!"), vec!["tada"]);
        assert_eq!(codes(":a1::b2:"), vec!["a1", "b2"]);
        assert_eq!(codes(":xx:,:yy:"), vec!["xx", "yy"]);
    }

    #[test]
    fn urls_and_times_are_not_shortcodes() {
        assert!(codes("http://example.com").is_empty());
        assert!(codes("https://x.dev/a:b").is_empty());
        assert!(codes(":Tada:").is_empty());
        assert!(codes(":not-ok:").is_empty());
    }

    #[test]
    fn rejects_degenerate_delimiters() {
        assert!(codes("::").is_empty());
        assert!(codes(":a:").is_empty(), "one char is below the minimum");
        assert!(codes(&format!(":{}:", "x".repeat(33))).is_empty());
        assert_eq!(codes(&format!(":{}:", "x".repeat(32))).len(), 1);
    }

    #[test]
    fn dm_tags_inline_small_known_emoji_once_and_report_the_rest() {
        let small = format!("data:image/png;base64,{}", "A".repeat(100));
        let big = format!("data:image/png;base64,{}", "A".repeat(DM_INLINE_MAX));
        let url_of = |code: &str| match code {
            "cat" => Some(small.clone()),
            "huge" => Some(big.clone()),
            "http" => Some("https://x/y.png".to_string()),
            _ => None,
        };
        let (tags, skipped) = dm_emoji_tags(":cat: :cat: :huge: :http: :nope: 10:30", url_of);
        assert_eq!(
            tags,
            vec![
                ("cat".to_string(), small.clone()),
                ("http".to_string(), "https://x/y.png".to_string())
            ]
        );
        assert_eq!(
            skipped,
            vec!["huge"],
            "unknown codes are plain text, not a complaint"
        );

        let mid = format!("data:image/png;base64,{}", "A".repeat(DM_INLINE_MAX - 100));
        let three = |_: &str| Some(mid.clone());
        let (tags, skipped) = dm_emoji_tags(":a1: :b2: :c3:", three);
        assert_eq!(tags.len(), 2, "the budget holds two of these");
        assert_eq!(skipped, vec!["c3"]);
    }

    #[test]
    fn safe_name_rejects_traversal() {
        let ok = format!("{}.png", "a".repeat(64));
        assert!(safe_name(&ok).is_some());
        assert!(safe_name("../../etc/passwd").is_none());
        assert!(safe_name("short.png").is_none());
        assert!(safe_name(&format!("{}.p/g", "a".repeat(64))).is_none());
    }
}
