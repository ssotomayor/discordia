//! WebSocket handlers: host control, friend join, host proxy pairing.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use uuid::Uuid;

use crate::Config;
use crate::protocol::{HostToRendezvous, RendezvousToHost};
use crate::registry::{HostEntry, Registry};
use crate::shortcode;

const MAX_CLAIM_ATTEMPTS: usize = 10;

pub async fn handle_host_control(socket: WebSocket, registry: Arc<Registry>, cfg: Arc<Config>) {
    let (mut tx, mut rx) = socket.split();

    // First frame must be a Register.
    let register = match rx.next().await {
        Some(Ok(Message::Text(t))) => match serde_json::from_str::<HostToRendezvous>(&t) {
            Ok(HostToRendezvous::Register {
                name,
                preferred,
                publish_public,
                description,
            }) => (name, preferred, publish_public, description),
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
    let (name, preferred, publish_public, description) = register;

    // Claim a shortcode.
    let shortcode = match claim_shortcode(&registry, preferred, &name).await {
        Some(c) => c,
        None => {
            send_err(&mut tx, "could not claim a shortcode").await;
            return;
        }
    };
    tracing::info!(%shortcode, host = ?name, public = publish_public, "host registered");

    let (control_tx, mut control_rx) =
        tokio::sync::mpsc::unbounded_channel::<RendezvousToHost>();
    let host_entry = HostEntry {
        name: name.clone(),
        description,
        public: publish_public,
        control_tx,
    };
    // We already verified the shortcode is free in claim_shortcode, but
    // re-check race conditions just in case.
    if !registry.try_claim(&shortcode, host_entry) {
        send_err(&mut tx, "shortcode collision").await;
        return;
    }

    // Send the assigned shortcode + livekit_url.
    let registered = RendezvousToHost::Registered {
        shortcode: shortcode.clone(),
        livekit_url: cfg.livekit_url.clone(),
    };
    if let Ok(json) = serde_json::to_string(&registered) {
        if tx.send(Message::Text(json.into())).await.is_err() {
            registry.release(&shortcode);
            return;
        }
    }

    // Forward control messages from rendezvous to host (e.g. NewFriend
    // notifications) until the host disconnects or an error occurs.
    loop {
        tokio::select! {
            outbound = control_rx.recv() => {
                let Some(msg) = outbound else { break };
                let Ok(json) = serde_json::to_string(&msg) else { continue };
                if tx.send(Message::Text(json.into())).await.is_err() {
                    break;
                }
            }
            inbound = rx.next() => {
                match inbound {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {} // ignore additional frames for now
                    Some(Err(e)) => {
                        tracing::warn!(%shortcode, err = %e, "host control recv error");
                        break;
                    }
                }
            }
        }
    }

    registry.release(&shortcode);
    tracing::info!(%shortcode, "host unregistered");
}

async fn claim_shortcode(
    registry: &Registry,
    preferred: Option<String>,
    _name: &Option<String>,
) -> Option<String> {
    if let Some(p) = preferred.as_ref().filter(|s| valid_shortcode(s)) {
        if !registry.hosts.contains_key(p) {
            return Some(p.clone());
        }
    }
    for _ in 0..MAX_CLAIM_ATTEMPTS {
        let candidate = shortcode::generate();
        if !registry.hosts.contains_key(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn valid_shortcode(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

pub async fn handle_friend_join(socket: WebSocket, registry: Arc<Registry>, code: String) {
    let (mut friend_tx, mut friend_rx) = socket.split();

    let Some(host) = registry.hosts.get(&code).map(|h| h.value().clone()) else {
        send_err(&mut friend_tx, &format!("no host registered with code '{code}'")).await;
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
            send_err(&mut friend_tx, "host did not open a proxy connection in time").await;
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

async fn send_err<S>(tx: &mut S, message: &str)
where
    S: SinkExt<Message> + Unpin,
    <S as futures_util::Sink<Message>>::Error: std::fmt::Display,
{
    let err = RendezvousToHost::Error {
        message: message.to_string(),
    };
    if let Ok(json) = serde_json::to_string(&err) {
        let _ = tx.send(Message::Text(json.into())).await;
    }
}
