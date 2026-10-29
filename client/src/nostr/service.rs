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

use super::relay::{Filter, RelayEvent, RelayPool};
use super::{metadata, nip02, nip17, nip51, nip59, profile, xp};
use crate::identity::Identity;
use crate::protocol::{Id, Message, User};
use crate::state::{AppState, DmInfo};

/// What the UI asks the service to do.
pub enum NostrCmd {
    Call(crate::features::dm_call::Action),
    Retry {
        message_id: Id,
    },
    /// Send `text` to `peer`, optionally answering a message. `emoji` are
    /// the NIP-30 tags for the custom emoji in `text`.
    Send {
        peer: String,
        text: String,
        reply_to: Option<String>,
        emoji: Vec<(String, String)>,
        /// The `Hold` this message takes the place of.
        replaces: Option<String>,
    },
    /// Show a message with the pending clock before it can be built: a DM's
    /// id hashes its emoji tags, and those wait on the pictures' upload.
    Hold {
        key: String,
        peer: String,
        text: String,
        reply_to: Option<String>,
        emoji: Vec<(String, String)>,
    },
    /// Make sure a conversation with `peer` exists in the list, and select it.
    Open {
        peer: String,
    },
    /// Add or remove a contact, and publish the whole replaced list.
    SetContact {
        peer: String,
        keep: bool,
    },
    /// Block or unblock a key, and publish the whole replaced mute list.
    SetMute {
        peer: String,
        mute: bool,
    },
    /// Republish the cross-server total. Sent when the ledger moves, and
    /// ignored when the number has not actually changed since the last one:
    /// kind 30078 is replaceable, so a re-send costs every relay a write.
    PublishXp(xp::GlobalXp),
    /// The profile just saved on this device, for the account's other ones.
    PublishProfile(crate::profile::LocalProfile),
    PublishName(String),
    /// Look an app's manifest up on our relays, for a guild about to install it.
    FetchAppManifest {
        author: String,
    },
    /// The app blocklists a guild follows, to hand to its host.
    FetchBlocklists {
        authors: Vec<String>,
    },
}

/// Handle the UI holds.
#[derive(Clone)]
pub struct NostrTx(UnboundedSender<NostrCmd>);

impl NostrTx {
    #[cfg(test)]
    pub(crate) fn test_channel() -> Self {
        Self(unbounded_channel().0)
    }
    #[cfg(test)]
    pub(crate) fn test_channel_pair() -> (Self, tokio::sync::mpsc::UnboundedReceiver<NostrCmd>) {
        let (tx, rx) = unbounded_channel();
        (Self(tx), rx)
    }
    pub fn try_send(&self, cmd: NostrCmd) -> bool {
        self.0.send(cmd).is_ok()
    }
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
pub fn spawn_nostr(
    identity: Identity,
    relays: Vec<String>,
    state: Signal<AppState>,
    settings: Signal<crate::settings::ClientSettings>,
) -> NostrTx {
    let (tx, mut rx) = unbounded_channel::<NostrCmd>();
    let handle = NostrTx(tx);

    crate::service_task::spawn(async move {
        let mut state = state;
        let our_pubkey = identity.pubkey.clone();
        let secret = identity.secret_key();
        let (call_outgoing, mut call_signals) = unbounded_channel();
        let (record_tx, mut call_records) = unbounded_channel();
        let calls = crate::features::dm_call::spawn_service(
            our_pubkey.clone(),
            state,
            call_outgoing,
            record_tx,
        );
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
                    kinds: Some(vec![
                        nip02::KIND_CONTACTS,
                        nip17::KIND_DM_RELAYS,
                        nip51::KIND_MUTE_LIST,
                    ]),
                    authors: Some(vec![our_pubkey.clone()]),
                    limit: Some(6),
                    ..Default::default()
                },
                Filter {
                    kinds: Some(vec![xp::KIND_APP_DATA]),
                    authors: Some(vec![our_pubkey.clone()]),
                    d: Some(vec![xp::D_TAG.to_string(), profile::D_TAG.to_string()]),
                    limit: Some(2),
                    ..Default::default()
                },
            ],
        );

        pool.publish(nip17::dm_relay_list(&secret, &relays, now()));
        // Without kind 0 two Discordia clients read each other as keys. It waits
        // for the synced profile: this device's name may be another's old one.
        let mut own_name = OwnName {
            pending: Some(identity.display_name.clone()),
            ..Default::default()
        };

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

        let mut xp_publisher = xp::Publisher::default();
        // The newest profile the relays hold or we sent them: what a relay's
        // copy must beat to be adopted, and whose pictures need no re-upload.
        let mut synced: Option<profile::SyncedProfile> = None;
        let mut profile_checked = false;
        let (profile_tx, mut profile_rx) = unbounded_channel::<profile::SyncedProfile>();
        let mut routes = super::delivery::Routes::default();
        let mut senders = super::delivery::Senders::new();
        let mut outbox = super::delivery::Outbox::new();
        let mut system_deliveries = std::collections::HashMap::<Id, u8>::new();
        for (peer, theirs, ours) in super::delivery::load(&our_pubkey).await {
            if let Ok(rumor) = nip59::unwrap(&secret, &ours)
                && let Ok(msg) = nip17::open_rumor(&our_pubkey, &rumor)
                && msg.author == our_pubkey
                && msg.peer == peer
                && theirs
                    .tags
                    .iter()
                    .any(|tag| tag.first().is_some_and(|t| t == "p") && tag.get(1) == Some(&peer))
            {
                let Ok(history) = super::call_history::open(&our_pubkey, &rumor) else {
                    continue;
                };
                let system = history.is_some();
                if let Some(record) = history {
                    insert_call_record(
                        record,
                        &rumor.pubkey,
                        &our_pubkey,
                        &mut state,
                        Source::Ours,
                        rumor.created_at,
                    );
                } else {
                    insert_message(&msg, &our_pubkey, &mut state, Source::Ours, false);
                }
                let id = message_id(&msg.id);
                if system {
                    system_deliveries.insert(id, 0);
                }
                state
                    .write()
                    .dm_delivery
                    .insert(id, super::delivery::Delivery::Failed);
                outbox.insert(
                    id,
                    super::delivery::Pending {
                        peer,
                        theirs,
                        ours,
                        due: std::time::Instant::now(),
                        started: !system,
                    },
                );
            }
        }
        let (delivery_tx, mut delivery_rx) = unbounded_channel();
        let mut delivery_tick = tokio::time::interval(std::time::Duration::from_millis(100));
        delivery_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        // Relays that have said they finished replaying since they connected.
        // A message arriving once every connected relay has is one that was
        // sent moments ago, which is the only kind whose delivery time says
        // anything about the sender's clock.
        let mut replayed: std::collections::HashSet<String> = std::collections::HashSet::new();

        loop {
            tokio::select! {
                cmd = rx.recv() => match cmd {
                    Some(NostrCmd::Call(action)) => {
                        if calls.send(crate::features::dm_call::Command::Action(action)).is_err() {
                            state.write().error_toast = Some("Voice call service is unavailable.".into());
                        }
                    }
                    Some(NostrCmd::Hold { key, peer, text, reply_to, emoji }) => {
                        let held = nip17::ChatMessage {
                            id: key,
                            author: our_pubkey.clone(),
                            peer,
                            content: text,
                            created_at: now(),
                            reply_to,
                            emoji,
                        };
                        insert_message(&held, &our_pubkey, &mut state, Source::Ours, false);
                        state
                            .write()
                            .dm_delivery
                            .insert(message_id(&held.id), super::delivery::Delivery::Pending);
                    }
                    Some(NostrCmd::Send { peer, text, reply_to, emoji, replaces }) => {
                        if let Some(key) = replaces {
                            withdraw_held(&mut state, &peer, &key);
                        }
                        if outbox.len() >= 128 {
                            state.write().error_toast = Some("Too many messages await delivery. Retry a failed message first.".into());
                            continue;
                        }
                        if let Some((id, pending)) = send_message(&secret, &our_pubkey, &peer, &text, reply_to, emoji, &mut state) {
                            outbox.insert(id, pending);
                            if let Err(error) = super::delivery::persist(&our_pubkey, &outbox).await {
                                state.write().error_toast = Some(format!("Could not save pending messages: {error}"));
                            }
                        }
                        // Speaking first also introduces somebody: this is the
                        // path a pasted npub takes when the composer is used
                        // before the conversation exists.
                        request_names(&pool, &state, &our_pubkey, &mut named);
                    }
                    Some(NostrCmd::Open { peer }) => {
                        open_conversation(&peer, &mut state);
                        request_names(&pool, &state, &our_pubkey, &mut named);
                    }
                    Some(NostrCmd::Retry { message_id }) => {
                        if state.peek().dm_delivery.get(&message_id) == Some(&super::delivery::Delivery::Failed)
                            && let Some(pending) = outbox.get_mut(&message_id)
                        {
                            pending.started = false;
                            pending.due = std::time::Instant::now() + std::time::Duration::from_secs(2);
                            state.write().dm_delivery.insert(message_id, super::delivery::Delivery::Pending);
                            request_names(&pool, &state, &our_pubkey, &mut Vec::new());
                        }
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
                        super::call_history::reconcile(&mut state.write(), &our_pubkey, now());
                        request_names(&pool, &state, &our_pubkey, &mut named);
                    }
                    Some(NostrCmd::SetMute { peer, mute }) => {
                        let current = state.read().mutes.clone();
                        let mut next = if mute { current.with(&peer) } else { current.without(&peer) };
                        next.created_at = now();
                        pool.publish(nip51::mute_list_event(&secret, &next, next.created_at));
                        state.write().set_mutes(next);
                    }
                    Some(NostrCmd::FetchAppManifest { author }) => {
                        pool.subscribe(
                            SUB_APP,
                            vec![Filter {
                                kinds: Some(vec![crate::protocol::APP_MANIFEST_KIND as u16]),
                                authors: Some(vec![author]),
                                d: Some(vec![crate::protocol::APP_MANIFEST_D.to_string()]),
                                limit: Some(1),
                                ..Default::default()
                            }],
                        );
                    }
                    Some(NostrCmd::FetchBlocklists { authors }) => {
                        if !authors.is_empty() {
                            pool.subscribe(
                                SUB_BLOCKLISTS,
                                vec![Filter {
                                    kinds: Some(vec![crate::protocol::APP_BLOCKLIST_KIND as u16]),
                                    authors: Some(authors),
                                    d: Some(vec![crate::protocol::APP_BLOCKLIST_D.to_string()]),
                                    ..Default::default()
                                }],
                            );
                        }
                    }
                    Some(NostrCmd::PublishName(name)) => {
                        if profile_checked {
                            own_name.publish(&pool, &secret, state, &our_pubkey, name);
                        } else {
                            own_name.pending = Some(name);
                        }
                    }
                    Some(NostrCmd::PublishProfile(local)) => {
                        share_profile(local, synced.clone(), settings, secret, profile_tx.clone());
                    }
                    Some(NostrCmd::PublishXp(total)) => {
                        if state.read().global_xp.get(&our_pubkey) != Some(&total) {
                            state.write().global_xp.insert(our_pubkey.clone(), total);
                        }
                        if let Some(total) = xp_publisher.offer(total, std::time::Instant::now()) {
                            pool.publish(xp::xp_event(&secret, &total, now()));
                        }
                    }
                    None => break,
                },
                record = call_records.recv() => {
                    if let Some(record) = record {
                        insert_call_record(record.clone(), &our_pubkey, &our_pubkey, &mut state, Source::Ours, now());
                        if outbox.len() >= 128 {
                            state.write().error_toast = Some("Too many pending messages to save call history.".into());
                            continue;
                        }
                        let result = super::call_history::rumor(&our_pubkey, &record, now()).and_then(|rumor| {
                            let peer = record.peer(&our_pubkey).ok_or("Invalid call participants")?.to_owned();
                            let (theirs, ours) = nip17::wrap_both(&secret, &peer, &rumor, now())?;
                            Ok((message_id(&rumor.id), super::delivery::Pending {
                                peer, theirs, ours, due: std::time::Instant::now(), started: false,
                            }))
                        });
                        match result {
                            Ok((id, pending)) => {
                                outbox.insert(id, pending);
                                system_deliveries.insert(id, 0);
                                if let Err(error) = super::delivery::persist(&our_pubkey, &outbox).await {
                                    outbox.remove(&id);
                                    system_deliveries.remove(&id);
                                    state.write().error_toast = Some(format!("Could not save call history: {error}"));
                                }
                            }
                            Err(error) => state.write().error_toast = Some(error),
                        }
                    }
                }
                _ = tokio::time::sleep(std::time::Duration::from_secs(
                    super::call_history::next_expiry(&state.peek()).map(|at| at.saturating_sub(now()).max(0) as u64).unwrap_or(3600)
                )), if super::call_history::next_expiry(&state.peek()).is_some() => {
                    super::call_history::expire(&mut state.write(), &our_pubkey, now());
                }
                outgoing = call_signals.recv() => {
                    if let Some((peer, signal)) = outgoing {
                        let wrap = super::calls::rumor(&our_pubkey, &peer, &signal)
                            .and_then(|rumor| nip59::wrap_with_expiration(&secret, &peer, &rumor, now(), Some(super::calls::relay_expiration(signal.sent_at))));
                        match wrap {
                            Ok(wrap) => {
                                let published = senders.publish_call(wrap, routes.targets(&peer, &relays), signal.sent_at + super::calls::TTL);
                                let calls = calls.clone();
                                spawn(async move {
                                    let accepted = published.await == super::delivery::Delivery::Accepted;
                                    if !accepted {
                                        tracing::warn!(call_id = %signal.call_id, "DM call signal was not accepted by any recipient relay");
                                    }
                                    if calls.send(crate::features::dm_call::Command::Published { call_id: signal.call_id, accepted }).is_err() {
                                        tracing::debug!("Call ended before relay publication completed");
                                    }
                                });
                            }
                            Err(error) => {
                                state.write().error_toast = Some(error);
                                if calls.send(crate::features::dm_call::Command::Published { call_id: signal.call_id, accepted: false }).is_err() {
                                    tracing::debug!("Call service stopped");
                                }
                            }
                        }
                    }
                }
                _ = delivery_tick.tick(), if outbox.values().any(|pending| !pending.started) => {
                    let active = outbox.iter().filter(|(id, pending)| pending.started && state.peek().dm_delivery.get(id) == Some(&super::delivery::Delivery::Pending)).count();
                    let mut slots = 8usize.saturating_sub(active);
                    for (id, pending) in &mut outbox {
                        if slots == 0 { break; }
                        if pending.started || pending.due > std::time::Instant::now() { continue; }
                        pending.started = true;
                        state.write().dm_delivery.insert(*id, super::delivery::Delivery::Pending);
                        slots -= 1;
                        let delivery = senders.publish(pending.theirs.clone(), routes.targets(&pending.peer, &relays));
                        let id = *id;
                        let result_tx = delivery_tx.clone();
                        pool.publish(pending.ours.clone());
                        spawn(async move {
                            let _ = result_tx.send((id, delivery.await));
                        });
                    }
                }
                shared = profile_rx.recv() => {
                    if let Some(shared) = shared
                        && synced.as_ref().is_none_or(|s| s.updated_at <= shared.updated_at)
                    {
                        match profile::profile_event(&secret, &our_pubkey, &shared) {
                            Ok(event) => pool.publish(event),
                            Err(e) => tracing::warn!(error = %e, "could not sign the profile"),
                        }
                        synced = Some(shared);
                    }
                }
                result = delivery_rx.recv() => {
                    if let Some((id, status)) = result {
                        state.write().dm_delivery.insert(id, status);
                        if status == super::delivery::Delivery::Failed
                            && let Some(attempt) = system_deliveries.get_mut(&id)
                            && *attempt < 5
                            && let Some(pending) = outbox.get_mut(&id)
                        {
                            *attempt += 1;
                            pending.started = false;
                            pending.due = std::time::Instant::now() + std::time::Duration::from_secs(15 << *attempt);
                        }
                        if status == super::delivery::Delivery::Accepted {
                            outbox.remove(&id);
                            if system_deliveries.remove(&id).is_some() {
                                state.write().dm_delivery.remove(&id);
                            }
                            if let Err(error) = super::delivery::persist(&our_pubkey, &outbox).await {
                                state.write().error_toast = Some(format!("Could not update pending messages: {error}"));
                            }
                        }
                    }
                }
                _ = tokio::time::sleep_until(
                    tokio::time::Instant::from_std(xp_publisher.due_at().unwrap_or_else(std::time::Instant::now))
                ), if xp_publisher.due_at().is_some() => {
                    if let Some(total) = xp_publisher.take_due(std::time::Instant::now()) {
                        pool.publish(xp::xp_event(&secret, &total, now()));
                    }
                }
                ev = events.recv() => match ev {
                    Some(RelayEvent::Event(event)) => match event.kind {
                        nip17::KIND_DM_RELAYS => routes.note(&event),
                        nip02::KIND_CONTACTS if event.pubkey == our_pubkey => {
                            state.write().contacts = nip02::parse_contact_list(&event);
                            super::call_history::reconcile(&mut state.write(), &our_pubkey, now());
                            request_names(&pool, &state, &our_pubkey, &mut named);
                        }
                        nip51::KIND_MUTE_LIST if event.pubkey == our_pubkey => {
                            // A relay can forge one, and the next mute would sign the
                            // forgery; a far-future one would pin itself for good.
                            if !event.verify() || event.created_at > now() + 600 {
                                continue;
                            }
                            // Relays serve stale copies of a replaceable event too.
                            if let Some(list) = nip51::parse_mute_list(&event)
                                && list.created_at > state.peek().mutes.created_at
                            {
                                state.write().set_mutes(list);
                            }
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
                        k if u32::from(k) == crate::protocol::APP_MANIFEST_KIND
                            && event.tag("d") == Some(crate::protocol::APP_MANIFEST_D) =>
                        {
                            let newer = state
                                .peek()
                                .relay_app_manifests
                                .get(&event.pubkey)
                                .is_none_or(|(at, _)| *at < event.created_at);
                            if newer && let Ok(json) = serde_json::to_string(&*event) {
                                state
                                    .write()
                                    .relay_app_manifests
                                    .insert(event.pubkey.clone(), (event.created_at, json));
                            }
                        }
                        k if u32::from(k) == crate::protocol::APP_BLOCKLIST_KIND
                            && event.tag("d") == Some(crate::protocol::APP_BLOCKLIST_D) =>
                        {
                            let newer = state
                                .peek()
                                .relay_blocklists
                                .get(&event.pubkey)
                                .is_none_or(|(at, _)| *at < event.created_at);
                            if newer && let Ok(json) = serde_json::to_string(&*event) {
                                state
                                    .write()
                                    .relay_blocklists
                                    .insert(event.pubkey.clone(), (event.created_at, json));
                            }
                        }
                        profile::KIND_APP_DATA if event.tag("d") == Some(profile::D_TAG) => {
                            // A far-future copy would beat every later edit for good.
                            if event.created_at > now() + 600 {
                                continue;
                            }
                            let Some(theirs) = profile::parse_profile(&secret, &our_pubkey, &event) else {
                                continue;
                            };
                            if synced.as_ref().is_some_and(|s| s.updated_at >= theirs.updated_at) {
                                continue;
                            }
                            synced = Some(theirs.clone());
                            adopt_profile(theirs, our_pubkey.clone(), state);
                        }
                        xp::KIND_APP_DATA => {
                            // Self-asserted: stored beside the key that signed
                            // it and drawn as a claim, never used to gate.
                            if let Some(total) = xp::parse_xp(&event) {
                                state.write().global_xp.insert(event.pubkey.clone(), total);
                            }
                        }
                        nip59::KIND_GIFT_WRAP => {
                            let Ok(rumor) = nip59::unwrap(&secret, &event) else { continue; };
                            if rumor.kind == super::calls::KIND_CALL {
                                let offset = state.peek().clock_offset(&rumor.pubkey);
                                let sender_now = super::calls::sender_now(now(), offset);
                                match super::calls::open(&rumor, &our_pubkey, sender_now) {
                                    Ok(signal) => {
                                        if matches!(signal.body, super::calls::Body::Invite) {
                                            insert_call_record(super::call_history::Record {
                                                version: 1, call_id: signal.call_id, caller: rumor.pubkey.clone(),
                                                callee: our_pubkey.clone(), started_at: signal.sent_at,
                                                outcome: super::call_history::Outcome::Started,
                                            }, &rumor.pubkey, &our_pubkey, &mut state, Source::Relay, rumor.created_at);
                                        }
                                        if calls.send(crate::features::dm_call::Command::Incoming(crate::features::dm_call::Incoming { author: rumor.pubkey, signal })).is_err() {
                                            tracing::debug!("Call service stopped");
                                        }
                                    }
                                    Err(reason) => tracing::warn!(%reason, clock_offset = offset, age_secs = sender_now.saturating_sub(rumor.created_at), "Rejected incoming DM call signal"),
                                }
                                continue;
                            }
                            match super::call_history::open(&our_pubkey, &rumor) {
                                Ok(Some(record)) => {
                                    insert_call_record(record, &rumor.pubkey, &our_pubkey, &mut state, Source::Relay, rumor.created_at);
                                    request_names(&pool, &state, &our_pubkey, &mut named);
                                    continue;
                                }
                                Err(error) => { tracing::warn!(%error, "Rejected call history record"); continue; }
                                Ok(None) => {},
                            }
                            let live = every_relay_replayed(&state, &replayed);
                            if let Ok(msg) = nip17::open_rumor(&our_pubkey, &rumor) {
                                insert_message(&msg, &our_pubkey, &mut state, Source::Relay, live);
                            }
                            request_names(&pool, &state, &our_pubkey, &mut named);
                        }
                        _ => {}
                    },
                    Some(RelayEvent::Connected(url)) => {
                        eprintln!("[nostr] {url}: connected");
                        // A reconnect re-issues the subscriptions, so this relay
                        // is replaying again until it says otherwise.
                        replayed.remove(&url);
                        state.write().nostr_relays_up.insert(url);
                        for (id, attempt) in &mut system_deliveries {
                            if state.peek().dm_delivery.get(id) == Some(&super::delivery::Delivery::Failed)
                                && let Some(pending) = outbox.get_mut(id)
                            {
                                *attempt = 0;
                                pending.started = false;
                                pending.due = std::time::Instant::now() + std::time::Duration::from_secs(2);
                            }
                        }
                    }
                    Some(RelayEvent::Disconnected { relay, why }) => {
                        // Pool retries automatically; this is the only source
                        // of the disconnect reason for status.
                        eprintln!("[nostr] {relay}: disconnected ({why}), retrying");
                        replayed.remove(&relay);
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
                    Some(RelayEvent::EndOfStored { relay, subscription }) => {
                        if subscription != SUB_DM { continue; }
                        eprintln!("[nostr] {relay}: finished replaying stored events");
                        replayed.insert(relay);
                        // Saved here while no relay answered, or before profiles synced.
                        if !profile_checked && every_relay_replayed(&state, &replayed) {
                            profile_checked = true;
                            let local = crate::profile::load(&our_pubkey);
                            let synced_at = synced.as_ref().map_or(0, |s| s.updated_at);
                            let newest_name = newest_name(local.as_ref(), synced.as_ref());
                            if let Some(local) = local
                                && local.updated_at > synced_at
                            {
                                share_profile(local, synced.clone(), settings, secret, profile_tx.clone());
                            }
                            if let Some(name) = newest_name.or(own_name.pending.take()) {
                                own_name.publish(&pool, &secret, state, &our_pubkey, name);
                            }
                        }
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

fn share_profile(
    local: crate::profile::LocalProfile,
    previous: Option<profile::SyncedProfile>,
    settings: Signal<crate::settings::ClientSettings>,
    secret: secp256k1::SecretKey,
    done: UnboundedSender<profile::SyncedProfile>,
) {
    let server = settings.peek().emoji_blob_server.clone();
    spawn(async move {
        let previous = previous.unwrap_or_default();
        let shared =
            profile::SyncedProfile::share(local, &previous, server.as_deref(), &secret, now())
                .await;
        let _ = done.send(shared);
    });
}

fn adopt_profile(theirs: profile::SyncedProfile, our_pubkey: String, mut state: Signal<AppState>) {
    spawn(async move {
        let avatar = profile::resolve(&theirs.avatar).await;
        let banner = profile::resolve(&theirs.banner).await;
        let current = crate::profile::load(&our_pubkey).unwrap_or_default();
        // Saved here while the pictures downloaded, and newer still.
        if current.updated_at >= theirs.updated_at {
            return;
        }
        let local = theirs.into_local(&current, avatar, banner);
        if let Err(e) = crate::profile::save(&our_pubkey, &local) {
            tracing::warn!(error = %e, "synced profile not saved");
            return;
        }
        let mut app = state.write();
        app.profile_synced_at = local.updated_at;
        app.account_pictures = Some((our_pubkey, local.into()));
    });
}

/// The subscription carrying gift wraps and our own replaceable events.
const SUB_DM: &str = "dxf-dm";
/// The subscription carrying kind 0 for the people we can see.
///
/// Its own id because it is re-issued whenever a new peer appears, and a `REQ`
/// replays everything under the id it names — sharing `SUB_DM` would have
/// re-delivered every gift wrap each time somebody new said hello.
const SUB_NAMES: &str = "dxf-names";

/// The name from the newest profile edit on any device; `None` when that edit
/// came from a build that did not sync names.
fn newest_name(
    local: Option<&crate::profile::LocalProfile>,
    synced: Option<&profile::SyncedProfile>,
) -> Option<String> {
    match (local, synced) {
        (Some(local), Some(synced)) if synced.updated_at > local.updated_at => synced.name.clone(),
        (Some(local), _) => local.name.clone(),
        (None, synced) => synced.and_then(|s| s.name.clone()),
    }
}

#[derive(Default)]
struct OwnName {
    pending: Option<String>,
    published: String,
    at: i64,
}

impl OwnName {
    fn publish(
        &mut self,
        pool: &RelayPool,
        secret: &secp256k1::SecretKey,
        mut state: Signal<AppState>,
        our_pubkey: &str,
        name: String,
    ) {
        let name = crate::protocol::canonical_username(&name);
        if name == self.published {
            return;
        }
        let observed = state
            .peek()
            .nostr_names
            .get(our_pubkey)
            .map_or(0, |(_, at)| *at);
        self.at = now().max(self.at.max(observed).saturating_add(1));
        let event = metadata::own_metadata_event(secret, &name, self.at);
        apply_own_name(&mut state.write(), our_pubkey, &name, self.at);
        self.published = name;
        pool.publish(event);
    }
}

fn apply_own_name(state: &mut AppState, pubkey: &str, name: &str, at: i64) {
    state.note_name(pubkey, name.to_owned(), at);
}

/// One app at a time: each lookup replaces the last under this id.
const SUB_APP: &str = "dxf-app";
const SUB_BLOCKLISTS: &str = "dxf-app-blocklists";

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
        vec![
            Filter {
                kinds: Some(vec![metadata::KIND_METADATA, nip17::KIND_DM_RELAYS]),
                authors: Some(want.clone()),
                ..Default::default()
            },
            // Same authors, same reason for no `limit`: one per key per `d`.
            Filter {
                kinds: Some(vec![xp::KIND_APP_DATA]),
                authors: Some(want),
                d: Some(vec![xp::D_TAG.to_string()]),
                ..Default::default()
            },
        ],
    );
}

/// Make sure the conversation exists in the list and select it. Only the
/// person opens one, so it is accepted, not a request they made themselves.
fn open_conversation(peer: &str, state: &mut Signal<AppState>) {
    let cid = conversation_id(peer);
    let mut s = state.write();
    s.dm_accepted.insert(peer.to_string());
    if !s.dms.iter().any(|d| d.channel_id == cid) {
        s.dms.push(DmInfo {
            channel_id: cid,
            other_pubkey: peer.to_string(),
        });
    }
    s.dm_mode = true;
    s.selected_channel = Some(cid);
    if let Some(account) = s.self_user.as_ref().map(|user| user.pubkey.clone()) {
        super::call_history::reconcile(&mut s, &account, now());
    }
    s.mark_dm_read(cid);
}

fn send_message(
    secret: &secp256k1::SecretKey,
    our_pubkey: &str,
    peer: &str,
    text: &str,
    reply_to: Option<String>,
    emoji: Vec<(String, String)>,
    state: &mut Signal<AppState>,
) -> Option<(Id, super::delivery::Pending)> {
    let ts = now();
    let rumor = nip17::chat_rumor(our_pubkey, peer, text, reply_to.as_deref(), &emoji, ts);
    match nip17::wrap_both(secret, peer, &rumor, ts) {
        Ok((theirs, ours)) => {
            let msg = nip17::ChatMessage {
                id: rumor.id.clone(),
                author: our_pubkey.to_string(),
                peer: peer.to_string(),
                content: text.to_string(),
                created_at: ts,
                reply_to,
                emoji,
            };
            insert_message(&msg, our_pubkey, state, Source::Ours, false);
            let id = message_id(&rumor.id);
            state
                .write()
                .dm_delivery
                .insert(id, super::delivery::Delivery::Pending);
            Some((
                id,
                super::delivery::Pending {
                    peer: peer.to_string(),
                    theirs,
                    ours,
                    due: std::time::Instant::now() + std::time::Duration::from_secs(2),
                    started: false,
                },
            ))
        }
        Err(e) => {
            // Refused rather than sent in the clear — there is no cleartext
            // path here to fall back to, so the only honest outcome is to say
            // it did not go.
            state.write().error_toast = Some(format!("Not sent — {e}"));
            None
        }
    }
}

/// Whether every relay we are connected to has finished replaying.
///
/// Conservative on purpose: with one relay still replaying, a stored event
/// could arrive next, and dating that by our clock would say the sender's is
/// wrong by however old the message is. No relays up at all is not "live"
/// either — it is nothing to conclude from.
fn every_relay_replayed(
    state: &Signal<AppState>,
    replayed: &std::collections::HashSet<String>,
) -> bool {
    let up = &state.read().nostr_relays_up;
    !up.is_empty() && up.iter().all(|relay| replayed.contains(relay))
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
///
/// `at` is the message's time already put on our clock, which the mark is on
/// too; comparing the sender's raw stamp hid a genuinely new message for as
/// long as the two clocks were apart.
fn hidden_by_delete(s: &AppState, peer: &str, at: i64, source: Source) -> bool {
    source == Source::Relay && s.dm_cleared_at.get(peer).is_some_and(|mark| at < *mark)
}

/// How far a sender's stamp may sit from ours before we call it a clock
/// difference rather than the delivery taking a moment. Wide enough that
/// network delay and a relay holding an event never move a conversation.
const CLOCK_DEADBAND_SECS: i64 = super::calls::CLOCK_DEADBAND;

/// Re-estimate how far ahead an author's clock runs, from a message that was
/// sent moments ago.
///
/// The latest sample rather than the largest: an author who fixes their clock
/// has to be able to bring the correction back to zero. The deadband is what
/// keeps ordinary latency from rewriting a conversation on every message.
fn note_sender_clock(s: &mut AppState, author: &str, claimed: i64, seen_at: i64) {
    let sample = claimed - seen_at;
    if (sample - s.clock_offset(author)).abs() <= CLOCK_DEADBAND_SECS {
        return;
    }
    eprintln!("[nostr] {author}: clock reads {sample}s from ours; correcting");
    s.note_clock_offset(author, sample);
}

/// Inside the deadband no offset is learned, so a clock two seconds behind
/// would still file a quick reply above the question it answers. A live
/// message therefore never goes behind the newest one held: the order it
/// arrived in is the order it happened. Replayed history keeps its stamps.
fn filed_at(created_at: i64, newest_held: Option<i64>, live: bool) -> i64 {
    match newest_held {
        Some(newest)
            if live && created_at < newest && newest - created_at <= CLOCK_DEADBAND_SECS =>
        {
            newest
        }
        _ => created_at,
    }
}

/// Put a message into the conversation it belongs to, in time order.
///
/// Relays replay stored events in whatever order they like and the same message
/// arrives from several of them, so this both deduplicates and sorts rather
/// than appending.
fn withdraw_held(state: &mut Signal<AppState>, peer: &str, key: &str) {
    let (cid, mid) = (conversation_id(peer), message_id(key));
    let mut s = state.write();
    if let Some(held) = s.messages.get_mut(&cid) {
        held.retain(|m| m.id != mid);
    }
    s.dm_delivery.remove(&mid);
    s.nostr_event_ids.remove(&mid);
    s.dm_emoji.remove(&mid);
}

fn insert_call_record(
    record: super::call_history::Record,
    author: &str,
    account: &str,
    state: &mut Signal<AppState>,
    source: Source,
    published_at: i64,
) {
    let Some(peer) = record.peer(account) else {
        return;
    };
    let snapshot = state.peek();
    let created_at = record
        .started_at
        .saturating_sub(snapshot.clock_offset(&record.caller));
    if hidden_by_delete(&snapshot, peer, created_at, source) {
        return;
    }
    let at = if record.outcome == super::call_history::Outcome::Missed {
        published_at.saturating_sub(snapshot.clock_offset(author))
    } else {
        now()
    };
    drop(snapshot);
    super::call_history::merge(&mut state.write(), account, record, author, at);
}

fn insert_message(
    msg: &nip17::ChatMessage,
    our_pubkey: &str,
    state: &mut Signal<AppState>,
    source: Source,
    live: bool,
) {
    if state.peek().mutes.contains(&msg.peer) {
        return;
    }
    let cid = conversation_id(&msg.peer);
    let mid = message_id(&msg.id);
    let mut s = state.write();
    if msg.author == our_pubkey {
        s.dm_accepted.insert(msg.peer.clone());
    }

    // A DM carries the time its sender wrote on it, and nothing keeps two
    // clocks in step: without this a peer running a minute behind has every
    // reply filed a minute above the question it answers.
    if live && source == Source::Relay && msg.author != our_pubkey {
        note_sender_clock(&mut s, &msg.author, msg.created_at, now());
    }
    let created_at = msg.created_at - s.clock_offset(&msg.author);
    let newest_held = s
        .messages
        .get(&cid)
        .and_then(|held| held.last())
        .map(|m| m.created_at.timestamp());
    let created_at = filed_at(created_at, newest_held, live || source == Source::Ours);

    if hidden_by_delete(&s, &msg.peer, created_at, source) {
        return;
    }

    if !s.dms.iter().any(|d| d.channel_id == cid) {
        s.dms.push(DmInfo {
            channel_id: cid,
            other_pubkey: msg.peer.clone(),
        });
    }
    s.nostr_event_ids.insert(mid, msg.id.clone());
    if !msg.emoji.is_empty() {
        s.dm_emoji.insert(mid, msg.emoji.clone());
    }

    let fresh = s.insert_message(
        cid,
        Message {
            author_bot: false,
            edited_at: None,
            image_metadata: None,
            attachment: None,
            pinned: false,
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
            created_at: chrono::DateTime::from_timestamp(created_at, 0)
                .unwrap_or_else(chrono::Utc::now),
        },
    );
    if !fresh {
        return;
    }

    #[cfg(any(target_os = "android", target_os = "windows"))]
    crate::dm_notifications::received(&s, msg, our_pubkey, live && source == Source::Relay);

    if msg.author != our_pubkey {
        s.note_dm_arrival(cid, &msg.peer, created_at);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_newest_edit_names_the_account_and_an_unnamed_one_defers() {
        let local = |at, name: Option<&str>| crate::profile::LocalProfile {
            name: name.map(str::to_owned),
            updated_at: at,
            ..Default::default()
        };
        let synced = |at, name: Option<&str>| profile::SyncedProfile {
            name: name.map(str::to_owned),
            updated_at: at,
            ..Default::default()
        };
        let other_device = synced(20, Some("Windows"));
        assert_eq!(
            newest_name(Some(&local(10, Some("Mac"))), Some(&other_device)).as_deref(),
            Some("Windows")
        );
        assert_eq!(
            newest_name(Some(&local(30, Some("Mac"))), Some(&other_device)).as_deref(),
            Some("Mac")
        );
        assert_eq!(
            newest_name(None, Some(&other_device)).as_deref(),
            Some("Windows")
        );
        assert_eq!(
            newest_name(Some(&local(10, None)), Some(&synced(20, None))),
            None
        );
        assert_eq!(newest_name(None, None), None);
    }

    #[test]
    fn renaming_updates_the_live_account_and_every_human_membership() {
        let mut state = AppState::empty();
        state.self_user = Some(User {
            pubkey: "me".into(),
            username: "Before".into(),
        });
        for (pubkey, bot) in [("me", false), ("me", false), ("me", true), ("other", false)] {
            state.members.push(crate::protocol::Member {
                user: User {
                    pubkey: pubkey.into(),
                    username: "Before".into(),
                },
                guild_id: Id::new_v4(),
                online: true,
                bot,
                roles: vec![],
                xp: 0,
                look: Default::default(),
            });
        }
        apply_own_name(&mut state, "me", "After", 101);
        assert_eq!(state.display_name("me"), "After");
        assert_eq!(state.nostr_names["me"], ("After".into(), 101));
        assert_eq!(
            state
                .members
                .iter()
                .map(|member| member.user.username.as_str())
                .collect::<Vec<_>>(),
            ["After", "After", "Before", "Before"]
        );
        assert!(!state.note_name("me", "Before".into(), 100));
        assert_eq!(state.display_name("me"), "After");
    }

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

    #[test]
    fn a_held_message_waits_with_the_clock_and_leaves_no_trace() {
        use dioxus::prelude::*;
        let mut dom = VirtualDom::new(|| rsx! {});
        dom.rebuild_in_place();
        dom.in_scope(ScopeId::ROOT, || {
            let (me, peer) = (key('a'), key('b'));
            let mut state = Signal::new(AppState::empty());
            let held = nip17::ChatMessage {
                id: "held-1".into(),
                author: me.clone(),
                peer: peer.clone(),
                content: "hi :wave:".into(),
                created_at: now(),
                reply_to: None,
                emoji: vec![("wave".into(), "data:image/gif;base64,AA==".into())],
            };
            insert_message(&held, &me, &mut state, Source::Ours, false);
            let mid = message_id("held-1");
            state
                .write()
                .dm_delivery
                .insert(mid, super::super::delivery::Delivery::Pending);
            assert_eq!(state.read().messages[&conversation_id(&peer)].len(), 1);
            assert!(state.read().dm_emoji.contains_key(&mid));

            withdraw_held(&mut state, &peer, "held-1");
            let s = state.read();
            assert!(s.messages[&conversation_id(&peer)].is_empty());
            assert!(!s.dm_delivery.contains_key(&mid));
            assert!(!s.dm_emoji.contains_key(&mid));
            assert!(!s.nostr_event_ids.contains_key(&mid));
        });
    }

    fn chat(id: &str, author: &str, peer: &str, at: i64) -> nip17::ChatMessage {
        nip17::ChatMessage {
            id: id.into(),
            author: author.into(),
            peer: peer.into(),
            content: "hello".into(),
            created_at: at,
            reply_to: None,
            emoji: Vec::new(),
        }
    }

    fn in_scope(f: impl FnOnce()) {
        use dioxus::prelude::*;
        let mut dom = VirtualDom::new(|| rsx! {});
        dom.rebuild_in_place();
        dom.in_scope(ScopeId::ROOT, f);
    }

    /// Our self-copies replay too, so a conversation we started stays accepted
    /// on a fresh device.
    #[test]
    fn writing_first_accepts_the_conversation() {
        in_scope(|| {
            let (me, peer) = (key('a'), key('b'));
            let mut state = Signal::new(AppState::empty());
            insert_message(
                &chat("q", &me, &peer, 100),
                &me,
                &mut state,
                Source::Relay,
                false,
            );
            insert_message(
                &chat("r", &peer, &peer, 200),
                &me,
                &mut state,
                Source::Relay,
                false,
            );
            let s = state.read();
            assert_eq!(s.dm_standing(&peer), crate::state::DmStanding::Inbox);
            assert_eq!(s.dm_unread.get(&conversation_id(&peer)), Some(&1));
        });
    }

    #[test]
    fn a_stranger_lands_in_requests() {
        in_scope(|| {
            let (me, peer) = (key('a'), key('b'));
            let mut state = Signal::new(AppState::empty());
            insert_message(
                &chat("r", &peer, &peer, 200),
                &me,
                &mut state,
                Source::Relay,
                false,
            );
            let s = state.read();
            assert_eq!(s.dm_request_count(), 1);
            assert!(s.dm_unread.is_empty());
            assert_eq!(s.dm_notify_tick, 0);
        });
    }

    #[test]
    fn a_blocked_key_never_opens_a_conversation() {
        in_scope(|| {
            let (me, peer) = (key('a'), key('b'));
            let mut state = Signal::new(AppState::empty());
            state.write().mutes = nip51::MuteList::default().with(&peer);
            insert_message(
                &chat("r", &peer, &peer, 200),
                &me,
                &mut state,
                Source::Relay,
                false,
            );
            let s = state.read();
            assert!(s.dms.is_empty());
            assert!(s.messages.is_empty());
        });
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

    /// What a delete is for: the relays still hold the events and hand them
    /// back on the next launch.
    #[test]
    fn replayed_history_stays_deleted() {
        let peer = key('b');
        let mut s = AppState::empty();
        s.clear_dm(&peer, 100);

        assert!(hidden_by_delete(&s, &peer, 99, Source::Relay));
        assert!(!hidden_by_delete(&s, &peer, 101, Source::Relay));
    }

    /// Delete, then write to the same person inside that second. Both stamps
    /// come from `now()`, so they are equal — and the message we just sent to
    /// the relays has to appear here too.
    #[test]
    fn writing_again_reopens_the_conversation() {
        let peer = key('b');
        let mut s = AppState::empty();
        s.clear_dm(&peer, 100);

        assert!(!hidden_by_delete(&s, &peer, 100, Source::Ours));
        // And once it comes back from a relay on the next launch, still ours.
        assert!(!hidden_by_delete(&s, &peer, 100, Source::Relay));
    }

    /// A delete is one conversation's, however old the message is.
    #[test]
    fn a_delete_never_reaches_another_peer() {
        let cleared = key('b');
        let other = key('c');
        let mut s = AppState::empty();
        s.clear_dm(&cleared, 100);

        assert!(!hidden_by_delete(&s, &other, 1, Source::Relay));
    }

    /// The case the deadband exists for: a message that took a moment to
    /// arrive is not a sender whose clock is wrong.
    #[test]
    fn ordinary_delivery_delay_is_not_a_clock_difference() {
        let author = key('a');
        let mut s = AppState::empty();
        note_sender_clock(&mut s, &author, 1_000, 1_002);
        assert_eq!(s.clock_offset(&author), 0);
    }

    /// And the case it must not swallow: a peer a quarter of an hour behind.
    #[test]
    fn a_skewed_sender_is_corrected_onto_our_clock() {
        let author = key('a');
        let mut s = AppState::empty();
        note_sender_clock(&mut s, &author, 1_000 - 900, 1_000);
        assert_eq!(s.clock_offset(&author), -900);

        // And once they fix it, the correction has to come back off.
        note_sender_clock(&mut s, &author, 2_000, 2_000);
        assert_eq!(s.clock_offset(&author), 0);
    }

    /// The case under the deadband (#186): two machines two seconds apart,
    /// a reply written after the question. It has to file after it.
    #[test]
    fn a_reply_from_inside_the_deadband_files_after_the_message_it_answers() {
        let ours_at = 1_000;
        assert_eq!(filed_at(ours_at - 2, Some(ours_at), true), ours_at);
        assert_eq!(filed_at(ours_at + 3, Some(ours_at), true), ours_at + 3);
        assert_eq!(filed_at(ours_at, None, true), ours_at);
    }

    /// Replayed history is old and arrives in any order: it keeps its stamps.
    /// So does a live straggler from beyond the deadband, which is a slow
    /// relay, not a clock — and past the deadband `note_sender_clock` has it.
    #[test]
    fn replayed_history_and_old_stragglers_keep_their_stamps() {
        assert_eq!(filed_at(998, Some(1_000), false), 998);
        let long_ago = 1_000 - CLOCK_DEADBAND_SECS - 1;
        assert_eq!(filed_at(long_ago, Some(1_000), true), long_ago);
    }
    #[test]
    fn call_history_versions_use_independent_delivery_ids() {
        use super::super::call_history::{self, Outcome, Record};
        let key = |byte| secp256k1::SecretKey::from_slice(&[byte; 32]).unwrap();
        let record = Record {
            version: 1,
            call_id: uuid::Uuid::new_v4(),
            caller: super::super::event::xonly_hex(&key(1)),
            callee: super::super::event::xonly_hex(&key(2)),
            started_at: 100,
            outcome: Outcome::Started,
        };
        let started = call_history::rumor(&record.caller, &record, 100).unwrap();
        let missed = call_history::rumor(
            &record.caller,
            &Record {
                outcome: Outcome::Missed,
                ..record.clone()
            },
            145,
        )
        .unwrap();
        assert_ne!(message_id(&started.id), message_id(&missed.id));
        let mut pending = std::collections::HashMap::from([
            (message_id(&started.id), started),
            (message_id(&missed.id), missed),
        ]);
        let missed_id = pending
            .iter()
            .find(|(_, rumor)| rumor.created_at == 145)
            .map(|(id, _)| *id)
            .unwrap();
        pending.retain(|id, _| *id == missed_id);
        assert!(pending.contains_key(&missed_id));
    }
}
