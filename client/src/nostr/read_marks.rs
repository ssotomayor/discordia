//! Kind 30078 `discordia:read`: read marks follow the key across devices,
//! encrypted to ourselves, since who we talk to and when we read is no relay's
//! business. No Discordia server ever holds them.

use std::collections::BTreeMap;
use std::time::Instant;

use secp256k1::SecretKey;
use serde::{Deserialize, Serialize};

use super::event::{self, Event};
use super::nip44;
use crate::protocol::Id;

pub use super::xp::KIND_APP_DATA;

pub const D_TAG: &str = "discordia:read";

/// Marks only move forward, so a burst of reading needs only its last state
/// out; replaceable, so each publish is a write on every relay (#198).
pub const PUBLISH_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(60);

/// Plaintext bytes. NIP-44 refuses past 65535 and base64 adds a third, which
/// some relays' event caps would not take.
const BUDGET: usize = 40_000;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadMarks {
    /// Peer → second of the newest DM read, on the *peer's* clock: devices
    /// learn slightly different offsets for a skewed peer (trap 18).
    #[serde(default)]
    pub dms: BTreeMap<String, i64>,
    /// Server origin → channel → ms of the newest message read.
    #[serde(default)]
    pub channels: BTreeMap<String, BTreeMap<Id, i64>>,
}

impl ReadMarks {
    fn covers(&self, other: &Self) -> bool {
        other
            .dms
            .iter()
            .all(|(peer, at)| self.dms.get(peer).is_some_and(|mine| mine >= at))
            && other.channels.iter().all(|(server, channels)| {
                channels.iter().all(|(channel, at)| {
                    self.channels
                        .get(server)
                        .and_then(|mine| mine.get(channel))
                        .is_some_and(|mine| mine >= at)
                })
            })
    }

    /// Drops the oldest marks until it fits: a device that loses one here
    /// still holds its own, and the others only re-baseline that conversation.
    fn trimmed(&self, budget: usize) -> ReadMarks {
        let mut marks = self.clone();
        let mut oldest: Vec<(i64, String, Option<Id>)> = marks
            .dms
            .iter()
            .map(|(peer, at)| (at.saturating_mul(1000), peer.clone(), None))
            .chain(marks.channels.iter().flat_map(|(server, channels)| {
                channels
                    .iter()
                    .map(|(id, at)| (*at, server.clone(), Some(*id)))
            }))
            .collect();
        oldest.sort();
        let mut oldest = oldest.into_iter();
        // Freed bytes are estimated, so it re-measures rather than serializing
        // once per mark dropped.
        while let Some(over) = serde_json::to_string(&marks)
            .map_or(0, |json| json.len())
            .checked_sub(budget)
            .filter(|over| *over > 0)
        {
            let mut freed = 0;
            while freed < over {
                let Some((_, key, channel)) = oldest.next() else {
                    return marks;
                };
                freed += key.len().max(36) + 24;
                match channel {
                    None => {
                        marks.dms.remove(&key);
                    }
                    Some(id) => {
                        if let Some(channels) = marks.channels.get_mut(&key) {
                            channels.remove(&id);
                            if channels.is_empty() {
                                marks.channels.remove(&key);
                            }
                        }
                    }
                }
            }
        }
        marks
    }
}

#[derive(Default)]
pub struct Publisher {
    held: ReadMarks,
    held_at: i64,
    pending: Option<(ReadMarks, Event)>,
    last_sent: Option<Instant>,
}

impl Publisher {
    pub fn flush(&mut self) {
        self.last_sent = None;
    }

    pub fn offer(
        &mut self,
        marks: ReadMarks,
        secret: &SecretKey,
        owner: &str,
        timestamp: i64,
        now: Instant,
    ) -> Result<Option<Event>, String> {
        let marks = marks.trimmed(BUDGET);
        if marks == self.held
            || self
                .last_sent
                .is_some_and(|last| now.duration_since(last) < PUBLISH_COOLDOWN)
        {
            return Ok(None);
        }
        let event = match self.pending.as_ref() {
            Some((pending, event)) if pending == &marks => event.clone(),
            _ => marks_event(
                secret,
                owner,
                &marks,
                timestamp.max(self.held_at.saturating_add(1)),
            )?,
        };
        self.pending = Some((marks, event.clone()));
        self.last_sent = Some(now);
        Ok(Some(event))
    }

    pub fn accepted(&mut self, id: &str) {
        if self
            .pending
            .as_ref()
            .is_some_and(|(_, event)| event.id == id)
            && let Some((marks, event)) = self.pending.take()
        {
            self.held = marks;
            self.held_at = event.created_at;
        }
    }

    pub fn note_held(&mut self, marks: ReadMarks, timestamp: i64) {
        if self
            .pending
            .as_ref()
            .is_some_and(|(pending, event)| marks.covers(pending) || event.created_at <= timestamp)
        {
            self.pending = None;
        }
        self.held = marks;
        self.held_at = timestamp;
    }
}

pub fn marks_event(
    secret: &SecretKey,
    our_pubkey: &str,
    marks: &ReadMarks,
    now: i64,
) -> Result<Event, String> {
    let key = nip44::conversation_key(secret, our_pubkey)?;
    let json = serde_json::to_string(&marks.trimmed(BUDGET)).map_err(|e| e.to_string())?;
    Ok(event::sign_with(
        secret,
        now,
        KIND_APP_DATA,
        vec![vec!["d".to_string(), D_TAG.to_string()]],
        nip44::encrypt(&key, &json)?,
    ))
}

/// `None` for anything that is not our own marks event. Decrypting is the
/// proof: only our key derives the conversation key with itself.
pub fn parse_marks(secret: &SecretKey, our_pubkey: &str, event: &Event) -> Option<ReadMarks> {
    if event.kind != KIND_APP_DATA || event.pubkey != our_pubkey || event.tag("d") != Some(D_TAG) {
        return None;
    }
    let key = nip44::conversation_key(secret, our_pubkey).ok()?;
    let json = nip44::decrypt(&key, &event.content).ok()?;
    let mut marks: ReadMarks = serde_json::from_str(&json).ok()?;
    marks
        .dms
        .retain(|peer, _| crate::protocol::is_canonical_pubkey(peer));
    Some(marks)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(seed: u8) -> (SecretKey, String) {
        let secret = SecretKey::from_slice(&[seed; 32]).expect("valid key");
        let pubkey = event::xonly_hex(&secret);
        (secret, pubkey)
    }

    fn dm_marks(at: i64) -> ReadMarks {
        ReadMarks {
            dms: [("a".repeat(64), at)].into(),
            ..Default::default()
        }
    }

    #[test]
    fn publication_retries_the_same_event_until_a_relay_confirms_it() {
        let (secret, owner) = identity(7);
        let now = Instant::now();
        let mut publisher = Publisher::default();
        let marks = dm_marks(200);
        let event = publisher
            .offer(marks.clone(), &secret, &owner, 1_000, now)
            .unwrap()
            .unwrap();
        assert!(
            publisher
                .offer(
                    marks.clone(),
                    &secret,
                    &owner,
                    1_001,
                    now + PUBLISH_COOLDOWN / 2
                )
                .unwrap()
                .is_none()
        );
        let retried = publisher
            .offer(
                marks.clone(),
                &secret,
                &owner,
                1_060,
                now + PUBLISH_COOLDOWN,
            )
            .unwrap()
            .unwrap();
        assert_eq!(retried, event);
        publisher.accepted("unrelated-publication");
        assert!(
            publisher
                .offer(
                    marks.clone(),
                    &secret,
                    &owner,
                    1_120,
                    now + PUBLISH_COOLDOWN * 2
                )
                .unwrap()
                .is_some()
        );
        publisher.accepted(&event.id);
        assert!(
            publisher
                .offer(marks, &secret, &owner, 1_180, now + PUBLISH_COOLDOWN * 3)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn the_latest_read_state_replaces_a_failed_publication() {
        let (secret, owner) = identity(7);
        let now = Instant::now();
        let mut publisher = Publisher::default();
        let first = publisher
            .offer(dm_marks(100), &secret, &owner, 1_000, now)
            .unwrap()
            .unwrap();
        let next = publisher
            .offer(
                dm_marks(200),
                &secret,
                &owner,
                1_060,
                now + PUBLISH_COOLDOWN,
            )
            .unwrap()
            .unwrap();
        assert_ne!(first.id, next.id);
        publisher.accepted(&first.id);
        assert!(
            publisher
                .offer(
                    dm_marks(200),
                    &secret,
                    &owner,
                    1_120,
                    now + PUBLISH_COOLDOWN * 2
                )
                .unwrap()
                .is_some()
        );
        publisher.accepted(&next.id);
        assert!(
            publisher
                .offer(
                    dm_marks(200),
                    &secret,
                    &owner,
                    1_180,
                    now + PUBLISH_COOLDOWN * 3
                )
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn reconnecting_with_a_newer_stale_copy_reissues_marks_with_a_newer_timestamp() {
        let (secret, owner) = identity(7);
        let now = Instant::now();
        let mut publisher = Publisher::default();
        let first = publisher
            .offer(dm_marks(200), &secret, &owner, 1_000, now)
            .unwrap()
            .unwrap();
        publisher.note_held(dm_marks(100), 1_100);
        let next = publisher
            .offer(
                dm_marks(200),
                &secret,
                &owner,
                1_050,
                now + PUBLISH_COOLDOWN,
            )
            .unwrap()
            .unwrap();
        assert_ne!(first.id, next.id);
        assert!(next.created_at > 1_100);
        publisher.note_held(dm_marks(300), 1_200);
        assert!(
            publisher
                .offer(
                    dm_marks(300),
                    &secret,
                    &owner,
                    1_200,
                    now + PUBLISH_COOLDOWN * 2
                )
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn relay_marks_skip_republishing_but_the_first_read_change_goes_immediately() {
        let (secret, owner) = identity(7);
        let now = Instant::now();
        let mut publisher = Publisher::default();
        publisher.note_held(dm_marks(100), 1_000);
        assert!(
            publisher
                .offer(dm_marks(100), &secret, &owner, 1_000, now)
                .unwrap()
                .is_none()
        );
        assert!(
            publisher
                .offer(dm_marks(200), &secret, &owner, 1_000, now)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn leaving_the_app_flushes_newer_reads_without_waiting_for_the_cooldown() {
        let (secret, owner) = identity(7);
        let now = Instant::now();
        let mut publisher = Publisher::default();
        let first = publisher
            .offer(dm_marks(100), &secret, &owner, 1_000, now)
            .unwrap()
            .unwrap();
        publisher.accepted(&first.id);
        assert!(
            publisher
                .offer(
                    dm_marks(200),
                    &secret,
                    &owner,
                    1_001,
                    now + std::time::Duration::from_secs(1)
                )
                .unwrap()
                .is_none()
        );
        publisher.flush();
        let next = publisher
            .offer(
                dm_marks(200),
                &secret,
                &owner,
                1_001,
                now + std::time::Duration::from_secs(1),
            )
            .unwrap()
            .unwrap();
        assert_eq!(parse_marks(&secret, &owner, &next), Some(dm_marks(200)));
        publisher.accepted(&next.id);
        publisher.flush();
        assert!(
            publisher
                .offer(
                    dm_marks(200),
                    &secret,
                    &owner,
                    1_002,
                    now + std::time::Duration::from_secs(2)
                )
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn marks_round_trip_encrypted_and_only_for_their_owner() {
        let (secret, pubkey) = identity(7);
        let mut marks = ReadMarks::default();
        marks.dms.insert("a".repeat(64), 100);
        marks
            .channels
            .entry("quic://host".into())
            .or_default()
            .insert(Id::new_v4(), 5_000);
        let event = marks_event(&secret, &pubkey, &marks, 1_000).unwrap();
        assert!(
            !event.content.contains(&"a".repeat(64)),
            "the relay reads nothing"
        );
        assert_eq!(parse_marks(&secret, &pubkey, &event), Some(marks));

        let (other, other_pubkey) = identity(9);
        assert_eq!(parse_marks(&other, &other_pubkey, &event), None);
    }

    #[test]
    fn an_oversized_set_sheds_its_oldest_marks_first() {
        let mut marks = ReadMarks::default();
        for i in 0..2_000 {
            marks.dms.insert(format!("{i:064x}"), 1_000 + i);
        }
        let fitted = marks.trimmed(BUDGET);
        assert!(serde_json::to_string(&fitted).unwrap().len() <= BUDGET);
        assert!(fitted.dms.contains_key(&format!("{:064x}", 1_999)));
        assert!(!fitted.dms.contains_key(&format!("{:064x}", 0)));
    }
}
