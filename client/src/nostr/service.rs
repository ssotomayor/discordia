use dioxus::prelude::*;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use super::event::Event;
use super::relay::{Filter, RelayEvent, RelayPool};
use super::{nip02, nip17, nip59};
use crate::identity::Identity;
use crate::protocol::{Id, Message, User};
use crate::state::{AppState, DmInfo};

pub enum NostrCmd {
    Send {
        peer: String,
        text: String,
        reply_to: Option<String>,
    },
    Open {
        peer: String,
    },
    SetContact {
        peer: String,
        keep: bool,
    },
}

#[derive(Clone)]
pub struct NostrTx(UnboundedSender<NostrCmd>);

impl NostrTx {
    pub fn send(&self, cmd: NostrCmd) {
        let _ = self.0.send(cmd);
    }
}

pub fn conversation_id(peer_pubkey: &str) -> Id {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"dxf/nostr-dm/conversation/v1");
    h.update(peer_pubkey.as_bytes());
    let d = h.finalize();
    Id::from_bytes(d[..16].try_into().expect("16 bytes"))
}

fn message_id(event_id: &str) -> Id {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"dxf/nostr-dm/message/v1");
    h.update(event_id.as_bytes());
    let d = h.finalize();
    Id::from_bytes(d[..16].try_into().expect("16 bytes"))
}

pub fn spawn_nostr(identity: Identity, relays: Vec<String>, state: Signal<AppState>) -> NostrTx {
    let (tx, mut rx) = unbounded_channel::<NostrCmd>();
    let handle = NostrTx(tx);

    spawn(async move {
        let mut state = state;
        let our_pubkey = identity.pubkey.clone();
        let secret = identity.secret_key();
        let (pool, mut events) = RelayPool::connect(relays.clone());

        pool.subscribe(vec![
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
        ]);

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
                        _ => {}
                    },
                    Some(RelayEvent::Connected(url)) => {
                        eprintln!("[nostr] {url}: connected");
                        state.write().nostr_relays_up.insert(url);
                    }
                    Some(RelayEvent::Disconnected { relay, why }) => {
                        eprintln!("[nostr] {relay}: disconnected ({why}), retrying");
                        state.write().nostr_relays_up.remove(&relay);
                    }
                    Some(RelayEvent::Published { relay, id, accepted, message }) => {
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
            state.write().error_toast = Some(format!("Not sent — {e}"));
        }
    }
}

fn receive(
    secret: &secp256k1::SecretKey,
    our_pubkey: &str,
    gift: &Event,
    state: &mut Signal<AppState>,
) {
    let Ok(msg) = nip17::open_chat(secret, our_pubkey, gift) else {
        return;
    };
    insert_message(&msg, our_pubkey, state);
}

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

    #[test]
    fn derived_ids_are_stable_and_distinct() {
        let a = "a".repeat(64);
        let b = "b".repeat(64);
        assert_eq!(conversation_id(&a), conversation_id(&a));
        assert_ne!(conversation_id(&a), conversation_id(&b));
        assert_eq!(message_id("beef"), message_id("beef"));
        assert_ne!(message_id("beef"), message_id("cafe"));
        assert_ne!(conversation_id(&a).as_bytes(), message_id(&a).as_bytes());
    }
}
