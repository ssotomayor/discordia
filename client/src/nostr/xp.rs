//! Kind 30078 — NIP-78 application data, carrying a level that spans servers.
//!
//! **This number has no authority behind it and the UI must never pretend it
//! does.** A per-guild level is counted by the server that saw the messages; a
//! global one is a sum its owner computes and signs for themselves, so anybody
//! can publish nine thousand. It ranks with a kind 0 name: shown, attributed,
//! and never allowed to gate anything. Permissions read roles, never this.
//!
//! What is deliberately *not* here is which servers contributed. A list of them
//! is a list of the communities someone is in, published to public relays and
//! readable forever — the number costs nothing to share and the list costs a
//! lot, so only the count of them travels.

use secp256k1::SecretKey;
use serde::{Deserialize, Serialize};

use super::event::{self, Event};

/// Parameterized replaceable: a relay holds one per key *per `d` tag*, which is
/// what lets one app's data sit beside another's under the same kind.
pub const KIND_APP_DATA: u16 = 30078;

/// Ours, and namespaced because the kind is shared with every other app.
pub const D_TAG: &str = "discordia:xp";

/// Past this the number is a claim about a lifetime nobody has, and treating it
/// as a `u64` in the level curve is a long loop. Clamped, never rejected.
const MAX_XP: u64 = 100_000_000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlobalXp {
    #[serde(default)]
    pub xp: u64,
    /// How many servers went into the sum. Context for the reader, and the one
    /// thing that distinguishes a number earned widely from one earned in a
    /// single room.
    #[serde(default)]
    pub servers: u32,
}

pub fn xp_event(secret: &SecretKey, xp: &GlobalXp, now: i64) -> Event {
    let content = serde_json::to_string(&GlobalXp {
        xp: xp.xp.min(MAX_XP),
        servers: xp.servers,
    })
    .unwrap_or_else(|_| "{}".to_string());
    event::sign_with(
        secret,
        now,
        KIND_APP_DATA,
        vec![vec!["d".to_string(), D_TAG.to_string()]],
        content,
    )
}

/// `None` for anything that is not ours: the kind is shared, so the `d` tag is
/// the only thing separating our content from another app's.
pub fn parse_xp(event: &Event) -> Option<GlobalXp> {
    if event.kind != KIND_APP_DATA {
        return None;
    }
    let ours = event.tags.iter().any(|t| {
        t.first().map(String::as_str) == Some("d") && t.get(1).map(String::as_str) == Some(D_TAG)
    });
    if !ours {
        return None;
    }
    let parsed: GlobalXp = serde_json::from_str(&event.content).ok()?;
    Some(GlobalXp {
        xp: parsed.xp.min(MAX_XP),
        servers: parsed.servers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(seed: u8) -> SecretKey {
        SecretKey::from_slice(&[seed; 32]).expect("valid key")
    }

    #[test]
    fn a_total_round_trips_and_is_signed() {
        let mine = GlobalXp {
            xp: 1_234,
            servers: 3,
        };
        let e = xp_event(&key(1), &mine, 1_700_000_000);
        assert!(e.verify());
        assert_eq!(parse_xp(&e), Some(mine));
    }

    #[test]
    fn another_apps_note_under_the_same_kind_is_not_ours() {
        let e = event::sign_with(
            &key(1),
            1,
            KIND_APP_DATA,
            vec![vec!["d".into(), "someone-else:prefs".into()]],
            r#"{"xp":9000}"#.into(),
        );
        assert_eq!(parse_xp(&e), None);
    }

    #[test]
    fn an_untagged_note_is_not_ours_either() {
        let e = event::sign_with(&key(1), 1, KIND_APP_DATA, vec![], r#"{"xp":1}"#.into());
        assert_eq!(parse_xp(&e), None);
    }

    #[test]
    fn a_wild_claim_is_clamped_rather_than_dropped() {
        let e = event::sign_with(
            &key(1),
            1,
            KIND_APP_DATA,
            vec![vec!["d".into(), D_TAG.into()]],
            format!(r#"{{"xp":{}}}"#, u64::MAX),
        );
        assert_eq!(parse_xp(&e).map(|x| x.xp), Some(MAX_XP));
    }

    #[test]
    fn a_missing_field_reads_as_zero_rather_than_failing() {
        let e = event::sign_with(
            &key(1),
            1,
            KIND_APP_DATA,
            vec![vec!["d".into(), D_TAG.into()]],
            "{}".into(),
        );
        assert_eq!(parse_xp(&e), Some(GlobalXp::default()));
    }
}
