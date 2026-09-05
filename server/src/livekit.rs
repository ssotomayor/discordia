use std::path::Path;

use livekit_api::access_token::{AccessToken, VideoGrants};

use crate::livekit_bundle;
use crate::protocol::Id;

#[derive(Debug, Clone)]
pub struct MintRequest {
    pub room: String,
    pub identity: String,
    pub name: String,
    pub can_publish: bool,
}

pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

pub trait VoiceTokenMinter: Send + Sync {
    fn mint<'a>(&'a self, req: MintRequest) -> BoxFuture<'a, Result<String, String>>;
}

#[derive(Clone)]
pub struct LiveKitConfig {
    pub explicit_url: Option<String>,
    pub port: u16,
    pub lan_host: Option<String>,
    pub public_host: Option<String>,
    pub api_key: String,
    pub api_secret: String,
    pub minter: Option<std::sync::Arc<dyn VoiceTokenMinter>>,
}

impl LiveKitConfig {
    pub fn from_env(data_dir: &Path) -> Self {
        let env = |name| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        let (api_key, api_secret) = match (env("LIVEKIT_API_KEY"), env("LIVEKIT_API_SECRET")) {
            (Some(key), Some(secret)) => (key, secret),
            _ => {
                let c = livekit_bundle::credentials_or_ephemeral(data_dir);
                (c.key, c.secret)
            }
        };
        Self {
            explicit_url: std::env::var("LIVEKIT_URL").ok(),
            port: std::env::var("LIVEKIT_PORT")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(7880),
            api_key,
            api_secret,
            minter: None,
            lan_host: None,
            public_host: None,
        }
    }

    /// Over QUIC the `Host` header is a fixed loopback placeholder, so the
    /// peer's own address says which side of the NAT it stands on: a private
    /// one gets the LAN address, a public one the mapped address.
    pub fn url_for_client(
        &self,
        client_host: Option<&str>,
        peer: Option<std::net::IpAddr>,
    ) -> String {
        if let Some(url) = &self.explicit_url {
            return url.clone();
        }
        let host = client_host.map(host_without_port).unwrap_or("127.0.0.1");
        let host = if is_loopback(host) {
            let prefer_lan =
                peer.is_some_and(|ip| !ip.is_loopback() && crate::protocol::is_private_ip(ip));
            let (first, second) = if prefer_lan {
                (&self.lan_host, &self.public_host)
            } else {
                (&self.public_host, &self.lan_host)
            };
            first.as_deref().or(second.as_deref()).unwrap_or(host)
        } else {
            host
        };
        format!("ws://{host}:{}", self.port)
    }
}

fn is_loopback(host: &str) -> bool {
    host == "localhost" || host == "127.0.0.1" || host == "[::1]" || host == "::1"
}

fn host_without_port(host: &str) -> &str {
    if host.starts_with('[')
        && let Some(end) = host.find(']')
    {
        return &host[..=end];
    }
    host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host)
}

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
                can_publish: true,
            })
            .await
        }
        None => mint_token(cfg, user_pubkey, username, channel_id),
    }
}

pub fn screen_audio_identity(user_pubkey: &str) -> String {
    format!("{user_pubkey}#audio")
}

pub fn screen_video_identity(user_pubkey: &str) -> String {
    format!("{user_pubkey}#video")
}

pub async fn screen_token_as(
    cfg: &LiveKitConfig,
    identity: &str,
    username: &str,
    channel_id: Id,
    can_publish: bool,
) -> Result<String, String> {
    match &cfg.minter {
        Some(m) => {
            m.mint(MintRequest {
                room: screen_room_name(channel_id),
                identity: identity.to_string(),
                name: username.to_string(),
                can_publish,
            })
            .await
        }
        None => mint_screen_token(cfg, identity, username, channel_id, can_publish),
    }
}

pub fn room_name(channel_id: Id) -> String {
    format!("voice-{channel_id}")
}

pub fn screen_room_name(channel_id: Id) -> String {
    format!("screen-{channel_id}")
}

pub fn mint_screen_token(
    cfg: &LiveKitConfig,
    identity: &str,
    username: &str,
    channel_id: Id,
    can_publish: bool,
) -> Result<String, String> {
    let room = screen_room_name(channel_id);
    AccessToken::with_api_key(&cfg.api_key, &cfg.api_secret)
        .with_identity(identity)
        .with_name(username)
        .with_grants(VideoGrants {
            room_join: true,
            room,
            can_publish,
            can_subscribe: true,
            can_publish_data: can_publish,
            ..Default::default()
        })
        .to_jwt()
        .map_err(|e| format!("livekit screen token: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_grants_follow_the_connection() {
        use livekit_api::access_token::TokenVerifier;

        let cfg = LiveKitConfig {
            explicit_url: None,
            port: 7880,
            api_key: "devkey".into(),
            api_secret: "secret-long-enough-for-hs256-signing".into(),
            minter: None,
            lan_host: None,
            public_host: None,
        };
        let channel = Id::new_v4();
        let verifier = TokenVerifier::with_api_key(&cfg.api_key, &cfg.api_secret);
        let pubkey = "a".repeat(64);

        let grants = |identity: &str, can_publish: bool| {
            let jwt = mint_screen_token(&cfg, identity, "name (screen)", channel, can_publish)
                .expect("mint");
            let claims = verifier.verify(&jwt).expect("verify");
            assert_eq!(claims.sub, identity);
            assert_eq!(claims.video.room, screen_room_name(channel));
            assert!(claims.video.room_join);
            claims.video
        };

        let webview = grants(&pubkey, true);
        assert!(webview.can_publish);
        assert!(webview.can_subscribe);

        let audio = grants(&screen_audio_identity(&pubkey), false);
        assert!(!audio.can_publish);
        assert!(!audio.can_publish_data);
        assert!(audio.can_subscribe);

        let video = grants(&screen_video_identity(&pubkey), true);
        assert!(video.can_publish);
    }

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
            public_host: None,
        };
        assert_eq!(
            cfg.url_for_client(Some("192.168.0.5:9000"), None),
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
            public_host: None,
        };
        assert_eq!(
            cfg.url_for_client(Some("192.168.0.5:9000"), None),
            "ws://192.168.0.5:7880"
        );
        assert_eq!(cfg.url_for_client(None, None), "ws://127.0.0.1:7880");
    }

    #[test]
    fn loopback_client_gets_lan_host() {
        let cfg = LiveKitConfig {
            explicit_url: None,
            port: 7880,
            api_key: "".into(),
            api_secret: "".into(),
            minter: None,
            lan_host: Some("192.168.0.61".into()),
            public_host: None,
        };
        for h in ["127.0.0.1:9000", "localhost:9000", "[::1]:9000"] {
            assert_eq!(cfg.url_for_client(Some(h), None), "ws://192.168.0.61:7880");
        }
        assert_eq!(
            cfg.url_for_client(Some("192.168.0.99:9000"), None),
            "ws://192.168.0.99:7880"
        );
    }

    #[test]
    fn public_host_outranks_lan_host() {
        let cfg = LiveKitConfig {
            explicit_url: None,
            port: 7880,
            api_key: "".into(),
            api_secret: "".into(),
            minter: None,
            lan_host: Some("192.168.0.61".into()),
            public_host: Some("203.0.113.5".into()),
        };
        assert_eq!(
            cfg.url_for_client(Some("127.0.0.1:9000"), None),
            "ws://203.0.113.5:7880"
        );
        assert_eq!(
            cfg.url_for_client(Some("192.168.0.99:9000"), None),
            "ws://192.168.0.99:7880"
        );
    }

    #[test]
    fn a_quic_peer_is_placed_by_its_own_address() {
        let cfg = LiveKitConfig {
            explicit_url: None,
            port: 7880,
            api_key: "".into(),
            api_secret: "".into(),
            minter: None,
            lan_host: Some("192.168.0.61".into()),
            public_host: Some("203.0.113.5".into()),
        };
        let quic_host = Some("127.0.0.1");
        assert_eq!(
            cfg.url_for_client(quic_host, Some("192.168.0.99".parse().unwrap())),
            "ws://192.168.0.61:7880",
            "a friend on the LAN gets the LAN address"
        );
        assert_eq!(
            cfg.url_for_client(quic_host, Some("198.51.100.9".parse().unwrap())),
            "ws://203.0.113.5:7880",
            "a friend across the internet gets the mapped address"
        );
        assert_eq!(
            cfg.url_for_client(quic_host, Some("127.0.0.1".parse().unwrap())),
            "ws://203.0.113.5:7880",
            "our own client behaves as before"
        );
    }
}
