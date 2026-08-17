//! WebSocket handlers: host control, friend join, host proxy pairing.
//!
//! Liveness: a host holds its listing only while its `/control` socket answers
//! heartbeats — see the loop in `handle_host_control`.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use dioxusfun_protocol::rendezvous::{HostToRendezvous, RendezvousToHost};
use futures_util::{SinkExt, StreamExt};
use uuid::Uuid;

use crate::Config;
use crate::registry::{ClaimError, HostEntry, Registry, ReleaseError, validate_name};
use crate::shortcode;
use crate::verify;

const MAX_CLAIM_ATTEMPTS: usize = 10;

pub async fn handle_host_control(socket: WebSocket, registry: Arc<Registry>, cfg: Arc<Config>) {
    let (mut tx, mut rx) = socket.split();

    // Issue an ownership challenge up front. A host claiming a persistent name
    // must sign this nonce; anonymous hosts (no name) can ignore it.
    let nonce = verify::fresh_nonce();
    if send_msg(
        &mut tx,
        &RendezvousToHost::Challenge {
            nonce: nonce.clone(),
        },
    )
    .await
    .is_err()
    {
        return;
    }

    // First frame must be a Register.
    let register = match rx.next().await {
        Some(Ok(Message::Text(t))) => match serde_json::from_str::<HostToRendezvous>(&t) {
            Ok(HostToRendezvous::Register {
                name,
                pubkey,
                signature,
                publish_public,
                description,
                endpoint,
                transport_key,
                transport_signature,
                transport_addrs,
            }) => (
                name,
                pubkey,
                signature,
                publish_public,
                description,
                endpoint,
                transport_key,
                transport_signature,
                transport_addrs,
            ),
            // An administrative frame rather than a session: verify, answer,
            // and close. It shares the challenge nonce with the claim because
            // it is the same proof — control of the key the name is bound to.
            Ok(HostToRendezvous::ReleaseName {
                name,
                pubkey,
                signature,
            }) => {
                let slug = match validate_name(&name) {
                    Ok(s) => s,
                    Err(e) => {
                        send_err(&mut tx, &e).await;
                        return;
                    }
                };
                if let Err(e) = verify::verify_ownership(&pubkey, &signature, &nonce, &name) {
                    send_err(&mut tx, &format!("name ownership rejected: {e}")).await;
                    return;
                }
                match registry.release_name(&slug, &pubkey) {
                    Ok(()) => {
                        tracing::info!(name = %slug, "reservation released");
                        let _ = send_msg(&mut tx, &RendezvousToHost::Released { name: slug }).await;
                    }
                    Err(ReleaseError::NotYours) => {
                        send_err(&mut tx, &format!("'{slug}' is not reserved by that key")).await;
                    }
                    Err(ReleaseError::LiveNow) => {
                        send_err(
                            &mut tx,
                            &format!("'{slug}' has a live session — stop the host first"),
                        )
                        .await;
                    }
                }
                return;
            }
            Err(e) => {
                send_err(&mut tx, &format!("invalid register frame: {e}")).await;
                return;
            }
        },
        Some(Ok(_)) => {
            send_err(&mut tx, "expected text Register frame").await;
            return;
        }
        Some(Err(e)) => {
            tracing::warn!(err = %e, "host control recv failed");
            return;
        }
        None => return,
    };
    let (
        name,
        pubkey,
        signature,
        publish_public,
        description,
        endpoint,
        transport_key,
        transport_signature,
        transport_addrs,
    ) = register;

    // A transport key is published only if the registering key vouches for it.
    // Unattested, it would be an invitation to point friends at somebody else's
    // host — the transport authenticates whoever holds that key, so publishing
    // one nobody proved ownership of moves the trust problem rather than
    // solving it. The pair is dropped rather than the registration refused: a
    // host with an unusable transport key is still perfectly good over the
    // relay, and this way an older or misconfigured client degrades instead of
    // being locked out.
    let transport = match (&transport_key, &pubkey, &transport_signature) {
        (Some(key), Some(pk), Some(sig)) => match verify::verify_ownership(pk, sig, &nonce, key) {
            Ok(()) => Some((key.clone(), transport_addrs)),
            Err(e) => {
                tracing::warn!(error = %e, "transport key not attested — publishing without it");
                None
            }
        },
        (Some(_), _, _) => {
            tracing::warn!("transport key offered without a pubkey and signature — ignoring");
            None
        }
        _ => None,
    };

    // Resolve a shortcode. A claimed name goes through the signed-ownership +
    // uniqueness path; no name falls back to an anonymous random shortcode.
    let (shortcode, display_name) = match &name {
        Some(raw) => {
            let slug = match validate_name(raw) {
                Ok(s) => s,
                Err(e) => {
                    send_err(&mut tx, &e).await;
                    return;
                }
            };
            let (Some(pubkey), Some(signature)) = (pubkey.as_ref(), signature.as_ref()) else {
                send_err(&mut tx, "claiming a name requires a pubkey and signature").await;
                return;
            };
            // Prove control of the key the name is bound to (signature over the
            // name as sent, against the challenge nonce).
            if let Err(e) = verify::verify_ownership(pubkey, signature, &nonce, raw) {
                send_err(&mut tx, &format!("name ownership rejected: {e}")).await;
                return;
            }
            match registry.claim_name(&slug, pubkey) {
                Ok(()) => {}
                Err(ClaimError::Taken) => {
                    send_err(&mut tx, &format!("the name '{slug}' is already taken")).await;
                    return;
                }
                Err(ClaimError::LiveElsewhere) => {
                    send_err(
                        &mut tx,
                        &format!("'{slug}' is currently in use by another session"),
                    )
                    .await;
                    return;
                }
            }
            (slug, Some(raw.clone()))
        }
        None => match claim_anonymous_shortcode(&registry).await {
            Some(c) => (c, None),
            None => {
                send_err(&mut tx, "could not claim a shortcode").await;
                return;
            }
        },
    };
    tracing::info!(%shortcode, host = ?display_name, public = publish_public, direct = ?endpoint, "host registered");

    let (control_tx, mut control_rx) = tokio::sync::mpsc::unbounded_channel::<RendezvousToHost>();
    let host_entry = HostEntry {
        name: display_name,
        description,
        public: publish_public,
        endpoint,
        transport_key: transport.as_ref().map(|(k, _)| k.clone()),
        transport_addrs: transport.map(|(_, a)| a).unwrap_or_default(),
        control_tx,
        // Stamped properly by try_claim; this is just a starting value.
        last_seen_ms: Default::default(),
    };
    // The live slot may momentarily race a same-slug reconnect; claim_name
    // already rejected a live-elsewhere claim, but re-check for anonymous.
    let Some(entry) = registry.try_claim(&shortcode, host_entry) else {
        send_err(&mut tx, "shortcode collision").await;
        return;
    };

    // Send the assigned shortcode + livekit_url, plus a per-session grant the
    // host uses to ask US to mint voice tokens. The signing secret stays here.
    let voice_token_grant = cfg
        .livekit_api_secret
        .is_some()
        .then(|| registry.issue_voice_grant(&shortcode));
    let registered = RendezvousToHost::Registered {
        shortcode: shortcode.clone(),
        livekit_url: cfg.livekit_url.clone(),
        voice_token_grant,
        relay_url: cfg.relay_url.clone(),
    };
    if let Ok(json) = serde_json::to_string(&registered)
        && tx.send(Message::Text(json)).await.is_err()
    {
        registry.release(&shortcode);
        return;
    }

    // Forward control messages from rendezvous to host (e.g. NewFriend
    // notifications) until the host disconnects, errors, or stops answering.
    //
    // The heartbeat is what makes "stops answering" detectable at all. A clean
    // shutdown sends Close and we drop the host immediately; the cases that
    // left stale entries in the browse list are the unclean ones — sleep, a
    // dropped Wi-Fi link, SIGKILL, a NAT that quietly forgets the flow. None of
    // those produce a Close or an error: the socket is half-open, `rx.next()`
    // blocks forever, and nothing ever removes the registration. (OS-level TCP
    // keepalive defaults to roughly two hours where it is enabled at all, which
    // is no help.) So we ping on a timer and give up when the replies stop.
    //
    // Nothing is needed on the host side: WebSocket Pong is automatic in the
    // client's tungstenite stack, so already-deployed hosts are covered too.
    let mut heartbeat = tokio::time::interval(cfg.heartbeat_interval);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    heartbeat.tick().await; // the first tick completes immediately
    loop {
        tokio::select! {
            outbound = control_rx.recv() => {
                let Some(msg) = outbound else { break };
                let Ok(json) = serde_json::to_string(&msg) else { continue };
                if tx.send(Message::Text(json)).await.is_err() {
                    break;
                }
            }
            inbound = rx.next() => {
                match inbound {
                    Some(Ok(Message::Close(_))) | None => break,
                    // Any frame at all proves the peer is there — Pong, or a
                    // control message we don't otherwise act on.
                    Some(Ok(_)) => entry.touch(),
                    Some(Err(e)) => {
                        tracing::warn!(%shortcode, err = %e, "host control recv error");
                        break;
                    }
                }
            }
            _ = heartbeat.tick() => {
                let idle_ms = entry.idle_ms();
                if idle_ms >= cfg.host_timeout.as_millis() as i64 {
                    tracing::info!(%shortcode, idle_ms, "host stopped answering heartbeats");
                    break;
                }
                // A send on a half-open socket often succeeds into the kernel
                // buffer, so this failing is a bonus rather than the mechanism —
                // the deadline above is what guarantees we let go.
                if tx.send(Message::Ping(Vec::new())).await.is_err() {
                    break;
                }
            }
        }
    }

    registry.release(&shortcode);
    tracing::info!(%shortcode, "host unregistered");
}

/// Pick a free random shortcode for an anonymous (unnamed) host.
async fn claim_anonymous_shortcode(registry: &Registry) -> Option<String> {
    for _ in 0..MAX_CLAIM_ATTEMPTS {
        let candidate = shortcode::generate();
        if !registry.hosts.contains_key(&candidate)
            && registry.reservation_owner(&candidate).is_none()
        {
            return Some(candidate);
        }
    }
    None
}

pub async fn handle_friend_join(socket: WebSocket, registry: Arc<Registry>, code: String) {
    let (mut friend_tx, mut friend_rx) = socket.split();

    // Join codes (named or anonymous) are matched case-insensitively — the slug
    // is the canonical lowercased form.
    let code = code.to_lowercase();
    let Some(host) = registry.hosts.get(&code).map(|h| h.value().clone()) else {
        send_err(
            &mut friend_tx,
            &format!("no host registered with code '{code}'"),
        )
        .await;
        return;
    };

    let session_id = Uuid::new_v4().to_string();
    let Some(host_socket_rx) = registry.open_pairing(&session_id) else {
        send_err(&mut friend_tx, "could not open pairing slot").await;
        return;
    };

    // Notify host so they open the matching proxy WS.
    if host
        .control_tx
        .send(RendezvousToHost::NewFriend {
            session_id: session_id.clone(),
        })
        .is_err()
    {
        send_err(&mut friend_tx, "host disconnected").await;
        return;
    }

    // Schedule a timeout in case the host never opens the proxy.
    registry
        .clone()
        .schedule_pairing_timeout(session_id.clone(), Duration::from_secs(10));

    // Wait for the host's proxy WS to arrive.
    let host_socket = match host_socket_rx.await {
        Ok(s) => s,
        Err(_) => {
            send_err(
                &mut friend_tx,
                "host did not open a proxy connection in time",
            )
            .await;
            return;
        }
    };
    tracing::info!(%code, %session_id, "pairing established");

    let (mut host_tx, mut host_rx) = host_socket.split();

    // Pipe both directions.
    let friend_to_host = async {
        while let Some(Ok(msg)) = friend_rx.next().await {
            if matches!(msg, Message::Close(_)) {
                break;
            }
            if host_tx.send(msg).await.is_err() {
                break;
            }
        }
        let _ = host_tx.close().await;
    };
    let host_to_friend = async {
        while let Some(Ok(msg)) = host_rx.next().await {
            if matches!(msg, Message::Close(_)) {
                break;
            }
            if friend_tx.send(msg).await.is_err() {
                break;
            }
        }
        let _ = friend_tx.close().await;
    };

    tokio::select! {
        _ = friend_to_host => {}
        _ = host_to_friend => {}
    }
    tracing::info!(%session_id, "pairing closed");
}

pub async fn handle_host_proxy(socket: WebSocket, registry: Arc<Registry>, session_id: String) {
    if !registry.fulfill_pairing(&session_id, socket).await {
        tracing::warn!(%session_id, "no waiting friend for proxy connection");
        // Socket is dropped; friend side has already timed out or moved on.
    }
}

/// Send any control frame to the host; returns Err if the socket is gone.
async fn send_msg<S>(tx: &mut S, msg: &RendezvousToHost) -> Result<(), ()>
where
    S: SinkExt<Message> + Unpin,
{
    let json = serde_json::to_string(msg).map_err(|_| ())?;
    tx.send(Message::Text(json)).await.map_err(|_| ())
}

async fn send_err<S>(tx: &mut S, message: &str)
where
    S: SinkExt<Message> + Unpin,
    <S as futures_util::Sink<Message>>::Error: std::fmt::Display,
{
    let err = RendezvousToHost::Error {
        message: message.to_string(),
    };
    if let Ok(json) = serde_json::to_string(&err) {
        let _ = tx.send(Message::Text(json)).await;
    }
}
