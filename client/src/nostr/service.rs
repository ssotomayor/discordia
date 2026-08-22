//! The Nostr DM service: one task, owning the relay pool, feeding `AppState`.
//!
//! Shaped like `net::spawn_gateway` deliberately — a task that owns a
//! connection and mutates the same `Signal<AppState>` the UI renders, with a
//! command channel for the other direction. A reader who understands one
//! understands the other.
//!
//! **Direct messages no longer touch the gateway.** Nothing here goes near the
//! server: the conversation lives on relays, so it survives changing servers,
//! self-hosting, or the host deleting its database. That is the whole reason
//! this exists.
//!
//! ## Fitting Nostr into a UI built for channels
//!
//! The DM surface was written against `dms: Vec<DmInfo>` and
//! `messages: HashMap<Id, Vec<Message>>`, both keyed by `Uuid`. Nostr has no
//! Uuids — a conversation is identified by the other person's pubkey and a
//! message by a 32-byte event id. Rather than rewrite every DM view, both are
//! *derived*: `conversation_id` hashes the peer's pubkey into a stable Uuid,
//! and `message_id` does the same for an event id. Deterministic, so the same
//! conversation is the same Uuid on every launch and every device, and the
//! mapping back to the real event id is kept in `AppState::nostr_event_ids`
//! for replies.

use dioxus::prelude::*;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use super::event::Event;
use super::relay::{Filter, RelayEvent, RelayPool};
use super::{metadata, nip02, nip17, nip59};
use crate::identity::Identity;
use crate::protocol::{Id, Message, User};
use crate::state::{AppState, DmInfo};

/// What the UI asks the service to do.
pub enum NostrCmd {
    /// Send `text` to `peer`, optionally answering a message.
    Send {
        peer: String,
        text: String,
        reply_to: Option<String>,
    },
    /// Make sure a conversation with `peer` exists in the list, and select it.
    Open { peer: String },
    /// Add or remove a contact, and publish the whole replaced list.
    SetContact { peer: String, keep: bool },
    /// Rename a contact you already have. `None` clears the name.
    ///
    /// Separate from `SetContact` because it means something different: that
    /// one decides whether a person is on a list anyone can read, this one only
    /// changes what *you* call someone already on it. Both republish the whole
    /// list, because kind 3 leaves no other option.
    SetPetname {
        peer: String,
        petname: Option<String>,
    },
}

/// Handle the UI holds.
#[derive(Clone)]
pub struct NostrTx(UnboundedSender<NostrCmd>);

impl NostrTx {
    pub fn send(&self, cmd: NostrCmd) {
        let _ = self.0.send(cmd);
    }
}

/// A stable `Uuid` for the conversation with `peer`.
///
/// Derived rather than random so it is the same on every device and after
/// every restart — the UI uses it as a map key, and a fresh id each launch
/// would split one conversation into many.
pub fn conversation_id(peer_pubkey: &str) -> Id {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"dxf/nostr-dm/conversation/v1");
    h.update(peer_pubkey.as_bytes());
    let d = h.finalize();
    Id::from_bytes(d[..16].try_into().expect("16 bytes"))
}

/// A stable `Uuid` for a Nostr event id, so a message has a key the UI can use.
fn message_id(event_id: &str) -> Id {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"dxf/nostr-dm/message/v1");
    h.update(event_id.as_bytes());
    let d = h.finalize();
    Id::from_bytes(d[..16].try_into().expect("16 bytes"))
}

/// Everything this client wants from the relays right now.
///
/// Rebuilt whole on every change because `RelayPool::subscribe` replaces the
/// standing subscription; handing it only the new filter would drop the others.
///
/// Three filters rather than one: a `REQ` ORs them, and the three things wanted
/// here share no shape. Gift wraps carry no author we can name — they are
/// signed by throwaway keys, which is the point — so `p` is the only handle.
/// Our own replaceable events are the opposite: author is all that identifies
/// them. And kind:0 is asked for by author, of other people.
fn subscription(our_pubkey: &str, people: &std::collections::BTreeSet<String>) -> Vec<Filter> {
    let mut filters = vec![
        Filter {
            kinds: Some(vec![nip59::KIND_GIFT_WRAP]),
            p: Some(vec![our_pubkey.to_string()]),
            limit: Some(500),
            ..Default::default()
        },
        Filter {
            kinds: Some(vec![nip02::KIND_CONTACTS, nip17::KIND_DM_RELAYS]),
            authors: Some(vec![our_pubkey.to_string()]),
            limit: Some(4),
            ..Default::default()
        },
    ];
    if !people.is_empty() {
        filters.push(Filter {
            kinds: Some(vec![metadata::KIND_METADATA]),
            authors: Some(people.iter().cloned().collect()),
            // Kind 0 is replaceable, so relays hold one per author and the
            // limit only bounds a misbehaving one.
            limit: Some(people.len() as u32),
            ..Default::default()
        });
    }
    filters
}

/// Everyone whose name we would like to know: your contacts, and whoever you
/// have a conversation with.
fn people_worth_naming(state: &AppState) -> std::collections::BTreeSet<String> {
    state
        .contacts
        .contacts
        .iter()
        .map(|c| c.pubkey.clone())
        .chain(state.dms.iter().map(|d| d.other.pubkey.clone()))
        .collect()
}

/// Start the service. Returns the handle the UI sends through.
pub fn spawn_nostr(identity: Identity, relays: Vec<String>, state: Signal<AppState>) -> NostrTx {
    let (tx, mut rx) = unbounded_channel::<NostrCmd>();
    let handle = NostrTx(tx);

    spawn(async move {
        let mut state = state;
        let our_pubkey = identity.pubkey.clone();
        let secret = identity.secret_key();
        let (pool, mut events) = RelayPool::connect(relays.clone());

        // Whose kind:0 we are asking for. Grows as the contact list arrives and
        // as conversations appear, and every growth means a fresh `subscribe` —
        // that call *replaces* the standing subscription rather than adding to
        // it, so the whole filter set is rebuilt each time.
        let mut known: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        pool.subscribe(subscription(&our_pubkey, &known));

        pool.publish(nip17::dm_relay_list(&secret, &relays, now()));

        loop {
            tokio::select! {
                cmd = rx.recv() => match cmd {
                    Some(NostrCmd::Send { peer, text, reply_to }) => {
                        send_message(&pool, &secret, &our_pubkey, &peer, &text, reply_to, &mut state);
                    }
                    Some(NostrCmd::Open { peer }) => {
                        open_conversation(&peer, &mut state);
                    }
                    Some(NostrCmd::SetContact { peer, keep }) => {
                        // Read-modify-write, never append: kind 3 is
                        // replaceable, so publishing a partial list deletes
                        // everyone missing from it.
                        let current = state.read().contacts.clone();
                        let next = if keep {
                            current.with(nip02::Contact {
                                pubkey: peer.clone(),
                                relay: None,
                                petname: None,
                            })
                        } else {
                            current.without(&peer)
                        };
                        pool.publish(nip02::contact_list_event(&secret, &next, now()));
                        state.write().contacts = next;
                    }
                    Some(NostrCmd::SetPetname { peer, petname }) => {
                        // Read-modify-write for the same reason as above. The
                        // rules live in `renamed` so they can be tested without
                        // a relay; the guard keeps a no-op rename from
                        // republishing an identical list.
                        let current = state.read().contacts.clone();
                        let next = current.renamed(&peer, petname);
                        if next != current {
                            pool.publish(nip02::contact_list_event(&secret, &next, now()));
                            state.write().contacts = next;
                        }
                    }
                    None => break,
                },
                ev = events.recv() => match ev {
                    Some(RelayEvent::Event(event)) => match event.kind {
                        nip02::KIND_CONTACTS if event.pubkey == our_pubkey => {
                            state.write().contacts = nip02::parse_contact_list(&event);
                        }
                        nip59::KIND_GIFT_WRAP => {
                            receive(&secret, &our_pubkey, &event, &mut state);
                        }
                        metadata::KIND_METADATA => {
                            // Self-declared and unverified beyond the signature
                            // — it proves a key said this, nothing more. Kind 0
                            // is replaceable, so what arrives is the whole
                            // current claim and replaces ours wholesale.
                            if let Some(m) = metadata::parse(&event) {
                                state.write().nostr_profiles.insert(event.pubkey.clone(), m);
                            }
                        }
                        _ => {}
                    },
                    Some(RelayEvent::Connected(url)) => {
                        eprintln!("[nostr] {url}: connected");
                        state.write().nostr_relays_up.insert(url);
                    }
                    Some(RelayEvent::Disconnected { relay, why }) => {
                        // Pool retries automatically; this is the only source
                        // of the disconnect reason for status.
                        eprintln!("[nostr] {relay}: disconnected ({why}), retrying");
                        state.write().nostr_relays_up.remove(&relay);
                    }
                    Some(RelayEvent::Published { relay, id, accepted, message }) => {
                        // Publish succeeds if any relay accepts; a single
                        // rejection is not a failure but is the only signal of
                        // partial delivery.
                        if !accepted {
                            eprintln!(
                                "[nostr] {relay}: rejected event {id}{}",
                                if message.is_empty() {
                                    String::new()
                                } else {
                                    format!(" ({message})")
                                }
                            );
                        }
                    }
                    Some(RelayEvent::EndOfStored { relay }) => {
                        eprintln!("[nostr] {relay}: finished replaying stored events");
                    }
                    None => break,
                },
            }

            // After anything that could have introduced someone: a contact list
            // arriving, a gift wrap opening a conversation, a contact added by
            // hand. Cheap to compare and the only place that has to know is
            // here, rather than every path that can add a person.
            let current = people_worth_naming(&state.read());
            if current != known {
                known = current;
                pool.subscribe(subscription(&our_pubkey, &known));
            }
        }
    });

    handle
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Make sure the conversation exists in the list and select it.
fn open_conversation(peer: &str, state: &mut Signal<AppState>) {
    let cid = conversation_id(peer);
    let mut s = state.write();
    if !s.dms.iter().any(|d| d.channel_id == cid) {
        let username = s.display_name(peer);
        s.dms.push(DmInfo {
            channel_id: cid,
            other: User {
                pubkey: peer.to_string(),
                username,
            },
        });
    }
    s.dm_mode = true;
    s.selected_channel = Some(cid);
    s.dm_unread.remove(&cid);
}

/// Build, wrap and publish a message; show it immediately.
///
/// The local copy is added before any relay answers, because waiting would mean
/// a visible delay on every message for a confirmation that says nothing useful
/// — publishing succeeds if *any* relay accepts, and the copy addressed to us
/// will arrive back through the subscription anyway. `insert_message`
/// deduplicates by id, so the echo is a no-op rather than a double.
#[allow(clippy::too_many_arguments)]
fn send_message(
    pool: &RelayPool,
    secret: &secp256k1::SecretKey,
    our_pubkey: &str,
    peer: &str,
    text: &str,
    reply_to: Option<String>,
    state: &mut Signal<AppState>,
) {
    let ts = now();
    let rumor = nip17::chat_rumor(our_pubkey, peer, text, reply_to.as_deref(), ts);
    match nip17::wrap_both(secret, peer, &rumor, ts) {
        Ok((theirs, ours)) => {
            pool.publish(theirs);
            pool.publish(ours);
            let msg = nip17::ChatMessage {
                id: rumor.id.clone(),
                author: our_pubkey.to_string(),
                peer: peer.to_string(),
                content: text.to_string(),
                created_at: ts,
                reply_to,
            };
            insert_message(&msg, our_pubkey, state);
        }
        Err(e) => {
            // Refused rather than sent in the clear — there is no cleartext
            // path here to fall back to, so the only honest outcome is to say
            // it did not go.
            state.write().error_toast = Some(format!("Not sent — {e}"));
        }
    }
}

/// Open an inbound gift wrap and file it, if it is a chat message for us.
fn receive(
    secret: &secp256k1::SecretKey,
    our_pubkey: &str,
    gift: &Event,
    state: &mut Signal<AppState>,
) {
    // Failure is the normal case, not an error: the subscription asks for every
    // gift wrap addressed to us, and other Nostr apps wrap other things. A wrap
    // we cannot read, or that turns out not to be a chat message, is simply not
    // ours to render.
    let Ok(msg) = nip17::open_chat(secret, our_pubkey, gift) else {
        return;
    };
    insert_message(&msg, our_pubkey, state);
}

/// Put a message into the conversation it belongs to, in time order.
///
/// Relays replay stored events in whatever order they like and the same message
/// arrives from several of them, so this both deduplicates and sorts rather
/// than appending.
fn insert_message(msg: &nip17::ChatMessage, our_pubkey: &str, state: &mut Signal<AppState>) {
    let cid = conversation_id(&msg.peer);
    let mid = message_id(&msg.id);
    let mut s = state.write();

    if !s.dms.iter().any(|d| d.channel_id == cid) {
        let username = s.display_name(&msg.peer);
        s.dms.push(DmInfo {
            channel_id: cid,
            other: User {
                pubkey: msg.peer.clone(),
                username,
            },
        });
    }
    s.nostr_event_ids.insert(mid, msg.id.clone());

    let author_name = s.display_name(&msg.author);
    let entry = s.messages.entry(cid).or_default();
    if entry.iter().any(|m| m.id == mid) {
        return;
    }
    entry.push(Message {
        id: mid,
        channel_id: cid,
        author: User {
            pubkey: msg.author.clone(),
            username: author_name,
        },
        content: msg.content.clone(),
        image: None,
        reactions: Vec::new(),
        reply_to: None,
        created_at: chrono::DateTime::from_timestamp(msg.created_at, 0)
            .unwrap_or_else(chrono::Utc::now),
    });
    entry.sort_by_key(|m| m.created_at);

    let viewing = s.selected_channel == Some(cid) && s.dm_mode;
    if msg.author != our_pubkey && !viewing {
        *s.dm_unread.entry(cid).or_insert(0) += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole UI keys off these, so they must be stable across launches and
    /// distinct between peers. A random id would split one conversation into a
    /// new thread every time the app started.
    #[test]
    fn derived_ids_are_stable_and_distinct() {
        let a = "a".repeat(64);
        let b = "b".repeat(64);
        assert_eq!(conversation_id(&a), conversation_id(&a));
        assert_ne!(conversation_id(&a), conversation_id(&b));
        assert_eq!(message_id("beef"), message_id("beef"));
        assert_ne!(message_id("beef"), message_id("cafe"));
        // And the two derivations must not collide with each other, or a
        // message could be mistaken for a conversation.
        assert_ne!(conversation_id(&a).as_bytes(), message_id(&a).as_bytes());
    }
}
