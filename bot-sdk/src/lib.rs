//! # dioxusfun-bot
//!
//! A thin async client for writing **Tier 1 bots** — external programs that
//! connect to a dioxusfun gateway over WebSocket and react to events, exactly
//! like Discord bots. A bot is just a Nostr-style secp256k1 keypair; there is
//! no bearer token to leak. A guild owner installs your bot by its public key
//! (64 hex chars) and grants it permissions + intents; the server enforces both.
//!
//! ```no_run
//! use dioxusfun_bot::{Bot, BotIdentity};
//! use dioxusfun_protocol::ServerMessage;
//!
//! # async fn run() -> Result<(), dioxusfun_bot::BotError> {
//! let identity = BotIdentity::generate();
//! println!("install me by pubkey: {}", identity.pubkey());
//!
//! let mut bot = Bot::connect("ws://localhost:9000", &identity, "PingBot").await?;
//! while let Some(event) = bot.next_event().await {
//!     if let ServerMessage::MessageCreate(msg) = event {
//!         if msg.content == "!ping" {
//!             bot.send_message(msg.channel_id, "pong").await?;
//!         }
//!     }
//! }
//! # Ok(())
//! # }
//! ```

use secp256k1::{Keypair, Message, Secp256k1, SecretKey};
use sha2::{Digest, Sha256};
use futures_util::{SinkExt, StreamExt};
use futures_util::stream::{SplitSink, SplitStream};
use rand::RngCore;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

pub use dioxusfun_protocol as protocol;
use protocol::{ClientMessage, Id, ServerMessage, User};

type Stream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Errors surfaced by the SDK.
#[derive(Debug)]
pub enum BotError {
    /// URL parse / scheme error.
    Url(String),
    /// WebSocket transport error.
    Ws(String),
    /// JSON (de)serialization error.
    Json(String),
    /// The server closed or misbehaved during the handshake.
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

/// A bot's cryptographic identity: a **Nostr** secp256k1/Schnorr keypair whose
/// x-only public key (64-char hex) is the durable handle a guild owner installs.
pub struct BotIdentity {
    secret: SecretKey,
    pubkey_hex: String,
}

impl BotIdentity {
    /// Generate a brand-new random identity. Persist [`secret_base58`] somewhere
    /// safe so the bot keeps the same pubkey across restarts (re-installs).
    ///
    /// [`secret_base58`]: BotIdentity::secret_base58
    pub fn generate() -> Self {
        loop {
            let mut secret = [0u8; 32];
            rand::rngs::OsRng.fill_bytes(&mut secret);
            if let Ok(id) = Self::try_from_secret_bytes(&secret) {
                return id;
            }
        }
    }

    /// Reconstruct an identity from its 32-byte secret seed. Panics if the
    /// bytes aren't a valid secp256k1 scalar (astronomically unlikely).
    pub fn from_secret_bytes(secret: &[u8; 32]) -> Self {
        Self::try_from_secret_bytes(secret).expect("valid secp256k1 secret")
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

    /// Reconstruct an identity from a base58-encoded 32-byte secret seed (the
    /// format produced by [`secret_base58`]).
    ///
    /// [`secret_base58`]: BotIdentity::secret_base58
    pub fn from_base58_secret(secret_b58: &str) -> Result<Self> {
        let bytes = bs58::decode(secret_b58.trim())
            .into_vec()
            .map_err(|e| BotError::Url(format!("secret not base58: {e}")))?;
        let arr: [u8; 32] = bytes
            .try_into()
            .map_err(|_| BotError::Url("secret must be 32 bytes".into()))?;
        Self::try_from_secret_bytes(&arr).map_err(|e| BotError::Url(format!("invalid secret: {e}")))
    }

    /// The bot's public key (Nostr x-only, hex). This is what a guild owner installs.
    pub fn pubkey(&self) -> &str {
        &self.pubkey_hex
    }

    /// The bot's secret seed (base58). Treat like a password; store it, don't
    /// share it.
    pub fn secret_base58(&self) -> String {
        bs58::encode(self.secret.secret_bytes()).into_string()
    }

    /// Schnorr-sign the `Identify` payload (`SHA256(nonce || pubkey || username)`)
    /// and return hex, matching the server's `auth::verify_identify`.
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

/// A connected bot. Drive it by looping on [`next_event`](Bot::next_event) and
/// calling the action helpers in response.
pub struct Bot {
    write: SplitSink<Stream, WsMessage>,
    read: SplitStream<Stream>,
    /// The bot's own identity as the server sees it (set after the handshake).
    pub user: User,
}

impl Bot {
    /// Connect to a gateway and complete the `Hello`/`Identify` handshake,
    /// self-declaring as a bot (scoped Ready, intent-filtered events).
    /// `url` may be a bare host (`localhost:9000`), an `http(s)://` URL, or a
    /// `ws(s)://` URL; the `/gateway` path is appended automatically.
    pub async fn connect(url: &str, identity: &BotIdentity, username: &str) -> Result<Bot> {
        Self::connect_declaring(url, identity, username, true).await
    }

    /// Connect as a regular (human) session — full Ready, unfiltered events,
    /// whole ClientMessage surface. Useful for integration tests and tooling
    /// that drive a user account through the same SDK.
    pub async fn connect_as_user(
        url: &str,
        identity: &BotIdentity,
        username: &str,
    ) -> Result<Bot> {
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

        // Wait for the server's Hello so we know which nonce to sign.
        let nonce = loop {
            let frame = read
                .next()
                .await
                .ok_or_else(|| BotError::Handshake("server closed before Hello".into()))?
                .map_err(|e| BotError::Ws(e.to_string()))?;
            match frame {
                WsMessage::Text(t) => {
                    let parsed: ServerMessage = serde_json::from_str(&t)
                        .map_err(|e| BotError::Json(e.to_string()))?;
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

        let signature = identity.sign_identify(&nonce, username);
        // For bot connections this self-declaration is what triggers the
        // server's scoped Ready + intent filtering; installs alone never
        // bot-gate an identity.
        let identify = ClientMessage::Identify {
            username: username.to_string(),
            pubkey: identity.pubkey().to_string(),
            signature,
            bot,
        };
        let mut bot = Bot {
            write,
            read,
            user: User {
                pubkey: identity.pubkey().to_string(),
                username: username.to_string(),
            },
        };
        bot.send(&identify).await?;
        Ok(bot)
    }

    /// Read the next event from the gateway. Returns `None` when the connection
    /// closes. The first event after connecting is typically `Ready`.
    pub async fn next_event(&mut self) -> Option<ServerMessage> {
        while let Some(frame) = self.read.next().await {
            let frame = frame.ok()?;
            match frame {
                WsMessage::Text(t) => match serde_json::from_str::<ServerMessage>(&t) {
                    Ok(msg) => return Some(msg),
                    // Skip frames we can't parse rather than killing the loop.
                    Err(_) => continue,
                },
                WsMessage::Close(_) => return None,
                _ => continue,
            }
        }
        None
    }

    /// Post a message to a text channel. Requires the `SendMessages` permission
    /// in that channel's guild.
    pub async fn send_message(&mut self, channel_id: Id, content: &str) -> Result<()> {
        self.send(&ClientMessage::SendMessage {
            channel_id,
            content: content.to_string(),
            image: None,
            reply_to: None,
        })
        .await
    }

    /// Post a message as a reply to `reply_to`, which must be in the same
    /// channel. The server builds the quoted snapshot from its own copy of that
    /// message; an id it can't resolve there sends as an ordinary message.
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

    /// Toggle an emoji reaction on a message. Requires the `AddReactions`
    /// permission.
    pub async fn react(&mut self, channel_id: Id, message_id: Id, emoji: &str) -> Result<()> {
        self.send(&ClientMessage::React {
            channel_id,
            message_id,
            emoji: emoji.to_string(),
        })
        .await
    }

    /// Request recent history for a channel (delivered as a `MessageHistory`
    /// event). Requires the `ReadMessageHistory` permission.
    pub async fn fetch_messages(&mut self, channel_id: Id, limit: u32) -> Result<()> {
        self.send(&ClientMessage::FetchMessages { channel_id, limit, before_ms: None })
            .await
    }

    /// Send an arbitrary client frame. Most bots won't need this directly.
    pub async fn send(&mut self, msg: &ClientMessage) -> Result<()> {
        let json = serde_json::to_string(msg).map_err(|e| BotError::Json(e.to_string()))?;
        self.write
            .send(WsMessage::Text(json.into()))
            .await
            .map_err(|e| BotError::Ws(e.to_string()))
    }
}

/// Accept a bare host, `http(s)://`, or `ws(s)://` and return the `/gateway`
/// WebSocket URL (mirrors the desktop client's `normalize_url`).
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
