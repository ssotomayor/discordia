//! Kind 30078 `discordia:read`: read marks follow the key across devices,
//! encrypted to ourselves, since who we talk to and when we read is no relay's
//! business. No Discordia server ever holds them.

use std::collections::BTreeMap;

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
