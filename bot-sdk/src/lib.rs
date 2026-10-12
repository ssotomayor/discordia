use std::pin::Pin;

use futures_util::{Sink, SinkExt, Stream, StreamExt};
use rand::RngCore;
use secp256k1::{Keypair, Message, Secp256k1, SecretKey};
use sha2::{Digest, Sha256};
use tokio_tungstenite::tungstenite::{Error as WsError, Message as WsMessage};

pub use dioxusfun_protocol as protocol;
use protocol::{AppManifest, BotCommand, ClientMessage, Id, Invocation, ServerMessage, User};

mod quic;

type FrameSink = Pin<Box<dyn Sink<WsMessage, Error = WsError> + Send + Sync>>;
type FrameStream =
    Pin<Box<dyn Stream<Item = std::result::Result<WsMessage, WsError>> + Send + Sync>>;

#[derive(Debug)]
pub enum BotError {
    Url(String),
    Ws(String),
    Json(String),
    Handshake(String),
    Quic(String),
}

impl std::fmt::Display for BotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BotError::Url(e) => write!(f, "url error: {e}"),
            BotError::Ws(e) => write!(f, "websocket error: {e}"),
            BotError::Json(e) => write!(f, "json error: {e}"),
            BotError::Handshake(e) => write!(f, "handshake error: {e}"),
            BotError::Quic(e) => write!(f, "quic error: {e}"),
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

    pub fn sign_identify(&self, nonce: &str, origin: &str, username: &str) -> String {
        let secp = Secp256k1::new();
        let keypair = Keypair::from_secret_key(&secp, &self.secret);
        let msg = dioxusfun_protocol::identify_payload(nonce, origin, &self.pubkey_hex, username);
        let digest: [u8; 32] = Sha256::digest(&msg).into();
        let m = Message::from_digest(digest);
        hex::encode(secp.sign_schnorr_no_aux_rand(&m, &keypair).serialize())
    }

    /// The signed NIP-89 event a guild installs from. Publish it to relays, or
    /// hand it to a host with `Bot::register_manifest`.
    pub fn sign_manifest(&self, manifest: &AppManifest) -> String {
        let content = serde_json::to_string(manifest).expect("manifest serializes");
        let tags = vec![vec![
            "d".to_string(),
            dioxusfun_protocol::APP_MANIFEST_D.to_string(),
        ]];
        self.sign_event(
            dioxusfun_protocol::APP_MANIFEST_KIND,
            tags,
            &content,
            now_secs(),
        )
    }

    /// A curator's list of app keys guilds may follow (NIP-51 kind 30000).
    pub fn sign_blocklist(&self, app_keys: &[&str], created_at: i64) -> String {
        let mut tags = vec![vec![
            "d".to_string(),
            dioxusfun_protocol::APP_BLOCKLIST_D.to_string(),
        ]];
        tags.extend(
            app_keys
                .iter()
                .map(|k| vec!["p".to_string(), k.to_string()]),
        );
        self.sign_event(dioxusfun_protocol::APP_BLOCKLIST_KIND, tags, "", created_at)
    }

    /// Any NIP-01 event signed by this key, as JSON.
    pub fn sign_event(
        &self,
        kind: u32,
        tags: Vec<Vec<String>>,
        content: &str,
        created_at: i64,
    ) -> String {
        let canonical =
            serde_json::json!([0, self.pubkey_hex, created_at, kind, tags, content]).to_string();
        let id: [u8; 32] = Sha256::digest(canonical.as_bytes()).into();
        let secp = Secp256k1::new();
        let keypair = Keypair::from_secret_key(&secp, &self.secret);
        let sig = secp.sign_schnorr_no_aux_rand(&Message::from_digest(id), &keypair);
        serde_json::json!({
            "id": hex::encode(id),
            "pubkey": self.pubkey_hex,
            "created_at": created_at,
            "kind": kind,
            "tags": tags,
            "content": content,
            "sig": hex::encode(sig.serialize()),
        })
        .to_string()
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub struct Bot {
    write: FrameSink,
    read: FrameStream,
    _quic: Option<quic::Link>,
    pub user: User,
}

/// The same WebSocket either way: over TCP, or over one QUIC stream to the
/// host's key, where the origin signed is `quic:<key>` (trap 12).
async fn open_socket(url: &str) -> Result<(FrameSink, FrameStream, Option<quic::Link>, String)> {
    if let Some(share) = protocol::parse_quic_share(url) {
        let (io, link) = quic::dial(&share).await?;
        let (ws, _) = tokio_tungstenite::client_async(quic::HANDSHAKE_URL, io)
            .await
            .map_err(|e| BotError::Ws(format!("websocket over quic: {e}")))?;
        let (write, read) = ws.split();
        return Ok((
            Box::pin(write),
            Box::pin(read),
            Some(link),
            protocol::quic_origin(&share.key),
        ));
    }
    let ws_url = normalize_gateway_url(url)?;
    let origin = protocol::dial_origin(&ws_url)
        .ok_or_else(|| BotError::Url("server URL has no host".into()))?;
    let (ws, _) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .map_err(|e| BotError::Ws(format!("connect failed: {e}")))?;
    let (write, read) = ws.split();
    Ok((Box::pin(write), Box::pin(read), None, origin))
}

impl Bot {
    /// `url` is `ws://`/`wss://`, or a host's `quic://key@addrs` share string: off
    /// loopback a gateway answers only QUIC (trap 15), unless a TLS proxy fronts it.
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
        let (write, mut read, link, origin) = open_socket(url).await?;

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
        let signature = identity.sign_identify(&nonce, &origin, &username);
        let identify = ClientMessage::Identify {
            username: username.clone(),
            pubkey: identity.pubkey().to_string(),
            signature,
            origin,
            bot,
            client_version: concat!("bot-sdk/", env!("CARGO_PKG_VERSION")).to_string(),
        };
        let mut bot = Bot {
            write,
            read,
            _quic: link,
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
            image_name: None,
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
            image_name: None,
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

    /// Replaces every command declared before. Call it after `Ready`, on each
    /// connect: the server keeps them only while it runs.
    pub async fn register_manifest(
        &mut self,
        identity: &BotIdentity,
        manifest: &AppManifest,
    ) -> Result<()> {
        self.send(&ClientMessage::RegisterManifest {
            event: identity.sign_manifest(manifest),
        })
        .await
    }

    /// Speaks in an instance of this bot's own activity, as its backend. `to`
    /// reaches one participant; `None` reaches them all.
    pub async fn activity_send(
        &mut self,
        instance_id: Id,
        to: Option<&str>,
        data: &str,
    ) -> Result<()> {
        self.send(&ClientMessage::ActivitySend {
            instance_id,
            to: to.map(str::to_string),
            data: data.to_string(),
        })
        .await
    }

    pub async fn register_commands(&mut self, commands: Vec<BotCommand>) -> Result<()> {
        self.send(&ClientMessage::RegisterCommands { commands })
            .await
    }

    /// A reply only the person who pressed sees. Once per invocation.
    pub async fn respond(&mut self, invocation: &Invocation, content: &str) -> Result<()> {
        self.send(&ClientMessage::RespondToCommand {
            invocation_id: invocation.id,
            content: content.to_string(),
            data: None,
        })
        .await
    }

    /// For an app that called: `data` is JSON text it reads instead of prose.
    pub async fn respond_with_data(
        &mut self,
        invocation: &Invocation,
        content: &str,
        data: &str,
    ) -> Result<()> {
        self.send(&ClientMessage::RespondToCommand {
            invocation_id: invocation.id,
            content: content.to_string(),
            data: Some(data.to_string()),
        })
        .await
    }

    /// Calls another installed app's command. `parent` is the invocation this
    /// bot is handling, if any; the guild must allow the pair.
    pub async fn invoke(
        &mut self,
        parent: Option<&Invocation>,
        guild_id: Id,
        channel_id: Id,
        bot_pubkey: &str,
        command: &str,
        args: Vec<protocol::CommandArg>,
    ) -> Result<()> {
        self.send(&ClientMessage::InvokeCommand {
            guild_id,
            channel_id,
            bot_pubkey: bot_pubkey.to_string(),
            command: command.to_string(),
            args,
            parent: parent.map(|p| p.id),
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
