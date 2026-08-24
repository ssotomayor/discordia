use std::net::SocketAddr;

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::protocol::rendezvous::{HostToRendezvous, RendezvousToHost};

#[derive(Debug, Clone)]
pub struct PublishInfo {
    pub shortcode: String,
    pub livekit_url: Option<String>,
    pub voice_token_grant: Option<String>,
    pub rendezvous_base: String,
}

pub struct RendezvousMinter {
    endpoint: String,
    grant: String,
}

impl RendezvousMinter {
    pub fn new(rendezvous_base: &str, grant: String) -> Self {
        let http = if let Some(rest) = rendezvous_base.strip_prefix("wss://") {
            format!("https://{rest}")
        } else if let Some(rest) = rendezvous_base.strip_prefix("ws://") {
            format!("http://{rest}")
        } else {
            rendezvous_base.to_string()
        };
        Self {
            endpoint: format!("{}/voice-token", http.trim_end_matches('/')),
            grant,
        }
    }
}

impl dioxusfun_server::livekit::VoiceTokenMinter for RendezvousMinter {
    fn mint<'a>(
        &'a self,
        req: dioxusfun_server::livekit::MintRequest,
    ) -> dioxusfun_server::livekit::BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            #[derive(serde::Deserialize)]
            struct Resp {
                token: String,
            }
            let res = reqwest::Client::new()
                .post(&self.endpoint)
                .json(&serde_json::json!({
                    "grant": self.grant,
                    "room": req.room,
                    "identity": req.identity,
                    "name": req.name,
                    "can_publish": req.can_publish,
                }))
                .send()
                .await
                .map_err(|e| format!("rendezvous mint request: {e}"))?;
            if !res.status().is_success() {
                let code = res.status();
                let body = res.text().await.unwrap_or_default();
                return Err(format!("rendezvous mint rejected ({code}): {body}"));
            }
            Ok(res
                .json::<Resp>()
                .await
                .map_err(|e| format!("rendezvous mint response: {e}"))?
                .token)
        })
    }
}

pub async fn coordination_offered(rendezvous_url: &str) -> dioxusfun_server::quic::Coordination {
    #[derive(serde::Deserialize)]
    struct Offered {
        #[serde(default)]
        relay_url: Option<String>,
    }
    let http = if let Some(rest) = rendezvous_url.strip_prefix("wss://") {
        format!("https://{rest}")
    } else if let Some(rest) = rendezvous_url.strip_prefix("ws://") {
        format!("http://{rest}")
    } else {
        rendezvous_url.to_string()
    };
    let fetched = reqwest::Client::new()
        .get(format!("{}/config", http.trim_end_matches('/')))
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .ok()
        .and_then(|r| r.error_for_status().ok());
    let relay = match fetched {
        Some(r) => r.json::<Offered>().await.ok().and_then(|o| o.relay_url),
        None => None,
    };
    match relay {
        Some(url) => {
            eprintln!("[host] rendezvous offers a coordinator at {url}");
            dioxusfun_server::quic::Coordination::Relay(url)
        }
        None => {
            eprintln!("[host] rendezvous offers no coordinator — no hole punching");
            dioxusfun_server::quic::Coordination::None
        }
    }
}

#[derive(Debug, Clone)]
pub struct TransportAdvert {
    pub key: String,
    pub addrs: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct PublishOptions {
    pub publish_name: Option<String>,
    pub description: Option<String>,
    pub publish_public: bool,
}

pub async fn register(
    rendezvous_url: &str,
    options: PublishOptions,
    endpoint: Option<String>,
    transport: Option<TransportAdvert>,
    identity: &crate::identity::Identity,
) -> Result<(PublishInfo, ControlStream), String> {
    let base = rendezvous_url.trim_end_matches('/').to_string();
    let control_url = format!("{base}/control");

    let (mut ws, _) = tokio_tungstenite::connect_async(&control_url)
        .await
        .map_err(|e| format!("rendezvous control connect: {e}"))?;

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

    let (transport_key, transport_signature, transport_addrs) = match &transport {
        Some(t) => {
            let pk = identity.pubkey.clone();
            let mut msg = Vec::new();
            msg.extend_from_slice(nonce.as_bytes());
            msg.extend_from_slice(pk.as_bytes());
            msg.extend_from_slice(t.key.as_bytes());
            (
                Some(t.key.clone()),
                Some(identity.sign_hex(&msg)),
                t.addrs.clone(),
            )
        }
        None => (None, None, Vec::new()),
    };
    let pubkey = pubkey.or_else(|| transport.as_ref().map(|_| identity.pubkey.clone()));

    let hello = HostToRendezvous::Register {
        name: options.publish_name,
        pubkey,
        signature,
        publish_public: options.publish_public,
        description: options.description,
        endpoint,
        transport_key,
        transport_signature,
        transport_addrs,
    };
    let json = serde_json::to_string(&hello).map_err(|e| e.to_string())?;
    ws.send(WsMessage::Text(json))
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
        let parsed: RendezvousToHost =
            serde_json::from_str(&text).map_err(|e| format!("bad rendezvous frame: {e}"))?;
        match parsed {
            RendezvousToHost::Registered {
                shortcode,
                livekit_url,
                voice_token_grant,
                ..
            } => {
                let info = PublishInfo {
                    shortcode,
                    livekit_url,
                    voice_token_grant,
                    rendezvous_base: base.clone(),
                };
                return Ok((info, ControlStream { ws }));
            }
            RendezvousToHost::Error { message } => return Err(message),
            RendezvousToHost::Released { name } => {
                eprintln!("[rendezvous] unexpected release confirmation for '{name}'");
                continue;
            }
            RendezvousToHost::NewFriend { .. } | RendezvousToHost::Challenge { .. } => continue,
        }
    }
}

pub struct ControlStream {
    ws: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
}

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
