use std::net::IpAddr;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket};
use dioxusfun_protocol::rendezvous::{HostToRendezvous, RendezvousToHost};
use futures_util::{SinkExt, StreamExt};

use crate::Config;
use crate::registry::{ClaimError, HostEntry, Registry, ReleaseError, validate_name};
use crate::shortcode;
use crate::verify;

const MAX_CLAIM_ATTEMPTS: usize = 10;
const MAX_DESCRIPTION_CHARS: usize = 140;

/// A LAN or loopback address reaches nobody a friend could not already reach
/// from where they stand; a public one has to be the registrant's own.
fn is_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_loopback() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

/// An address is `ip:port` for a hole-punch candidate or a relay URL; the only
/// relay a friend should ever be sent to is this coordinator's own.
fn admits_transport_addr(addr: &str, peer: Option<IpAddr>, relay_url: Option<&str>) -> bool {
    if let Ok(socket) = addr.parse::<std::net::SocketAddr>() {
        return peer.is_none_or(|peer| socket.ip() == peer || is_local(socket.ip()));
    }
    relay_url.is_some_and(|relay| relay.trim_end_matches('/') == addr.trim_end_matches('/'))
}

pub async fn handle_host_control(
    socket: WebSocket,
    registry: Arc<Registry>,
    cfg: Arc<Config>,
    peer: Option<IpAddr>,
) {
    let (mut tx, mut rx) = socket.split();

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

    let first = match tokio::time::timeout(cfg.register_timeout, rx.next()).await {
        Ok(frame) => frame,
        Err(_) => {
            send_err(&mut tx, "no register frame in time").await;
            return;
        }
    };
    let register = match first {
        Some(Ok(Message::Text(t))) => match serde_json::from_str::<HostToRendezvous>(&t) {
            Ok(HostToRendezvous::Register {
                name,
                pubkey,
                signature,
                publish_public,
                description,
                transport_key,
                transport_signature,
                transport_addrs,
            }) => (
                name,
                pubkey,
                signature,
                publish_public,
                description,
                transport_key,
                transport_signature,
                transport_addrs,
            ),
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
        transport_key,
        transport_signature,
        transport_addrs,
    ) = register;

    // Only the name and the transport key are signed, so where the entry points
    // is checked against where the frame came from: else it aims friends at a stranger.
    let transport_addrs: Vec<String> = transport_addrs
        .into_iter()
        .filter(|a| admits_transport_addr(a, peer, cfg.relay_url.as_deref()))
        .collect();
    let description = description
        .map(|d| dioxusfun_protocol::sanitize_line(&d, MAX_DESCRIPTION_CHARS))
        .filter(|d| !d.is_empty());

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
                Err(ClaimError::OwnerLimit) => {
                    send_err(
                        &mut tx,
                        "this key already holds its share of names — release one first",
                    )
                    .await;
                    return;
                }
                Err(ClaimError::Full) => {
                    send_err(&mut tx, "this rendezvous is not taking new names").await;
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
    tracing::info!(%shortcode, host = ?display_name, public = publish_public, "host registered");

    let host_entry = HostEntry {
        name: display_name,
        description,
        public: publish_public,
        transport_key: transport.as_ref().map(|(k, _)| k.clone()),
        transport_addrs: transport.map(|(_, a)| a).unwrap_or_default(),
        last_seen_ms: Default::default(),
    };
    let Some(entry) = registry.try_claim(&shortcode, host_entry) else {
        send_err(&mut tx, "shortcode collision").await;
        return;
    };

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

    // Detects half-open sockets (sleep, NAT reset), which never send anything
    // and would otherwise stay registered forever.
    let mut heartbeat = tokio::time::interval(cfg.heartbeat_interval);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    heartbeat.tick().await; // the first tick completes immediately
    loop {
        tokio::select! {
            inbound = rx.next() => {
                match inbound {
                    Some(Ok(Message::Close(_))) | None => break,
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
                if tx.send(Message::Ping(Vec::new())).await.is_err() {
                    break;
                }
            }
        }
    }

    registry.release(&shortcode);
    tracing::info!(%shortcode, "host unregistered");
}

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

pub async fn refuse(mut socket: WebSocket, message: &str) {
    send_err(&mut socket, message).await;
    let _ = socket.close().await;
}

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
{
    let err = RendezvousToHost::Error {
        message: message.to_string(),
    };
    if let Ok(json) = serde_json::to_string(&err) {
        let _ = tx.send(Message::Text(json)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn a_transport_address_is_the_registrant_a_lan_or_our_own_relay() {
        let peer = Some(ip("203.0.113.5"));
        let relay = Some("https://relay.example/");
        assert!(admits_transport_addr("203.0.113.5:4433", peer, relay));
        assert!(admits_transport_addr("10.0.0.7:4433", peer, relay));
        assert!(admits_transport_addr("[fd00::1]:4433", peer, relay));
        assert!(!admits_transport_addr("198.51.100.9:4433", peer, relay));
        assert!(admits_transport_addr("https://relay.example", peer, relay));
        assert!(!admits_transport_addr("https://evil.example/", peer, relay));
        assert!(
            !admits_transport_addr("https://relay.example", peer, None),
            "no relay configured"
        );
    }
}
