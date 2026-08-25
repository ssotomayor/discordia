//! Every message is wrapped twice, to them and to us: a wrap opens only for
//! the key it was addressed to, including for its sender.

use secp256k1::SecretKey;

use super::event::{self, Event, Rumor};
use super::nip59;

pub const KIND_CHAT: u16 = 14;
pub const KIND_DM_RELAYS: u16 = 10050;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    pub id: String,
    pub author: String,
    pub peer: String,
    pub content: String,
    pub created_at: i64,
    pub reply_to: Option<String>,
}

pub fn chat_rumor(
    from_pubkey: &str,
    to_pubkey: &str,
    text: &str,
    reply_to: Option<&str>,
    now: i64,
) -> Rumor {
    let mut tags = vec![vec!["p".to_string(), to_pubkey.to_string()]];
    if let Some(id) = reply_to {
        tags.push(vec![
            "e".to_string(),
            id.to_string(),
            String::new(),
            "reply".to_string(),
        ]);
    }
    event::rumor(from_pubkey, now, KIND_CHAT, tags, text.to_string())
}

pub fn wrap_both(
    sender_secret: &SecretKey,
    recipient_pubkey: &str,
    rumor: &Rumor,
    now: i64,
) -> Result<(Event, Event), String> {
    let theirs = nip59::wrap(sender_secret, recipient_pubkey, rumor, now)?;
    let ours = nip59::wrap(sender_secret, &event::xonly_hex(sender_secret), rumor, now)?;
    Ok((theirs, ours))
}

pub fn open_chat(
    our_secret: &SecretKey,
    our_pubkey: &str,
    gift: &Event,
) -> Result<ChatMessage, String> {
    let rumor = nip59::unwrap(our_secret, gift)?;
    if rumor.kind != KIND_CHAT {
        return Err(format!("not a chat message (kind {})", rumor.kind));
    }
    let recipient = rumor
        .tag("p")
        .ok_or("a chat message must name its recipient")?
        .to_string();
    let peer = if rumor.pubkey == our_pubkey {
        recipient
    } else {
        rumor.pubkey.clone()
    };
    if rumor.pubkey != our_pubkey && recipient_of(&rumor) != Some(our_pubkey.to_string()) {
        return Err("this message is not part of a conversation we are in".into());
    }
    let reply_to = rumor
        .tags
        .iter()
        .find(|t| t.first().map(String::as_str) == Some("e"))
        .and_then(|t| t.get(1))
        .cloned();
    Ok(ChatMessage {
        id: rumor.id.clone(),
        author: rumor.pubkey.clone(),
        peer,
        content: rumor.content.clone(),
        created_at: rumor.created_at,
        reply_to,
    })
}

fn recipient_of(rumor: &Rumor) -> Option<String> {
    rumor.tag("p").map(str::to_string)
}

pub fn dm_relay_list(secret: &SecretKey, relays: &[String], now: i64) -> Event {
    let tags = relays
        .iter()
        .map(|r| vec!["relay".to_string(), r.clone()])
        .collect();
    event::sign_with(secret, now, KIND_DM_RELAYS, tags, String::new())
}

#[allow(dead_code)]
pub fn parse_dm_relay_list(event: &Event) -> Vec<String> {
    if event.kind != KIND_DM_RELAYS {
        return Vec::new();
    }
    event
        .tags
        .iter()
        .filter(|t| t.first().map(String::as_str) == Some("relay"))
        .filter_map(|t| t.get(1).cloned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nostr::event::xonly_hex;

    fn key(seed: u8) -> SecretKey {
        SecretKey::from_slice(&[seed; 32]).expect("valid key")
    }

    #[test]
    fn both_sides_read_the_same_conversation() {
        let (alice, bob) = (key(1), key(2));
        let (a_pub, b_pub) = (xonly_hex(&alice), xonly_hex(&bob));
        let r = chat_rumor(&a_pub, &b_pub, "are you there?", None, 1_700_000_000);
        let (theirs, ours) = wrap_both(&alice, &b_pub, &r, 1_700_000_000).expect("wrap");

        let at_bob = open_chat(&bob, &b_pub, &theirs).expect("bob reads it");
        assert_eq!(at_bob.content, "are you there?");
        assert_eq!(at_bob.author, a_pub);
        assert_eq!(at_bob.peer, a_pub, "for bob the peer is alice");

        let at_alice = open_chat(&alice, &a_pub, &ours).expect("alice reads her own copy");
        assert_eq!(at_alice.content, "are you there?");
        assert_eq!(at_alice.peer, b_pub, "for alice the peer is bob");
        assert_eq!(at_alice.id, at_bob.id, "one message, one id, two copies");
    }

    #[test]
    fn our_own_copy_is_addressed_to_us() {
        let (alice, bob) = (key(1), key(2));
        let (a_pub, b_pub) = (xonly_hex(&alice), xonly_hex(&bob));
        let r = chat_rumor(&a_pub, &b_pub, "note to self", None, 1_700_000_000);
        let (theirs, ours) = wrap_both(&alice, &b_pub, &r, 1_700_000_000).expect("wrap");
        assert_eq!(ours.tag("p"), Some(a_pub.as_str()));
        assert_eq!(theirs.tag("p"), Some(b_pub.as_str()));
        assert_ne!(ours.pubkey, theirs.pubkey);
    }

    #[test]
    fn a_reply_points_at_its_parent() {
        let (alice, bob) = (key(1), key(2));
        let (a_pub, b_pub) = (xonly_hex(&alice), xonly_hex(&bob));
        let first = chat_rumor(&a_pub, &b_pub, "question", None, 1_700_000_000);
        let second = chat_rumor(&b_pub, &a_pub, "answer", Some(&first.id), 1_700_000_100);
        let (to_alice, _) = wrap_both(&bob, &a_pub, &second, 1_700_000_100).expect("wrap");
        let got = open_chat(&alice, &a_pub, &to_alice).expect("open");
        assert_eq!(got.reply_to.as_deref(), Some(first.id.as_str()));
    }

    #[test]
    fn a_message_between_other_people_is_refused() {
        let (alice, bob, carol) = (key(1), key(2), key(3));
        let (b_pub, c_pub) = (xonly_hex(&bob), xonly_hex(&carol));
        let r = chat_rumor(&b_pub, &c_pub, "about alice…", None, 1_700_000_000);
        let to_alice = nip59::wrap(&bob, &xonly_hex(&alice), &r, 1_700_000_000).expect("wrap");
        let err = open_chat(&alice, &xonly_hex(&alice), &to_alice).expect_err("refused");
        assert!(
            err.contains("not part of a conversation we are in"),
            "{err}"
        );
    }

    #[test]
    fn only_chat_kinds_become_messages() {
        let (alice, bob) = (key(1), key(2));
        let b_pub = xonly_hex(&bob);
        let not_chat = event::rumor(&xonly_hex(&alice), 1_700_000_000, 9999, vec![], "x".into());
        let gift = nip59::wrap(&alice, &b_pub, &not_chat, 1_700_000_000).expect("wrap");
        assert!(open_chat(&bob, &b_pub, &gift).is_err());
    }

    #[test]
    fn the_dm_relay_list_round_trips() {
        let relays = vec!["wss://a.example".to_string(), "wss://b.example".to_string()];
        let e = dm_relay_list(&key(1), &relays, 1_700_000_000);
        assert_eq!(e.kind, KIND_DM_RELAYS);
        assert!(e.content.is_empty(), "the list carries no content");
        assert!(e.verify());
        assert_eq!(parse_dm_relay_list(&e), relays);
    }
}
