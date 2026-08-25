use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use rand::RngCore;
use secp256k1::{Keypair, Message, Secp256k1, SecretKey};
use sha2::{Digest, Sha256};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

pub use dioxusfun_protocol as protocol;
use protocol::{ClientMessage, Id, ServerMessage, User};

type Stream = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Debug)]
pub enum BotError {
    Url(String),
    Ws(String),
    Json(String),
    Handshake(String),
}

impl std::fmt::Display for BotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BotError::Url(e) => write!(f, "url error: {e}"),
            BotError::Ws(e) => write!(f, "websocket error: {e}"),
            BotError::Json(e) => write!(f, "json error: {e}"),
            BotError::Handshake(e) => write!(f, "handshake error: {e}"),
        }
    }
}

impl std::error::Error for BotError {}

type Result<T> = std::result::Result<T, BotError>;

pub struct BotIdentity {
    secret: SecretKey,
    pubkey_hex: String,
}

impl BotIdentity {
    pub fn generate() -> Self {
        loop {
            let mut secret = [0u8; 32];
            rand::rngs::OsRng.fill_bytes(&mut secret);
            if let Ok(id) = Self::try_from_secret_bytes(&secret) {
                return id;
            }
        }
    }

    fn try_from_secret_bytes(secret: &[u8; 32]) -> std::result::Result<Self, secp256k1::Error> {
        let secp = Secp256k1::new();
        let sk = SecretKey::from_slice(secret)?;
        let (xonly, _parity) = sk.x_only_public_key(&secp);
        Ok(Self {
            secret: sk,
            pubkey_hex: hex::encode(xonly.serialize()),
        })
    }

    pub fn from_base58_secret(secret_b58: &str) -> Result<Self> {
        let bytes = bs58::decode(secret_b58.trim())
            .into_vec()
            .map_err(|e| BotError::Url(format!("secret not base58: {e}")))?;
        let arr: [u8; 32] = bytes
            .try_into()
            .map_err(|_| BotError::Url("secret must be 32 bytes".into()))?;
        Self::try_from_secret_bytes(&arr).map_err(|e| BotError::Url(format!("invalid secret: {e}")))
    }

    pub fn pubkey(&self) -> &str {
        &self.pubkey_hex
    }

    pub fn secret_base58(&self) -> String {
        bs58::encode(self.secret.secret_bytes()).into_string()
    }

    fn sign_identify(&self, nonce: &str, username: &str) -> String {
        let secp = Secp256k1::new();
        let keypair = Keypair::from_secret_key(&secp, &self.secret);
        let mut msg = Vec::with_capacity(nonce.len() + self.pubkey_hex.len() + username.len());
        msg.extend_from_slice(nonce.as_bytes());
        msg.extend_from_slice(self.pubkey_hex.as_bytes());
        msg.extend_from_slice(username.as_bytes());
        let digest: [u8; 32] = Sha256::digest(&msg).into();
        let m = Message::from_digest(digest);
        hex::encode(secp.sign_schnorr_no_aux_rand(&m, &keypair).serialize())
    }
}

pub struct Bot {
    write: SplitSink<Stream, WsMessage>,
    read: SplitStream<Stream>,
    pub user: User,
}

impl Bot {
    pub async fn connect(url: &str, identity: &BotIdentity, username: &str) -> Result<Bot> {
        Self::connect_declaring(url, identity, username, true).await
    }

    pub async fn connect_as_user(url: &str, identity: &BotIdentity, username: &str) -> Result<Bot> {
        Self::connect_declaring(url, identity, username, false).await
    }

    async fn connect_declaring(
        url: &str,
        identity: &BotIdentity,
        username: &str,
        bot: bool,
    ) -> Result<Bot> {
        let ws_url = normalize_gateway_url(url)?;
        let (stream, _) = tokio_tungstenite::connect_async(&ws_url)
            .await
            .map_err(|e| BotError::Ws(format!("connect failed: {e}")))?;
        let (write, mut read) = stream.split();

        let nonce = loop {
            let frame = read
                .next()
                .await
                .ok_or_else(|| BotError::Handshake("server closed before Hello".into()))?
                .map_err(|e| BotError::Ws(e.to_string()))?;
            match frame {
                WsMessage::Text(t) => {
                    let parsed: ServerMessage =
                        serde_json::from_str(&t).map_err(|e| BotError::Json(e.to_string()))?;
                    match parsed {
                        ServerMessage::Hello { nonce } => break nonce,
                        other => {
                            return Err(BotError::Handshake(format!(
                                "expected Hello, got {other:?}"
                            )));
                        }
                    }
                }
                WsMessage::Close(_) => {
                    return Err(BotError::Handshake("server closed before Hello".into()));
                }
                _ => continue,
            }
        };

        // The server canonicalizes before verifying, so signing the raw
        // string fails for names over 32 chars.
        let username = dioxusfun_protocol::canonical_username(username);
        let signature = identity.sign_identify(&nonce, &username);
        let identify = ClientMessage::Identify {
            username: username.clone(),
            pubkey: identity.pubkey().to_string(),
            signature,
            bot,
            client_version: concat!("bot-sdk/", env!("CARGO_PKG_VERSION")).to_string(),
        };
        let mut bot = Bot {
            write,
            read,
            user: User {
                pubkey: identity.pubkey().to_string(),
                username,
            },
        };
        bot.send(&identify).await?;
        Ok(bot)
    }

    pub async fn next_event(&mut self) -> Option<ServerMessage> {
        while let Some(frame) = self.read.next().await {
            let frame = frame.ok()?;
            match frame {
                WsMessage::Text(t) => match serde_json::from_str::<ServerMessage>(&t) {
                    Ok(msg) => return Some(msg),
                    // Skip a frame we cannot parse rather than kill the loop.
                    Err(_) => continue,
                },
                WsMessage::Close(_) => return None,
                _ => continue,
            }
        }
        None
    }

    pub async fn send_message(&mut self, channel_id: Id, content: &str) -> Result<()> {
        self.send(&ClientMessage::SendMessage {
            channel_id,
            content: content.to_string(),
            image: None,
            reply_to: None,
        })
        .await
    }

    pub async fn reply_message(
        &mut self,
        channel_id: Id,
        content: &str,
        reply_to: Id,
    ) -> Result<()> {
        self.send(&ClientMessage::SendMessage {
            channel_id,
            content: content.to_string(),
            image: None,
            reply_to: Some(reply_to),
        })
        .await
    }

    pub async fn react(&mut self, channel_id: Id, message_id: Id, emoji: &str) -> Result<()> {
        self.send(&ClientMessage::React {
            channel_id,
            message_id,
            emoji: emoji.to_string(),
        })
        .await
    }

    pub async fn send(&mut self, msg: &ClientMessage) -> Result<()> {
        let json = serde_json::to_string(msg).map_err(|e| BotError::Json(e.to_string()))?;
        self.write
            .send(WsMessage::Text(json))
            .await
            .map_err(|e| BotError::Ws(e.to_string()))
    }
}

fn normalize_gateway_url(raw: &str) -> Result<String> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err(BotError::Url("server URL is required".into()));
    }
    let with_scheme = if trimmed.starts_with("ws://") || trimmed.starts_with("wss://") {
        trimmed.to_string()
    } else if let Some(rest) = trimmed.strip_prefix("http://") {
        format!("ws://{rest}")
    } else if let Some(rest) = trimmed.strip_prefix("https://") {
        format!("wss://{rest}")
    } else {
        format!("ws://{trimmed}")
    };
    let mut url = url::Url::parse(&with_scheme).map_err(|e| BotError::Url(e.to_string()))?;
    url.set_path("/gateway");
    Ok(url.to_string())
}
