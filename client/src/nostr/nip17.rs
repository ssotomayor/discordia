//! NIP-17 private direct messages — what the wrapped events actually *mean*.
//!
//! `nip59` can hide any event from a relay. This decides that the thing being
//! hidden is a chat message, gives it a recipient and a reply, and says where
//! to look for one.
//!
//! **Every message is wrapped twice.** Once to the recipient and once to
//! ourselves, because a gift wrap can only be opened by the key it was
//! addressed to — including by the person who sent it. Without the second copy
//! your own sent messages would be unreadable to you the moment the app
//! restarts, and unreadable on a second device always. It is not a backup, it
//! is the only record we have.
//!
//! The two copies are independent events with independent throwaway keys, so a
//! relay cannot pair them and learn that A and B are talking.

use secp256k1::SecretKey;

use super::event::{self, Event, Rumor};
use super::nip59;

/// A chat message, per NIP-17.
pub const KIND_CHAT: u16 = 14;
/// Where a user wants to receive DMs, per NIP-17.
pub const KIND_DM_RELAYS: u16 = 10050;

/// A message once it has been unwrapped and understood.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    /// Event id of the rumor — stable, and what a reply points at.
    pub id: String,
    /// Who wrote it.
    pub author: String,
    /// The other party in the conversation, from *our* point of view.
    ///
    /// For a message we received this is the author; for one we sent it is the
    /// recipient. Computed on unwrap rather than stored, because the same rumor
    /// arrives in two copies and only the reader knows which side they are on.
    pub peer: String,
    pub content: String,
    pub created_at: i64,
    /// Id of the message this replies to.
    pub reply_to: Option<String>,
}

/// Build the unsigned chat rumor for one message.
pub fn chat_rumor(
    from_pubkey: &str,
    to_pubkey: &str,
    text: &str,
    reply_to: Option<&str>,
    now: i64,
) -> Rumor {
    let mut tags = vec![vec!["p".to_string(), to_pubkey.to_string()]];
    if let Some(id) = reply_to {
        // "reply" marker per NIP-10, so a client that threads knows this is an
        // answer rather than a mention of another message.
        tags.push(vec![
            "e".to_string(),
            id.to_string(),
            String::new(),
            "reply".to_string(),
        ]);
    }
    event::rumor(from_pubkey, now, KIND_CHAT, tags, text.to_string())
}

/// The two gift wraps a message travels as: one for them, one for us.
///
/// Returned as a pair rather than published here so the caller decides where
/// each goes — in principle the recipient's copy goes to *their* preferred
/// relays and ours to ours, which is what `KIND_DM_RELAYS` exists to say.
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

/// Open a gift wrap and, if it is a chat message, say what it says.
///
/// `our_pubkey` is what decides which side of the conversation we are on, and
/// so who the `peer` is. A message we sent comes back to us with our own key as
/// the author and the recipient in a `p` tag; a message we received is the
/// reverse.
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
    // Reject messages not involving us to prevent thread injection.
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

/// Build the kind:10050 event announcing where we read DMs.
///
/// Published so somebody who wants to reach us knows which relays to send to
/// rather than guessing. It is a *public* event and deliberately says nothing
/// else — the relay list is not private, but who you talk to on them is.
pub fn dm_relay_list(secret: &SecretKey, relays: &[String], now: i64) -> Event {
    let tags = relays
        .iter()
        .map(|r| vec!["relay".to_string(), r.clone()])
        .collect();
    event::sign_with(secret, now, KIND_DM_RELAYS, tags, String::new())
}

/// Read a kind:10050 back into a relay list.
///
/// **Unreached, and the gap it leaves is worth naming.** We publish our own
/// list (`dm_relay_list`, above) but never read anyone else's, so a DM goes to
/// the relays *we* chose rather than the ones the recipient said they read.
/// That works while both ends default to the same list and silently does not
/// when they do not. Wiring it needs a per-recipient subscription and publish
/// routing, which is more than this reader — see the audit's register.
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

    /// The round trip both ways: the recipient reads it, and so do we, from our
    /// own copy — and both agree who the other party is.
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

    /// Without the self-copy a sender cannot read their own history. This
    /// asserts the copy is really addressed to us and not merely a duplicate.
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

    /// A reply carries the parent's id so a thread can be rebuilt.
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

    /// A wrap we can open but that belongs to somebody else's conversation must
    /// not become a thread in our list.
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

    /// A non-chat event that happens to be gift wrapped is not a message.
    #[test]
    fn only_chat_kinds_become_messages() {
        let (alice, bob) = (key(1), key(2));
        let b_pub = xonly_hex(&bob);
        let not_chat = event::rumor(&xonly_hex(&alice), 1_700_000_000, 9999, vec![], "x".into());
        let gift = nip59::wrap(&alice, &b_pub, &not_chat, 1_700_000_000).expect("wrap");
        assert!(open_chat(&bob, &b_pub, &gift).is_err());
    }

    /// The relay list round-trips, and says nothing but the relays.
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
