use std::path::PathBuf;

use crate::identity::config_dir;

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

pub fn has_shortcode(s: &str) -> bool {
    s.matches(':').count() >= 2
        && split_shortcodes(s)
            .iter()
            .any(|p| matches!(p, Piece::Shortcode(_)))
}

fn cache_dir() -> PathBuf {
    config_dir().join("emoji")
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
    std::fs::read_to_string(cache_dir().join(name)).ok()
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
