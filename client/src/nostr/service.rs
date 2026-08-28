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

/// Start the service. Returns the handle the UI sends through.
pub fn spawn_nostr(identity: Identity, relays: Vec<String>, state: Signal<AppState>) -> NostrTx {
    let (tx, mut rx) = unbounded_channel::<NostrCmd>();
    let handle = NostrTx(tx);

    spawn(async move {
        let mut state = state;
        let our_pubkey = identity.pubkey.clone();
        let secret = identity.secret_key();
        let (pool, mut events) = RelayPool::connect(relays.clone());

        // No author filter: gift wraps are signed by ephemeral keys. `p` is
        // the only handle.
        pool.subscribe(
            SUB_DM,
            vec![
                Filter {
                    kinds: Some(vec![nip59::KIND_GIFT_WRAP]),
                    p: Some(vec![our_pubkey.clone()]),
                    limit: Some(500),
                    ..Default::default()
                },
                Filter {
                    kinds: Some(vec![nip02::KIND_CONTACTS, nip17::KIND_DM_RELAYS]),
                    authors: Some(vec![our_pubkey.clone()]),
                    limit: Some(4),
                    ..Default::default()
                },
            ],
        );

        pool.publish(nip17::dm_relay_list(&secret, &relays, now()));
        // Reading kind 0 is half a loop: between two Discordia clients neither
        // had ever published one, so both read the other as a truncated key.
        pool.publish(metadata::own_metadata_event(
            &secret,
            &identity.display_name,
            now(),
        ));

        // Offline nothing else ever sets this, and your own messages then read
        // as your own truncated key. The gateway's `Ready` overwrites it with
        // the authoritative username when there is one — hence `is_none`, which
        // is what keeps this a fallback rather than a competing source.
        {
            let mut s = state.write();
            if s.self_user.is_none() {
                s.self_user = Some(User {
                    pubkey: our_pubkey.clone(),
                    username: identity.display_name.clone(),
                });
            }
        }

        // Whoever the metadata subscription currently asks about. Held so the
        // REQ is re-issued when the set grows and not on every message.
        let mut named: Vec<String> = Vec::new();
        request_names(&pool, &state, &our_pubkey, &mut named);

        loop {
            tokio::select! {
                cmd = rx.recv() => match cmd {
                    Some(NostrCmd::Send { peer, text, reply_to }) => {
                        send_message(&pool, &secret, &our_pubkey, &peer, &text, reply_to, &mut state);
                        // Speaking first also introduces somebody: this is the
                        // path a pasted npub takes when the composer is used
                        // before the conversation exists.
                        request_names(&pool, &state, &our_pubkey, &mut named);
                    }
                    Some(NostrCmd::Open { peer }) => {
                        open_conversation(&peer, &mut state);
                        request_names(&pool, &state, &our_pubkey, &mut named);
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
                        request_names(&pool, &state, &our_pubkey, &mut named);
                    }
                    None => break,
                },
                ev = events.recv() => match ev {
                    Some(RelayEvent::Event(event)) => match event.kind {
                        nip02::KIND_CONTACTS if event.pubkey == our_pubkey => {
                            state.write().contacts = nip02::parse_contact_list(&event);
                            request_names(&pool, &state, &our_pubkey, &mut named);
                        }
                        metadata::KIND_METADATA => {
                            // Anyone's, not only the people we asked about: a
                            // relay may answer another filter with one, and a
                            // name we did not ask for costs a map entry.
                            if let Some(name) = metadata::name_from(&event) {
                                // Never a plain insert: see `note_name`. A
                                // rename that lands before a slow relay's copy
                                // of the old one would otherwise be undone.
                                state.write().note_name(&event.pubkey, name, event.created_at);
                            }
                        }
                        nip59::KIND_GIFT_WRAP => {
                            receive(&secret, &our_pubkey, &event, &mut state);
                            request_names(&pool, &state, &our_pubkey, &mut named);
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
        }
    });

    handle
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// The subscription carrying gift wraps and our own replaceable events.
const SUB_DM: &str = "dxf-dm";
/// The subscription carrying kind 0 for the people we can see.
///
/// Its own id because it is re-issued whenever a new peer appears, and a `REQ`
/// replays everything under the id it names — sharing `SUB_DM` would have
/// re-delivered every gift wrap each time somebody new said hello.
const SUB_NAMES: &str = "dxf-names";

/// Most keys the name subscription will ask about at once.
///
/// A `REQ` is one frame and relays cap how large one may be; a contact list of
/// two thousand keys would get the whole filter rejected and leave us with no
/// names at all rather than most of them. Conversation peers are taken first
/// because they are the names actually on screen.
const NAME_AUTHOR_CAP: usize = 256;

/// Whose name is worth asking a relay for, in the order it matters.
///
/// Ourselves first: offline there is no gateway roster to say who we are, so
/// our own published name is the only one there is. Then the people we are
/// talking to, then the rest of the contact list — the cap bites at the far end
/// of that, which is the end nobody is looking at.
fn names_wanted(state: &AppState, our_pubkey: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let push = |pk: &str, out: &mut Vec<String>| {
        if pk.len() == 64 && !out.iter().any(|p| p == pk) {
            out.push(pk.to_string());
        }
    };
    push(our_pubkey, &mut out);
    for dm in &state.dms {
        push(&dm.other_pubkey, &mut out);
    }
    for c in &state.contacts.contacts {
        push(&c.pubkey, &mut out);
    }
    out.truncate(NAME_AUTHOR_CAP);
    out
}

/// Re-issue the name subscription if the set of people it covers has grown.
///
/// `asked` is what the standing REQ currently names. Comparing against it is
/// what keeps this off the hot path: every inbound message calls this, and all
/// but the ones that introduce somebody new return without touching a relay.
fn request_names(
    pool: &RelayPool,
    state: &Signal<AppState>,
    our_pubkey: &str,
    asked: &mut Vec<String>,
) {
    let want = names_wanted(&state.read(), our_pubkey);
    if want == *asked || want.is_empty() {
        return;
    }
    *asked = want.clone();
    // No `limit`: kind 0 is replaceable, so a relay holds at most one per key
    // and the author list is the bound. A limit here would instead cap the
    // *total*, quietly starving whoever sorted last.
    pool.subscribe(
        SUB_NAMES,
        vec![Filter {
            kinds: Some(vec![metadata::KIND_METADATA]),
            authors: Some(want),
            ..Default::default()
        }],
    );
}

/// Make sure the conversation exists in the list and select it.
fn open_conversation(peer: &str, state: &mut Signal<AppState>) {
    let cid = conversation_id(peer);
    let mut s = state.write();
    if !s.dms.iter().any(|d| d.channel_id == cid) {
        s.dms.push(DmInfo {
            channel_id: cid,
            other_pubkey: peer.to_string(),
        });
    }
    s.dm_mode = true;
    s.selected_channel = Some(cid);
    s.mark_dm_read(cid);
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
            insert_message(&msg, our_pubkey, state, Source::Ours);
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
    insert_message(&msg, our_pubkey, state, Source::Relay);
}

/// Where a message came from. Only a relay can be replaying deleted history;
/// what we just typed here is new by construction.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Source {
    Ours,
    Relay,
}

/// Whether a delete still hides this message.
///
/// Relays replay the whole history on every launch, so a cleared conversation
/// would walk back in without this. Strictly below the mark, and never for our
/// own send: both are the same second of a delete, and dropping a message
/// someone actually sent is worse than a stale one reappearing once.
fn hidden_by_delete(s: &AppState, msg: &nip17::ChatMessage, source: Source) -> bool {
    source == Source::Relay
        && s.dm_cleared_at
            .get(&msg.peer)
            .is_some_and(|at| msg.created_at < *at)
}

/// Put a message into the conversation it belongs to, in time order.
///
/// Relays replay stored events in whatever order they like and the same message
/// arrives from several of them, so this both deduplicates and sorts rather
/// than appending.
fn insert_message(
    msg: &nip17::ChatMessage,
    our_pubkey: &str,
    state: &mut Signal<AppState>,
    source: Source,
) {
    let cid = conversation_id(&msg.peer);
    let mid = message_id(&msg.id);
    let mut s = state.write();

    if hidden_by_delete(&s, msg, source) {
        return;
    }

    if !s.dms.iter().any(|d| d.channel_id == cid) {
        s.dms.push(DmInfo {
            channel_id: cid,
            other_pubkey: msg.peer.clone(),
        });
    }
    s.nostr_event_ids.insert(mid, msg.id.clone());

    let entry = s.messages.entry(cid).or_default();
    if entry.iter().any(|m| m.id == mid) {
        return;
    }
    entry.push(Message {
        id: mid,
        channel_id: cid,
        author: User {
            pubkey: msg.author.clone(),
            // The key, deliberately — not a name looked up now. Nostr carries
            // no username, so any name here would be borrowed from the gateway
            // roster or the contact list, and both of those arrive later than
            // this and can go away again. `features::chat` resolves the name
            // for DM authors at render; storing one froze whichever answer was
            // true when the wrap was opened, which is why a friend read as a
            // key until the next message re-derived it.
            username: crate::identity::truncate_pubkey(&msg.author),
        },
        content: msg.content.clone(),
        image: None,
        reactions: Vec::new(),
        reply_to: None,
        created_at: chrono::DateTime::from_timestamp(msg.created_at, 0)
            .unwrap_or_else(chrono::Utc::now),
    });
    entry.sort_by_key(|m| m.created_at);

    if msg.author != our_pubkey {
        s.note_dm_arrival(cid, &msg.peer, msg.created_at);
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

    fn key(c: char) -> String {
        c.to_string().repeat(64)
    }

    fn with_dm(state: &mut AppState, peer: &str) {
        state.dms.push(crate::state::DmInfo {
            channel_id: conversation_id(peer),
            other_pubkey: peer.to_string(),
        });
    }

    /// Ourselves, then the people on screen, then the rest — and each key once,
    /// because a contact you are also talking to is one person.
    #[test]
    fn the_people_worth_naming_are_ordered_and_deduplicated() {
        let me = key('a');
        let talking = key('b');
        let contact = key('c');
        let mut s = AppState::empty();
        with_dm(&mut s, &talking);
        with_dm(&mut s, &contact);
        for pk in [&talking, &contact] {
            s.contacts = s.contacts.clone().with(nip02::Contact {
                pubkey: pk.clone(),
                relay: None,
                petname: None,
            });
        }

        assert_eq!(names_wanted(&s, &me), vec![me, talking, contact]);
    }

    /// A contact list is public and can be anything; a malformed key in one
    /// must not go into a filter, because a relay may reject the whole `REQ`
    /// over it and then no name resolves at all.
    #[test]
    fn a_malformed_key_never_reaches_a_filter() {
        let me = key('a');
        let mut s = AppState::empty();
        with_dm(&mut s, "not-a-pubkey");
        s.contacts = s.contacts.clone().with(nip02::Contact {
            pubkey: String::new(),
            relay: None,
            petname: None,
        });

        assert_eq!(names_wanted(&s, &me), vec![me]);
    }

    /// The cap has to bite at the end nobody is looking at: a long contact list
    /// must not push the conversation you have open out of the filter.
    #[test]
    fn the_cap_drops_contacts_not_conversations() {
        let me = key('a');
        let talking = key('b');
        let mut s = AppState::empty();
        with_dm(&mut s, &talking);
        for i in 0..NAME_AUTHOR_CAP * 2 {
            s.contacts = s.contacts.clone().with(nip02::Contact {
                pubkey: format!("{i:064x}"),
                relay: None,
                petname: None,
            });
        }

        let want = names_wanted(&s, &me);
        assert_eq!(want.len(), NAME_AUTHOR_CAP);
        assert_eq!(want[0], me);
        assert_eq!(want[1], talking);
    }

    /// Offline with nobody to talk to, our own name is still worth asking for —
    /// it is the only one there is when no gateway roster exists.
    #[test]
    fn our_own_name_is_always_asked_for() {
        let me = key('a');
        assert_eq!(names_wanted(&AppState::empty(), &me), vec![me]);
    }

    fn chat(peer: &str, created_at: i64) -> nip17::ChatMessage {
        nip17::ChatMessage {
            id: "beef".into(),
            author: key('a'),
            peer: peer.to_string(),
            content: "hi".into(),
            created_at,
            reply_to: None,
        }
    }

    /// What a delete is for: the relays still hold the events and hand them
    /// back on the next launch.
    #[test]
    fn replayed_history_stays_deleted() {
        let peer = key('b');
        let mut s = AppState::empty();
        s.clear_dm(&peer, 100);

        assert!(hidden_by_delete(&s, &chat(&peer, 99), Source::Relay));
        assert!(!hidden_by_delete(&s, &chat(&peer, 101), Source::Relay));
    }

    /// Delete, then write to the same person inside that second. Both stamps
    /// come from `now()`, so they are equal — and the message we just sent to
    /// the relays has to appear here too.
    #[test]
    fn writing_again_reopens_the_conversation() {
        let peer = key('b');
        let mut s = AppState::empty();
        s.clear_dm(&peer, 100);

        assert!(!hidden_by_delete(&s, &chat(&peer, 100), Source::Ours));
        // And once it comes back from a relay on the next launch, still ours.
        assert!(!hidden_by_delete(&s, &chat(&peer, 100), Source::Relay));
    }

    /// The mark is our clock; `created_at` is the sender's. Nothing keeps the
    /// two in step, so the mark may only ever hide, never a whole conversation.
    #[test]
    fn a_delete_never_reaches_another_peer() {
        let cleared = key('b');
        let other = key('c');
        let mut s = AppState::empty();
        s.clear_dm(&cleared, 100);

        assert!(!hidden_by_delete(&s, &chat(&other, 1), Source::Relay));
    }
}
