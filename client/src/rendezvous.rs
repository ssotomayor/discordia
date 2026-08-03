//! Host-side rendezvous client.
//!
//! When self-host mode publishes to a rendezvous, this module:
//! 1. Opens a long-lived control WebSocket to `{rendezvous}/control`
//! 2. Sends `Register`, receives `Registered { shortcode, livekit_url }`
//! 3. For each `NewFriend { session_id }` notification, opens a pair of
//!    WebSockets — one outbound to `{rendezvous}/proxy/{session_id}` and one
//!    inbound to the local gateway — and pipes frames between them.

use std::net::SocketAddr;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio_tungstenite::tungstenite::Message as WsMessage;

#[derive(Debug, Serialize)]
#[serde(tag = "op", content = "d", rename_all = "snake_case")]
enum HostToRendezvous {
    Register {
        name: Option<String>,
        pubkey: Option<String>,
        signature: Option<String>,
        publish_public: bool,
        description: Option<String>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op", content = "d", rename_all = "snake_case")]
enum RendezvousToHost {
    Challenge {
        nonce: String,
    },
    Registered {
        shortcode: String,
        livekit_url: Option<String>,
        #[serde(default)]
        livekit_api_key: Option<String>,
        #[serde(default)]
        livekit_api_secret: Option<String>,
    },
    NewFriend {
        session_id: String,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone)]
pub struct PublishInfo {
    pub shortcode: String,
    pub livekit_url: Option<String>,
    /// Credentials for the rendezvous operator's shared LiveKit, when it runs
    /// one. We must sign JoinVoice tokens with these or that SFU rejects them.
    pub livekit_api_key: Option<String>,
    pub livekit_api_secret: Option<String>,
    pub rendezvous_base: String,
}

#[derive(Debug, Clone, Default)]
pub struct PublishOptions {
    pub publish_name: Option<String>,
    pub description: Option<String>,
    pub publish_public: bool,
}

/// Connect to rendezvous, register, and return the published info.
/// The caller then runs [`run_adapter`] to handle ongoing NewFriend events.
///
/// `identity` signs the ownership proof when a name is claimed (`publish_name`
/// set) — the rendezvous binds the name to this key and persists the claim.
pub async fn register(
    rendezvous_url: &str,
    options: PublishOptions,
    identity: &crate::identity::Identity,
) -> Result<(PublishInfo, ControlStream), String> {
    let base = rendezvous_url.trim_end_matches('/').to_string();
    let control_url = format!("{base}/control");

    let (mut ws, _) = tokio_tungstenite::connect_async(&control_url)
        .await
        .map_err(|e| format!("rendezvous control connect: {e}"))?;

    // The rendezvous opens with a Challenge nonce we sign to prove name
    // ownership. Wait for it before registering.
    let nonce = loop {
        let frame = ws
            .next()
            .await
            .ok_or_else(|| "rendezvous closed before challenge".to_string())?
            .map_err(|e| format!("rendezvous recv: {e}"))?;
        if let WsMessage::Text(t) = frame {
            match serde_json::from_str::<RendezvousToHost>(&t) {
                Ok(RendezvousToHost::Challenge { nonce }) => break nonce,
                Ok(RendezvousToHost::Error { message }) => return Err(message),
                _ => continue,
            }
        }
    };

    // If we're claiming a name, sign SHA256(nonce || pubkey || name) so the
    // rendezvous can verify we control the key the name is bound to.
    let (pubkey, signature) = match options.publish_name.as_deref() {
        Some(name) => {
            let pubkey = identity.pubkey.clone();
            let mut msg = Vec::new();
            msg.extend_from_slice(nonce.as_bytes());
            msg.extend_from_slice(pubkey.as_bytes());
            msg.extend_from_slice(name.as_bytes());
            (Some(pubkey), Some(identity.sign_hex(&msg)))
        }
        None => (None, None),
    };

    let hello = HostToRendezvous::Register {
        name: options.publish_name,
        pubkey,
        signature,
        publish_public: options.publish_public,
        description: options.description,
    };
    let json = serde_json::to_string(&hello).map_err(|e| e.to_string())?;
    ws.send(WsMessage::Text(json.into()))
        .await
        .map_err(|e| format!("send register: {e}"))?;

    loop {
        let frame = ws
            .next()
            .await
            .ok_or_else(|| "rendezvous closed before registered".to_string())?
            .map_err(|e| format!("rendezvous recv: {e}"))?;
        let text = match frame {
            WsMessage::Text(t) => t.to_string(),
            _ => continue,
        };
        let parsed: RendezvousToHost = serde_json::from_str(&text)
            .map_err(|e| format!("bad rendezvous frame: {e}"))?;
        match parsed {
            RendezvousToHost::Registered {
                shortcode,
                livekit_url,
                livekit_api_key,
                livekit_api_secret,
            } => {
                let info = PublishInfo {
                    shortcode,
                    livekit_url,
                    livekit_api_key,
                    livekit_api_secret,
                    rendezvous_base: base.clone(),
                };
                return Ok((info, ControlStream { ws }));
            }
            RendezvousToHost::Error { message } => return Err(message),
            RendezvousToHost::NewFriend { .. } | RendezvousToHost::Challenge { .. } => continue,
        }
    }
}

/// The still-open control socket after Register/Registered. Pass to
/// [`run_adapter`] to start receiving NewFriend notifications.
pub struct ControlStream {
    ws: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
}

/// Run the long-lived adapter loop: receive NewFriend notifications on the
/// control stream, and for each spawn a task that bridges the rendezvous
/// proxy WS to the local gateway.
pub fn run_adapter(
    mut stream: ControlStream,
    rendezvous_base: String,
    local_gateway_addr: SocketAddr,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(frame) = stream.ws.next().await {
            let frame = match frame {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("[rendezvous] control recv error: {e}");
                    break;
                }
            };
            let text = match frame {
                WsMessage::Text(t) => t.to_string(),
                WsMessage::Close(_) => break,
                _ => continue,
            };
            let Ok(msg) = serde_json::from_str::<RendezvousToHost>(&text) else {
                continue;
            };
            if let RendezvousToHost::NewFriend { session_id } = msg {
                let proxy_url = format!("{rendezvous_base}/proxy/{session_id}");
                let sid = session_id.clone();
                tokio::spawn(async move {
                    if let Err(e) = bridge_friend(&proxy_url, local_gateway_addr).await {
                        eprintln!("[rendezvous] bridge {sid} failed: {e}");
                    }
                });
            }
        }
        eprintln!("[rendezvous] control stream ended");
    })
}

async fn bridge_friend(proxy_url: &str, local_gateway: SocketAddr) -> Result<(), String> {
    let (proxy_ws, _) = tokio_tungstenite::connect_async(proxy_url)
        .await
        .map_err(|e| format!("proxy connect: {e}"))?;

    let local_url = format!("ws://{}/gateway", local_gateway);
    let (gw_ws, _) = tokio_tungstenite::connect_async(&local_url)
        .await
        .map_err(|e| format!("local gateway connect: {e}"))?;

    let (mut proxy_tx, mut proxy_rx) = proxy_ws.split();
    let (mut gw_tx, mut gw_rx) = gw_ws.split();

    let to_gw = async move {
        while let Some(Ok(msg)) = proxy_rx.next().await {
            if matches!(msg, WsMessage::Close(_)) {
                break;
            }
            if gw_tx.send(msg).await.is_err() {
                break;
            }
        }
    };
    let to_friend = async move {
        while let Some(Ok(msg)) = gw_rx.next().await {
            if matches!(msg, WsMessage::Close(_)) {
                break;
            }
            if proxy_tx.send(msg).await.is_err() {
                break;
            }
        }
    };

    tokio::select! {
        _ = to_gw => {}
        _ = to_friend => {}
    }
    Ok(())
}
