//! Custom-emoji support: the on-disk image cache and the `:shortcode:` scanner.
//!
//! Images are never pushed with the catalog. They arrive on demand via
//! `FetchEmoji`/`EmojiBlobs` and are cached here, keyed by the content address
//! the server assigned (`<sha256>.<ext>`). Because the name *is* the hash of the
//! bytes, the cache never needs invalidating — an entry is either right or
//! absent, so there is no staleness to reason about and no expiry to tune.

use std::path::PathBuf;

use crate::identity::config_dir;

/// One piece of a word: either literal text, or a custom-emoji shortcode with
/// its delimiting colons stripped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece<'a> {
    Text(&'a str),
    Shortcode(&'a str),
}

/// Split a word into text and `:shortcode:` runs.
///
/// This runs *after* the renderer's URL branch, which matters: `http://host`
/// contains a colon pair that would otherwise be a tempting match. Requiring
/// both delimiters and `valid_shortcode`'s narrow character set means the
/// remaining false-positive surface is text that genuinely looks like an
/// emoji reference.
///
/// A trailing colon can open the next shortcode (`:a::b:` is `:a:` then `:b:`),
/// so scanning restarts at the closing colon rather than after it.
pub fn split_shortcodes(s: &str) -> Vec<Piece<'_>> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut cursor = 0; // start of the pending Text run
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
                    // Restart *at* the closing colon so `:a::b:` yields both.
                    i = close;
                } else {
                    // Not a shortcode — the closing colon may still open one.
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

/// True if the word contains at least one shortcode — lets the renderer keep
/// its cheap path for the overwhelming majority of words.
pub fn has_shortcode(s: &str) -> bool {
    s.matches(':').count() >= 2
        && split_shortcodes(s)
            .iter()
            .any(|p| matches!(p, Piece::Shortcode(_)))
}

fn cache_dir() -> PathBuf {
    config_dir().join("emoji")
}

/// Reject anything that isn't `<64 hex>.<short alnum ext>` before it reaches
/// the filesystem. The name comes from the server, and a self-hosted server is
/// not automatically trusted with our path separators.
fn safe_name(image: &str) -> Option<&str> {
    let (hash, ext) = image.split_once('.')?;
    (hash.len() == 64
        && hash.chars().all(|c| c.is_ascii_hexdigit())
        && (1..=5).contains(&ext.len())
        && ext.chars().all(|c| c.is_ascii_alphanumeric()))
    .then_some(image)
}

/// Read a cached emoji image (as the `data:` URL an `<img src>` wants).
pub fn load_cached(image: &str) -> Option<String> {
    let name = safe_name(image)?;
    std::fs::read_to_string(cache_dir().join(name)).ok()
}

/// Cache an emoji image. Best-effort: a failure here costs a re-fetch next
/// launch, nothing more, so it is never worth surfacing.
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

    /// Punctuation right after the closing colon is what the old word-splitting
    /// approach could not handle.
    #[test]
    fn handles_adjacent_punctuation_and_repeats() {
        assert_eq!(codes(":tada:!"), vec!["tada"]);
        assert_eq!(codes(":a1::b2:"), vec!["a1", "b2"]);
        assert_eq!(codes(":xx:,:yy:"), vec!["xx", "yy"]);
    }

    /// A URL reaching the scanner (defence in depth — the renderer's URL branch
    /// should catch it first) must not be chewed up.
    #[test]
    fn urls_and_times_are_not_shortcodes() {
        assert!(codes("http://example.com").is_empty());
        assert!(codes("https://x.dev/a:b").is_empty());
        // Uppercase and hyphens are outside our charset on purpose.
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
    fn has_shortcode_matches_the_scanner() {
        assert!(has_shortcode("hey :tada: there"));
        assert!(!has_shortcode("hey there"));
        assert!(!has_shortcode("ratio 3:1"));
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
