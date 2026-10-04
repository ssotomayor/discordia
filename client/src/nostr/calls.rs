use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::event::{self, Rumor};

pub const KIND_CALL: u16 = 24133;
pub const TTL: i64 = 60;
pub const MAX_CLOCK_SKEW: i64 = 300;
pub const CLOCK_DEADBAND: i64 = 60;
pub const MAX_SDP: usize = 24_000;

pub fn sender_now(now: i64, learned_offset: i64) -> i64 {
    now.saturating_add(learned_offset.clamp(-MAX_CLOCK_SKEW, MAX_CLOCK_SKEW))
}

pub fn relay_expiration(sent_at: i64) -> i64 {
    // Relay retention tolerates skew; authenticated inner signals still expire after TTL.
    sent_at.saturating_add(TTL).saturating_add(MAX_CLOCK_SKEW)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Body {
    Invite,
    Accept,
    Offer { sdp: String },
    Answer { sdp: String },
    End { reason: EndReason },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndReason {
    Hangup,
    Declined,
    Busy,
    Timeout,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Signal {
    pub version: u8,
    pub call_id: Uuid,
    pub device: Uuid,
    pub target: Option<Uuid>,
    pub sent_at: i64,
    pub body: Body,
}

impl Signal {
    pub fn validate(&self, now: i64) -> Result<(), String> {
        if self.version != 1 || self.call_id.is_nil() || self.device.is_nil() {
            return Err("Unsupported call signal".into());
        }
        if self.sent_at < now.saturating_sub(TTL)
            || self.sent_at > now.saturating_add(CLOCK_DEADBAND)
        {
            return Err("Expired call signal or clock mismatch".into());
        }
        match &self.body {
            Body::Offer { sdp } | Body::Answer { sdp }
                if sdp.len() > MAX_SDP
                    || !sdp.starts_with("v=0\r\n")
                    || !sdp.contains("a=fingerprint:sha-256 ")
                    || !sdp.contains("m=audio ")
                    || sdp.contains("m=video ")
                    || sdp.contains("m=application ")
                    || sdp.lines().filter(|line| line.starts_with("m=")).count() != 1 =>
            {
                return Err("Invalid voice call description".into());
            }
            _ => {}
        }
        Ok(())
    }
}

pub fn rumor(author: &str, recipient: &str, signal: &Signal) -> Result<Rumor, String> {
    signal.validate(signal.sent_at)?;
    Ok(event::rumor(
        author,
        signal.sent_at,
        KIND_CALL,
        vec![
            vec!["p".into(), recipient.into()],
            vec!["d".into(), "discordia/voice-call/1".into()],
        ],
        serde_json::to_string(signal).map_err(|e| e.to_string())?,
    ))
}

pub fn open(rumor: &Rumor, recipient: &str, now: i64) -> Result<Signal, String> {
    if rumor.kind != KIND_CALL
        || rumor.tag("p") != Some(recipient)
        || rumor.tag("d") != Some("discordia/voice-call/1")
        || rumor.content.len() > MAX_SDP * 2 + 1024
    {
        return Err("Not a voice call addressed to us".into());
    }
    let signal: Signal = serde_json::from_str(&rumor.content).map_err(|e| e.to_string())?;
    if signal.sent_at != rumor.created_at {
        return Err("Call timestamp mismatch".into());
    }
    signal.validate(now)?;
    Ok(signal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nostr::{nip17, nip59};
    use secp256k1::SecretKey;

    fn invite() -> Signal {
        Signal {
            version: 1,
            call_id: Uuid::new_v4(),
            device: Uuid::new_v4(),
            target: None,
            sent_at: 1_800_000_000,
            body: Body::Invite,
        }
    }

    #[test]
    fn encrypted_call_is_authenticated_and_never_a_chat_message() {
        let alice = SecretKey::from_slice(&[1; 32]).unwrap();
        let bob = SecretKey::from_slice(&[2; 32]).unwrap();
        let a = event::xonly_hex(&alice);
        let b = event::xonly_hex(&bob);
        let signal = invite();
        let rumor = rumor(&a, &b, &signal).unwrap();
        let wrap = nip59::wrap_with_expiration(
            &alice,
            &b,
            &rumor,
            signal.sent_at,
            Some(relay_expiration(signal.sent_at)),
        )
        .unwrap();
        assert_eq!(
            wrap.tag("expiration"),
            Some(relay_expiration(signal.sent_at).to_string().as_str())
        );
        assert_ne!(wrap.pubkey, a);
        let opened = nip59::unwrap(&bob, &wrap).unwrap();
        assert_eq!(opened.pubkey, a);
        assert_eq!(open(&opened, &b, signal.sent_at).unwrap(), signal);
        assert!(nip17::open_chat(&bob, &b, &wrap).is_err());
        assert!(open(&opened, &a, signal.sent_at).is_err());
        assert!(open(&opened, &b, signal.sent_at + TTL + 1).is_err());
    }

    #[test]
    fn freshness_uses_inner_timestamp_and_bounds_clock_skew() {
        let signal = invite();
        assert!(signal.validate(signal.sent_at + TTL).is_ok());
        assert!(signal.validate(signal.sent_at + TTL + 1).is_err());
        assert!(signal.validate(signal.sent_at - CLOCK_DEADBAND).is_ok());
        assert!(
            signal
                .validate(signal.sent_at - CLOCK_DEADBAND - 1)
                .is_err()
        );
    }

    #[test]
    fn learned_clock_offsets_preserve_freshness_in_both_directions() {
        let signal = invite();
        for offset in [-94, 94] {
            let local_now = signal.sent_at - offset;
            let decoded = rumor("sender", "recipient", &signal).unwrap();
            assert!(open(&decoded, "recipient", sender_now(local_now, offset)).is_ok());
            assert!(
                signal
                    .validate(sender_now(local_now + TTL + 1, offset))
                    .is_err()
            );
            assert!(relay_expiration(signal.sent_at) > local_now + TTL);
        }
        assert!(
            signal
                .validate(sender_now(signal.sent_at - 1000, 1000))
                .is_err()
        );
        assert!(
            signal
                .validate(sender_now(signal.sent_at + 1000, -1000))
                .is_err()
        );
        assert_eq!(sender_now(i64::MAX, 94), i64::MAX);
        assert_eq!(relay_expiration(i64::MAX), i64::MAX);
    }

    #[test]
    fn oversized_and_non_voice_descriptions_are_rejected() {
        let mut signal = invite();
        for sdp in ["x".repeat(MAX_SDP + 1), "v=0\r\nm=video 9\r\n".into()] {
            signal.body = Body::Offer { sdp };
            assert!(signal.validate(signal.sent_at).is_err());
        }
    }
}
