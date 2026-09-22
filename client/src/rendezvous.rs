use std::sync::{Arc, RwLock};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::protocol::rendezvous::{HostToRendezvous, RendezvousToHost};

/// A token request beyond this has failed; the server bounds the join too,
/// but a request left running would pile up behind a dead rendezvous.
const MINT_TIMEOUT: Duration = Duration::from_secs(8);

/// A registration attempt beyond this is a black hole, not a slow link.
const REGISTER_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone)]
pub struct PublishInfo {
    pub shortcode: String,
    pub livekit_url: Option<String>,
    pub voice_token_grant: Option<String>,
    pub rendezvous_base: String,
}

pub struct RendezvousMinter {
    endpoint: String,
    /// Replaced by `maintain` after every re-registration: the rendezvous
    /// keeps grants in memory, so its restart voids the one it gave us.
    grant: RwLock<String>,
    http: reqwest::Client,
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
            grant: RwLock::new(grant),
            http: reqwest::Client::builder()
                .timeout(MINT_TIMEOUT)
                .build()
                .unwrap_or_default(),
        }
    }

    pub fn set_grant(&self, grant: String) {
        *self.grant.write().unwrap_or_else(|e| e.into_inner()) = grant;
    }

    fn grant(&self) -> String {
        self.grant.read().unwrap_or_else(|e| e.into_inner()).clone()
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
            let res = self
                .http
                .post(&self.endpoint)
                .json(&serde_json::json!({
                    "grant": self.grant(),
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
            let token = res
                .json::<Resp>()
                .await
                .map_err(|e| format!("rendezvous mint response: {e}"))?
                .token;
            grants_match(&token, req.can_publish)?;
            Ok(token)
        })
    }
}

/// The rendezvous signs with a secret this host never holds, so the claims
/// cannot be verified — but they can be read, and a subscribe-only identity
/// handed publish rights is refused here rather than passed to the client.
fn grants_match(token: &str, can_publish: bool) -> Result<(), String> {
    use base64::Engine as _;
    let payload = token
        .split('.')
        .nth(1)
        .ok_or("rendezvous token is not a JWT")?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| "rendezvous token payload is not base64url")?;
    let claims: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| "rendezvous token payload is not JSON")?;
    let video = &claims["video"];
    let publish = video["canPublish"].as_bool().unwrap_or(false);
    let data = video["canPublishData"].as_bool().unwrap_or(false);
    if publish != can_publish || data != can_publish {
        return Err(format!(
            "rendezvous minted publish={publish} data={data} for an identity that asked for \
             publish={can_publish}"
        ));
    }
    Ok(())
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
    pub location: Option<crate::protocol::rendezvous::GeoPoint>,
}

pub async fn register(
    rendezvous_url: &str,
    options: &PublishOptions,
    transport: Option<&TransportAdvert>,
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

    let (transport_key, transport_signature, transport_addrs) = match transport {
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
    let pubkey = pubkey.or_else(|| transport.map(|_| identity.pubkey.clone()));

    let hello = HostToRendezvous::Register {
        name: options.publish_name.clone(),
        pubkey,
        signature,
        publish_public: options.publish_public,
        description: options.description.clone(),
        transport_key,
        transport_signature,
        transport_addrs,
        location: options.location,
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
            RendezvousToHost::Challenge { .. } => continue,
        }
    }
}

pub struct ControlStream {
    ws: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostUpdate {
    RendezvousLost { error: String },
    RendezvousRestored { shortcode: String },
}

/// Everything a second registration needs; the frame is signed afresh each time.
pub struct Registration {
    pub url: String,
    pub options: PublishOptions,
    pub transport: Option<TransportAdvert>,
    pub identity: crate::identity::Identity,
}

/// Answers the rendezvous's pings, and when the stream ends registers again
/// with backoff: its memory is all that holds the listing, the pin and the voice grant.
pub fn maintain(
    mut stream: ControlStream,
    registration: Registration,
    minter: Option<Arc<RendezvousMinter>>,
    updates: tokio::sync::mpsc::UnboundedSender<HostUpdate>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            while let Some(Ok(frame)) = stream.ws.next().await {
                if matches!(frame, WsMessage::Close(_)) {
                    break;
                }
            }
            eprintln!("[rendezvous] control stream ended — registering again");
            tracing::warn!("rendezvous control stream ended; registering again");
            let mut attempt = 0u32;
            stream = loop {
                tokio::time::sleep(backoff(attempt)).await;
                attempt += 1;
                let again = tokio::time::timeout(
                    REGISTER_TIMEOUT,
                    register(
                        &registration.url,
                        &registration.options,
                        registration.transport.as_ref(),
                        &registration.identity,
                    ),
                )
                .await
                .unwrap_or_else(|_| Err("no answer from the rendezvous".to_string()));
                match again {
                    Ok((info, control)) => {
                        if let (Some(m), Some(grant)) = (&minter, info.voice_token_grant) {
                            m.set_grant(grant);
                        }
                        eprintln!("[rendezvous] registered again as {}", info.shortcode);
                        tracing::info!(shortcode = %info.shortcode, attempt, "rendezvous registration restored");
                        let _ = updates.send(HostUpdate::RendezvousRestored {
                            shortcode: info.shortcode,
                        });
                        break control;
                    }
                    Err(error) => {
                        tracing::warn!(%error, attempt, "rendezvous re-registration failed");
                        let _ = updates.send(HostUpdate::RendezvousLost { error });
                    }
                }
            };
        }
    })
}

/// 1 s, 2 s, 4 s … 32 s: quick after a blip, patient through a restart.
pub fn backoff(attempt: u32) -> Duration {
    Duration::from_secs(1u64 << attempt.min(5))
}

#[cfg(test)]
mod backoff_tests {
    use super::*;

    #[test]
    fn backoff_doubles_and_caps() {
        assert_eq!(backoff(0), Duration::from_secs(1));
        assert_eq!(backoff(3), Duration::from_secs(8));
        assert_eq!(backoff(5), Duration::from_secs(32));
        assert_eq!(backoff(40), Duration::from_secs(32));
    }
}

#[cfg(test)]
mod grant_tests {
    use super::grants_match;
    use livekit_api::access_token::{AccessToken, VideoGrants};

    fn token(can_publish: bool) -> String {
        AccessToken::with_api_key("devkey", "a-secret-long-enough-for-hs256-signing")
            .with_identity("someone#audio")
            .with_grants(VideoGrants {
                room_join: true,
                room: "code--screen-1".into(),
                can_publish,
                can_subscribe: true,
                can_publish_data: can_publish,
                ..Default::default()
            })
            .to_jwt()
            .expect("mint")
    }

    #[test]
    fn a_token_is_only_accepted_with_the_grant_that_was_asked_for() {
        assert!(grants_match(&token(false), false).is_ok());
        assert!(grants_match(&token(true), true).is_ok());
        let err = grants_match(&token(true), false).expect_err("publish handed to a subscriber");
        assert!(err.contains("publish=true"), "{err}");
        assert!(
            grants_match(&token(false), true).is_err(),
            "a publisher denied publish"
        );
    }

    #[test]
    fn garbage_is_not_a_token() {
        assert!(grants_match("not-a-jwt", false).is_err());
        assert!(grants_match("a.!!!.c", false).is_err());
        assert!(grants_match("a.bm90IGpzb24.c", false).is_err());
    }
}
