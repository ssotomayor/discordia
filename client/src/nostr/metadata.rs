//! Kind 0 — the name a key publishes for itself.
//!
//! Named for what it is rather than for its NIP: kind 0 is part of NIP-01, and
//! `event` already owns that spec's ids and signatures. This is the one field
//! of it this client wants, which is a name for somebody who is in no roster —
//! a DM peer you share no server with was otherwise permanently `abcd1234…ef01`
//! even though their name was sitting on the same relays the messages came
//! from.
//!
//! **The content is a stranger's string, and it is treated like one.** Every
//! other name in this app comes from somewhere with an owner: a gateway
//! username belongs to a server that can moderate it, a petname you typed
//! yourself. A kind 0 name is asserted by whoever holds the key and checked by
//! nobody, so it ranks below both in `AppState::display_name` and it is
//! sanitised here rather than at the render sites — `sanitize` is what stands
//! between a relay and the strings this UI draws.
//!
//! What that does *not* claim to solve is impersonation: two keys may publish
//! the same name, and the answer to that is the same one gateway usernames
//! already need — those are mutable and non-unique too — the key-derived
//! `#discriminator` and signature colour drawn beside every name.

use super::event::Event;

/// NIP-01 metadata, replaceable: a relay holds one per key.
pub const KIND_METADATA: u16 = 0;

/// Longest name this will hand to the UI.
///
/// A name is drawn in a fixed-width sidebar row and a message header, so an
/// unbounded one is a layout the publisher controls. Cut rather than rejected:
/// somebody with a long name should still be recognisable.
const MAX_NAME_CHARS: usize = 48;

/// Characters removed outright rather than collapsed.
///
/// Two families, both chosen because they are *invisible* and therefore cannot
/// be judged by the reader:
///
/// - **Bidi overrides** (U+200E/200F, U+202A–202E, U+2066–2069) reorder the
///   glyphs after them, which is the classic way to make one string read as
///   another.
/// - **Zero-width space and BOM** (U+200B, U+FEFF) pad a name invisibly, so two
///   keys can publish names that render identically.
///
/// ZWNJ and ZWJ (U+200C/200D) are deliberately *not* here: they are load-bearing
/// in Persian and Devanagari and in every multi-person emoji, so removing them
/// would corrupt honest names to inconvenience dishonest ones.
fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{200B}'
            | '\u{200E}'
            | '\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
            | '\u{00AD}'
    )
}

/// Turn a published name into one this UI can draw, or `None` if there is
/// nothing left of it.
///
/// Control characters — newlines above all — are the reason this is not a
/// `trim()`. A name containing `\n` occupies two rows and can spell out a
/// second line of interface next to itself; the same goes for a tab in a
/// flex row. Every run of whitespace becomes one space, so a name cannot be
/// indented into the middle of the column either.
pub fn sanitize(raw: &str) -> Option<String> {
    let mut out = String::new();
    let mut pending_space = false;
    for c in raw.chars() {
        if is_invisible(c) {
            continue;
        }
        if c.is_whitespace() {
            // Held rather than pushed, so a trailing run leaves nothing behind
            // and a leading one is never opened.
            pending_space = !out.is_empty();
            continue;
        }
        if c.is_control() {
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        if out.chars().count() >= MAX_NAME_CHARS {
            break;
        }
        out.push(c);
    }
    if out.is_empty() { None } else { Some(out) }
}

/// The name in a kind 0 event, sanitised.
///
/// `display_name` wins over `name` because that is what the field was added
/// for: `name` is the NIP-01 handle (often lowercase, often a slug) and
/// `display_name` is what its owner wants read. `displayName` is the same
/// field under the spelling older clients wrote, and enough of it is still on
/// relays to be worth reading.
pub fn name_from(event: &Event) -> Option<String> {
    if event.kind != KIND_METADATA {
        return None;
    }
    let json = serde_json::from_str::<serde_json::Value>(&event.content).ok()?;
    ["display_name", "displayName", "name"]
        .into_iter()
        .filter_map(|k| json.get(k)?.as_str())
        .find_map(sanitize)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata(content: &str) -> Event {
        let secret = secp256k1::SecretKey::from_slice(&[9; 32]).expect("valid key");
        super::super::event::sign_with(
            &secret,
            1_700_000_000,
            KIND_METADATA,
            vec![],
            content.to_string(),
        )
    }

    /// The ordinary case, and the field precedence: a slug handle plus the name
    /// its owner would rather be called.
    #[test]
    fn the_display_name_wins_over_the_handle() {
        let e = metadata(r#"{"name":"ana_r","display_name":"Ana Rivas","about":"hi"}"#);
        assert_eq!(name_from(&e).as_deref(), Some("Ana Rivas"));
    }

    /// Older clients wrote the same field in camelCase and those events are
    /// still on relays.
    #[test]
    fn the_legacy_spelling_is_read_too() {
        let e = metadata(r#"{"name":"ana_r","displayName":"Ana"}"#);
        assert_eq!(name_from(&e).as_deref(), Some("Ana"));
    }

    /// With no display name the handle is the name, because it is the only one
    /// offered.
    #[test]
    fn the_handle_is_used_when_it_is_all_there_is() {
        let e = metadata(r#"{"name":"ana_r"}"#);
        assert_eq!(name_from(&e).as_deref(), Some("ana_r"));
    }

    /// A present-but-useless `display_name` must fall through to `name` rather
    /// than winning with nothing — the precedence is over *usable* values.
    #[test]
    fn an_empty_display_name_falls_through_to_the_handle() {
        let e = metadata(r#"{"display_name":"   ","name":"ana_r"}"#);
        assert_eq!(name_from(&e).as_deref(), Some("ana_r"));
    }

    /// Kind 0 content is free-form JSON published by a stranger, so none of
    /// this may be an error: it is simply a key with no name.
    #[test]
    fn unusable_content_yields_no_name() {
        assert_eq!(name_from(&metadata("not json")), None);
        assert_eq!(name_from(&metadata("{}")), None);
        assert_eq!(name_from(&metadata(r#"{"name":42}"#)), None);
        assert_eq!(name_from(&metadata(r#"{"name":""}"#)), None);
    }

    /// Another kind reaching this by mistake must not be read as a name — the
    /// subscription asks for kind 0, but the pool delivers every subscription
    /// on one channel.
    #[test]
    fn only_kind_zero_carries_a_name() {
        let mut e = metadata(r#"{"name":"ana"}"#);
        e.kind = 1;
        assert_eq!(name_from(&e), None);
    }

    /// A newline would give the publisher a second row of interface beside
    /// their own name.
    #[test]
    fn a_name_cannot_span_two_lines() {
        assert_eq!(
            sanitize("Ana\nadmin — verified").as_deref(),
            Some("Ana admin — verified")
        );
        assert_eq!(sanitize("Ana\t\tRivas").as_deref(), Some("Ana Rivas"));
        assert_eq!(sanitize("  Ana  ").as_deref(), Some("Ana"));
    }

    /// Invisible characters are the ones a reader cannot judge, so they go —
    /// but not the ones honest names in other scripts need.
    #[test]
    fn invisible_characters_are_removed_and_useful_ones_are_not() {
        assert_eq!(sanitize("A\u{202E}na").as_deref(), Some("Ana"));
        assert_eq!(sanitize("An\u{200B}a").as_deref(), Some("Ana"));
        assert_eq!(sanitize("\u{FEFF}Ana").as_deref(), Some("Ana"));
        // ZWJ holds a family emoji together; ZWNJ separates Persian letters.
        assert_eq!(
            sanitize("\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}").as_deref(),
            Some("\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}")
        );
        assert_eq!(
            sanitize("\u{645}\u{200C}\u{6CC}").as_deref(),
            Some("\u{645}\u{200C}\u{6CC}")
        );
    }

    /// A name that is nothing but invisible characters is not a name.
    #[test]
    fn a_name_made_only_of_nothing_is_no_name() {
        assert_eq!(sanitize("\u{200B}\u{202E}  \n"), None);
        assert_eq!(sanitize(""), None);
    }

    /// The cap is on characters, not bytes, so a name in a non-Latin script
    /// gets the same room as one in Latin.
    #[test]
    fn a_long_name_is_cut_to_the_cap() {
        let cut = sanitize(&"ñ".repeat(200)).expect("some name");
        assert_eq!(cut.chars().count(), MAX_NAME_CHARS);
    }
}
