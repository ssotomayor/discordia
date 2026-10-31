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

/// At most one ledger event per this span. The event is replaceable, so a
/// burst of points needs only its last total out, not one event per point (#198).
pub const PUBLISH_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(300);

/// Paces a replaceable event's publishes: the first goes at once, later ones
/// wait for the cooldown and only the latest waiting value is sent when it ends.
#[derive(Debug)]
pub struct Publisher<T> {
    cooldown: std::time::Duration,
    published: Option<T>,
    last_at: Option<std::time::Instant>,
    pending: Option<T>,
}

impl<T: Clone + PartialEq> Publisher<T> {
    pub fn new(cooldown: std::time::Duration) -> Self {
        Self {
            cooldown,
            published: None,
            last_at: None,
            pending: None,
        }
    }

    /// `Some` is a value to sign and send now.
    pub fn offer(&mut self, value: T, now: std::time::Instant) -> Option<T> {
        if self.published.as_ref() == Some(&value) {
            self.pending = None;
            return None;
        }
        if !self.cooled(now) {
            self.pending = Some(value);
            return None;
        }
        self.mark_sent(value.clone(), now);
        Some(value)
    }

    /// What the relays already hold, so offering it again sends nothing.
    pub fn note_held(&mut self, value: T) {
        if self.pending.as_ref() == Some(&value) {
            self.pending = None;
        }
        self.published = Some(value);
    }

    /// When the waiting value may go, if one is waiting.
    pub fn due_at(&self) -> Option<std::time::Instant> {
        self.pending.as_ref()?;
        Some(self.last_at? + self.cooldown)
    }

    /// The waiting value, once its time has come.
    pub fn take_due(&mut self, now: std::time::Instant) -> Option<T> {
        let due = self.due_at()?;
        if now < due {
            return None;
        }
        let value = self.pending.take()?;
        self.mark_sent(value.clone(), now);
        Some(value)
    }

    fn cooled(&self, now: std::time::Instant) -> bool {
        self.last_at
            .is_none_or(|t| now.duration_since(t) >= self.cooldown)
    }

    fn mark_sent(&mut self, value: T, now: std::time::Instant) {
        self.published = Some(value);
        self.last_at = Some(now);
        self.pending = None;
    }
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

#[cfg(test)]
mod publisher_tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn xp(n: u64) -> GlobalXp {
        GlobalXp { xp: n, servers: 1 }
    }

    #[test]
    fn a_burst_sends_the_first_total_now_and_the_last_one_later() {
        let t0 = Instant::now();
        let mut p = Publisher::new(PUBLISH_COOLDOWN);
        assert_eq!(p.offer(xp(10), t0), Some(xp(10)));
        assert_eq!(p.offer(xp(11), t0 + Duration::from_secs(1)), None);
        assert_eq!(p.offer(xp(12), t0 + Duration::from_secs(2)), None);
        assert_eq!(p.due_at(), Some(t0 + PUBLISH_COOLDOWN));
        assert_eq!(p.take_due(t0 + Duration::from_secs(10)), None);
        assert_eq!(p.take_due(t0 + PUBLISH_COOLDOWN), Some(xp(12)));
        assert_eq!(p.due_at(), None, "nothing left waiting");
    }

    #[test]
    fn an_unchanged_total_is_never_resent() {
        let t0 = Instant::now();
        let mut p = Publisher::new(PUBLISH_COOLDOWN);
        assert_eq!(p.offer(xp(10), t0), Some(xp(10)));
        assert_eq!(p.offer(xp(11), t0 + Duration::from_secs(1)), None);
        assert_eq!(p.offer(xp(10), t0 + Duration::from_secs(2)), None);
        assert_eq!(
            p.due_at(),
            None,
            "back to the published total: nothing to send"
        );
        assert_eq!(p.offer(xp(10), t0 + PUBLISH_COOLDOWN * 2), None);
    }

    #[test]
    fn what_the_relays_already_hold_is_not_sent_again() {
        let t0 = Instant::now();
        let mut p = Publisher::new(PUBLISH_COOLDOWN);
        p.note_held(xp(10));
        assert_eq!(p.offer(xp(10), t0), None);
        assert_eq!(
            p.offer(xp(11), t0),
            Some(xp(11)),
            "the first change still goes at once"
        );
    }

    #[test]
    fn after_the_cooldown_a_new_total_goes_at_once() {
        let t0 = Instant::now();
        let mut p = Publisher::new(PUBLISH_COOLDOWN);
        assert_eq!(p.offer(xp(10), t0), Some(xp(10)));
        let later = t0 + PUBLISH_COOLDOWN + Duration::from_secs(1);
        assert_eq!(p.offer(xp(20), later), Some(xp(20)));
        assert_eq!(p.offer(xp(21), later + Duration::from_secs(1)), None);
        assert_eq!(p.due_at(), Some(later + PUBLISH_COOLDOWN));
    }
}
