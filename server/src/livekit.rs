//! LiveKit access-token minting and URL resolution.

use livekit_api::access_token::{AccessToken, VideoGrants};

use crate::protocol::Id;

/// A voice-token request. Rooms are named by the gateway; a delegated minter
/// may namespace them further (see the rendezvous).
#[derive(Debug, Clone)]
pub struct MintRequest {
    pub room: String,
    pub identity: String,
    pub name: String,
}

pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// Something that can sign LiveKit tokens on our behalf.
///
/// When a self-hosting gateway uses a *shared* SFU it doesn't own (e.g. the one
/// a public rendezvous operates), it must not hold that SFU's signing secret —
/// otherwise every host on that relay could mint tokens for every other host's
/// rooms. Instead the gateway asks the operator to mint, and the operator scopes
/// the room to this host. See `client::rendezvous::RendezvousMinter`.
pub trait VoiceTokenMinter: Send + Sync {
    fn mint<'a>(&'a self, req: MintRequest) -> BoxFuture<'a, Result<String, String>>;
}

#[derive(Clone)]
pub struct LiveKitConfig {
    /// Explicit URL that overrides per-connection resolution. Set this when
    /// LiveKit lives at a known address (LiveKit Cloud, a separate host,
    /// behind a reverse proxy, etc.).
    pub explicit_url: Option<String>,
    /// LiveKit WebSocket port. Used when deriving the URL from the host the
    /// client used to dial the gateway (self-host / same-machine case).
    pub port: u16,
    /// This machine's LAN address, when self-hosting. Used INSTEAD of a
    /// loopback-derived host: a client whose `Host` header is 127.0.0.1 is
    /// either us (LAN IP works fine too) or — critically — a friend arriving
    /// through the rendezvous proxy, which dials our gateway on loopback.
    /// Handing that friend `ws://127.0.0.1:7880` points them at their OWN
    /// machine ("Connection refused"); the LAN address is at least reachable
    /// from the same network.
    pub lan_host: Option<String>,
    pub api_key: String,
    pub api_secret: String,
    /// When set, tokens are minted by this delegate instead of signed locally
    /// (we don't hold the shared SFU's secret).
    pub minter: Option<std::sync::Arc<dyn VoiceTokenMinter>>,
}

impl LiveKitConfig {
    pub fn from_env() -> Self {
        Self {
            explicit_url: std::env::var("LIVEKIT_URL").ok(),
            port: std::env::var("LIVEKIT_PORT")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(7880),
            api_key: std::env::var("LIVEKIT_API_KEY").unwrap_or_else(|_| "devkey".into()),
            api_secret: std::env::var("LIVEKIT_API_SECRET")
                .unwrap_or_else(|_| "secret-must-be-at-least-32-chars-long".into()),
            minter: None,
            lan_host: None,
        }
    }

    /// Resolve the LiveKit URL to hand back to a specific client.
    ///
    /// - If an explicit URL was configured (e.g. via `LIVEKIT_URL`), return it.
    /// - Otherwise, build `ws://{client connection host}:{port}` so the
    ///   client dials LiveKit on the same host they used to reach the
    ///   gateway. This makes a self-host operator's `192.168.0.48:9000`
    ///   gateway naturally pair with `192.168.0.48:7880` for LiveKit.
    pub fn url_for_client(&self, client_host: Option<&str>) -> String {
        if let Some(url) = &self.explicit_url {
            return url.clone();
        }
        let host = client_host.map(host_without_port).unwrap_or("127.0.0.1");
        // Loopback means "this connection reached us locally" — true for us,
        // and also for anyone proxied in by the rendezvous. Prefer our LAN
        // address so proxied friends get something they can actually dial.
        let host = match (&self.lan_host, is_loopback(host)) {
            (Some(lan), true) => lan.as_str(),
            _ => host,
        };
        format!("ws://{host}:{}", self.port)
    }
}

/// True for hosts that only resolve on the machine itself.
fn is_loopback(host: &str) -> bool {
    host == "localhost" || host == "127.0.0.1" || host == "[::1]" || host == "::1"
}

/// Strip the port from a HTTP `Host` header, handling IPv6 in brackets.
fn host_without_port(host: &str) -> &str {
    if host.starts_with('[') {
        if let Some(end) = host.find(']') {
            return &host[..=end];
        }
    }
    host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host)
}

/// Build a LiveKit JWT scoped to the given voice channel room. Identity is
/// the user's Ed25519 pubkey so LiveKit's participant identifiers match
/// dioxusfun's universal user id.
pub fn mint_token(
    cfg: &LiveKitConfig,
    user_pubkey: &str,
    username: &str,
    channel_id: Id,
) -> Result<String, String> {
    let room = room_name(channel_id);
    AccessToken::with_api_key(&cfg.api_key, &cfg.api_secret)
        .with_identity(user_pubkey)
        .with_name(username)
        .with_grants(VideoGrants {
            room_join: true,
            room,
            can_publish: true,
            can_subscribe: true,
            can_publish_data: true,
            ..Default::default()
        })
        .to_jwt()
        .map_err(|e| format!("livekit token: {e}"))
}

/// Mint a voice token, delegating to `cfg.minter` when one is configured.
pub async fn voice_token(
    cfg: &LiveKitConfig,
    user_pubkey: &str,
    username: &str,
    channel_id: Id,
) -> Result<String, String> {
    match &cfg.minter {
        Some(m) => {
            m.mint(MintRequest {
                room: room_name(channel_id),
                identity: user_pubkey.to_string(),
                name: username.to_string(),
            })
            .await
        }
        None => mint_token(cfg, user_pubkey, username, channel_id),
    }
}

/// Mint a screen-share token, delegating like `voice_token`.
pub async fn screen_token(
    cfg: &LiveKitConfig,
    user_pubkey: &str,
    username: &str,
    channel_id: Id,
) -> Result<String, String> {
    match &cfg.minter {
        Some(m) => {
            m.mint(MintRequest {
                room: screen_room_name(channel_id),
                identity: user_pubkey.to_string(),
                name: username.to_string(),
            })
            .await
        }
        None => mint_screen_token(cfg, user_pubkey, username, channel_id),
    }
}

pub fn room_name(channel_id: Id) -> String {
    format!("voice-{channel_id}")
}

/// Screen sharing rides in a SEPARATE room from voice so native-audio clients
/// (in `voice-…`) never auto-subscribe to — and waste bandwidth decoding — the
/// screen video, which only the webview JS clients render.
pub fn screen_room_name(channel_id: Id) -> String {
    format!("screen-{channel_id}")
}

/// Mint a token for the webview JS client to join the screen-share room. The
/// identity stays the user's pubkey (fine — it's a different room from the
/// native-audio one, and identities only need to be unique per room).
pub fn mint_screen_token(
    cfg: &LiveKitConfig,
    user_pubkey: &str,
    username: &str,
    channel_id: Id,
) -> Result<String, String> {
    let room = screen_room_name(channel_id);
    AccessToken::with_api_key(&cfg.api_key, &cfg.api_secret)
        .with_identity(user_pubkey)
        .with_name(username)
        .with_grants(VideoGrants {
            room_join: true,
            room,
            can_publish: true,
            can_subscribe: true,
            can_publish_data: true,
            ..Default::default()
        })
        .to_jwt()
        .map_err(|e| format!("livekit screen token: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_ipv4_port() {
        assert_eq!(host_without_port("192.168.1.10:9000"), "192.168.1.10");
        assert_eq!(host_without_port("192.168.1.10"), "192.168.1.10");
        assert_eq!(host_without_port("localhost:9000"), "localhost");
    }

    #[test]
    fn strips_ipv6_port() {
        assert_eq!(host_without_port("[::1]:9000"), "[::1]");
        assert_eq!(host_without_port("[::1]"), "[::1]");
        assert_eq!(host_without_port("[2001:db8::1]:9000"), "[2001:db8::1]");
    }

    #[test]
    fn explicit_url_wins() {
        let cfg = LiveKitConfig {
            explicit_url: Some("wss://my.livekit.cloud".into()),
            port: 7880,
            api_key: "".into(),
            api_secret: "".into(),
            minter: None,
            lan_host: None,
        };
        assert_eq!(
            cfg.url_for_client(Some("192.168.0.5:9000")),
            "wss://my.livekit.cloud"
        );
    }

    #[test]
    fn derives_from_client_host() {
        let cfg = LiveKitConfig {
            explicit_url: None,
            port: 7880,
            api_key: "".into(),
            api_secret: "".into(),
            minter: None,
            lan_host: None,
        };
        assert_eq!(
            cfg.url_for_client(Some("192.168.0.5:9000")),
            "ws://192.168.0.5:7880"
        );
        assert_eq!(cfg.url_for_client(None), "ws://127.0.0.1:7880");
    }

    /// A rendezvous-proxied friend reaches the gateway on loopback; without
    /// the LAN substitution they'd be told to dial LiveKit on their own box.
    #[test]
    fn loopback_client_gets_lan_host() {
        let cfg = LiveKitConfig {
            explicit_url: None,
            port: 7880,
            api_key: "".into(),
            api_secret: "".into(),
            minter: None,
            lan_host: Some("192.168.0.61".into()),
        };
        for h in ["127.0.0.1:9000", "localhost:9000", "[::1]:9000"] {
            assert_eq!(cfg.url_for_client(Some(h)), "ws://192.168.0.61:7880");
        }
        // A real LAN/remote host header is still honoured verbatim.
        assert_eq!(
            cfg.url_for_client(Some("192.168.0.99:9000")),
            "ws://192.168.0.99:7880"
        );
    }
}
