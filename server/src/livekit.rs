use std::path::Path;

use livekit_api::access_token::{AccessToken, VideoGrants};

use crate::livekit_bundle;
use crate::protocol::{IceServer, Id};

#[derive(Debug, Clone)]
pub struct MintRequest {
    pub room: String,
    pub identity: String,
    pub name: String,
    pub can_publish: bool,
}

pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

pub struct VoiceFallback {
    pub url: String,
    pub minter: std::sync::Arc<dyn VoiceTokenMinter>,
    selection: std::sync::atomic::AtomicU8,
}

impl VoiceFallback {
    pub fn new(url: String, minter: std::sync::Arc<dyn VoiceTokenMinter>) -> Self {
        Self {
            url,
            minter,
            selection: std::sync::atomic::AtomicU8::new(0),
        }
    }

    pub fn confirmed_local(&self) {
        let _ = self.selection.compare_exchange(
            0,
            1,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        );
    }

    pub fn use_shared(&self) -> bool {
        // Once a remote caller verified local media, a later caller cannot move the session.
        self.selection
            .compare_exchange(
                0,
                2,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_ok()
    }

    fn shared(&self) -> bool {
        self.selection.load(std::sync::atomic::Ordering::Acquire) == 2
    }
}

pub trait VoiceTokenMinter: Send + Sync {
    fn mint<'a>(&'a self, req: MintRequest) -> BoxFuture<'a, Result<String, String>>;

    fn evict<'a>(&'a self, _channel_id: Id, _pubkey: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Err("this token minter does not support participant eviction".into()) })
    }
}

#[derive(Clone)]
pub struct LiveKitConfig {
    pub explicit_url: Option<String>,
    pub port: u16,
    pub lan_host: Option<String>,
    pub public_host: Option<String>,
    pub alternate_hosts: Vec<std::net::IpAddr>,
    pub fallback: Option<std::sync::Arc<VoiceFallback>>,
    pub api_key: String,
    pub api_secret: String,
    pub minter: Option<std::sync::Arc<dyn VoiceTokenMinter>>,
    /// Handed to every client with its voice tokens. Shared because a
    /// self-host swaps in renewed relay credentials while the gateway runs.
    pub ice_servers: SharedIceServers,
}

pub type SharedIceServers = std::sync::Arc<std::sync::RwLock<Vec<IceServer>>>;

pub fn shared_ice_servers(servers: Vec<IceServer>) -> SharedIceServers {
    std::sync::Arc::new(std::sync::RwLock::new(servers))
}

impl LiveKitConfig {
    pub fn ice_servers(&self) -> Vec<IceServer> {
        self.ice_servers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn from_env(data_dir: &Path) -> Self {
        let env = |name| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        let ice_servers = env("DIOXUSFUN_ICE_SERVERS")
            .map(|json| {
                serde_json::from_str::<Vec<IceServer>>(&json).unwrap_or_else(|e| {
                    panic!("DIOXUSFUN_ICE_SERVERS is not a JSON list of ICE servers: {e}")
                })
            })
            .unwrap_or_default();
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
            alternate_hosts: Vec::new(),
            fallback: None,
            ice_servers: shared_ice_servers(ice_servers),
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
        if !self.alternate_hosts.is_empty() {
            return self.urls_for_client(client_host, peer).remove(0);
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

    pub fn urls_for_client(
        &self,
        client_host: Option<&str>,
        peer: Option<std::net::IpAddr>,
    ) -> Vec<String> {
        if self.alternate_hosts.is_empty() || self.explicit_url.is_some() {
            return vec![self.url_for_client(client_host, peer)];
        }
        let mut hosts = Vec::new();
        if peer.is_some_and(|ip| ip.is_loopback()) {
            hosts.push("127.0.0.1".to_string());
        }
        if peer.is_some_and(crate::protocol::is_private_ip)
            && let Some(lan) = &self.lan_host
        {
            hosts.push(lan.clone());
        }
        let prefer_v6 = peer.is_some_and(|ip| ip.is_ipv6());
        let (mut preferred, mut other): (
            std::collections::VecDeque<_>,
            std::collections::VecDeque<_>,
        ) = self
            .alternate_hosts
            .iter()
            .copied()
            .partition(|ip| ip.is_ipv6() == prefer_v6);
        let mut ips = Vec::new();
        while !preferred.is_empty() || !other.is_empty() {
            ips.extend(preferred.pop_front());
            ips.extend(other.pop_front());
        }
        hosts.extend(ips.into_iter().map(|ip| match ip {
            std::net::IpAddr::V6(ip) => format!("[{ip}]"),
            std::net::IpAddr::V4(ip) => ip.to_string(),
        }));
        if let Some(public) = &self.public_host {
            hosts.push(public.clone());
        }
        let mut urls = Vec::new();
        for host in hosts {
            let url = format!("ws://{host}:{}", self.port);
            if !urls.contains(&url) {
                urls.push(url);
            }
        }
        urls
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

pub fn screen_viewer_identity(user_pubkey: &str) -> String {
    format!("{user_pubkey}#viewer")
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

impl LiveKitConfig {
    pub fn route(&self) -> (Self, u32) {
        let mut cfg = self.clone();
        cfg.fallback = None;
        if let Some(fallback) = &self.fallback
            && fallback.shared()
        {
            cfg.explicit_url = Some(fallback.url.clone());
            cfg.minter = Some(fallback.minter.clone());
            cfg.alternate_hosts.clear();
            (cfg, 1)
        } else {
            (cfg, 0)
        }
    }

    /// `None` when a rendezvous mints the tokens: its SFU, its keys.
    fn admin_url(&self) -> Option<String> {
        if self.minter.is_some() {
            return None;
        }
        Some(match &self.explicit_url {
            Some(url) => url
                .replacen("wss://", "https://", 1)
                .replacen("ws://", "http://", 1),
            None => format!("http://127.0.0.1:{}", self.port),
        })
    }
}

/// Every identity one person can hold in a channel's two rooms (trap 10). One
/// that is not there is the usual answer, not a failure.
pub async fn evict(cfg: &LiveKitConfig, channel_id: Id, user_pubkey: &str) {
    let (cfg, _) = cfg.route();
    if let Some(minter) = &cfg.minter {
        if let Err(error) = minter.evict(channel_id, user_pubkey).await {
            tracing::warn!(%channel_id, %error, "delegated SFU eviction failed");
        }
        return;
    }
    let Some(url) = cfg.admin_url() else {
        tracing::debug!(%channel_id, "no SFU keys here; the app is trusted to leave");
        return;
    };
    let client =
        livekit_api::services::room::RoomClient::with_api_key(&url, &cfg.api_key, &cfg.api_secret);
    let screen = screen_room_name(channel_id);
    let seats = [
        (room_name(channel_id), user_pubkey.to_string()),
        (screen.clone(), user_pubkey.to_string()),
        (screen.clone(), screen_audio_identity(user_pubkey)),
        (screen.clone(), screen_video_identity(user_pubkey)),
        (screen, screen_viewer_identity(user_pubkey)),
    ];
    for (room, identity) in seats {
        match client.remove_participant(&room, &identity).await {
            Ok(()) => tracing::info!(%room, %identity, "evicted from the SFU"),
            Err(e) => tracing::debug!(%room, %identity, error = %e, "nothing to evict"),
        }
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

    struct FakeMinter;
    impl VoiceTokenMinter for FakeMinter {
        fn mint<'a>(&'a self, req: MintRequest) -> BoxFuture<'a, Result<String, String>> {
            Box::pin(async move { Ok(format!("delegated-{}", req.identity)) })
        }
    }

    fn direct_routes() -> LiveKitConfig {
        LiveKitConfig {
            explicit_url: None,
            port: 7880,
            lan_host: Some("192.168.0.16".into()),
            public_host: Some("203.0.113.5".into()),
            ice_servers: Default::default(),
            alternate_hosts: vec![
                "203.0.113.5".parse().unwrap(),
                "2800:810::123".parse().unwrap(),
            ],
            fallback: Some(std::sync::Arc::new(VoiceFallback::new(
                "wss://last-option".into(),
                std::sync::Arc::new(FakeMinter),
            ))),
            api_key: "key".into(),
            api_secret: "secret".into(),
            minter: None,
        }
    }

    #[test]
    fn the_host_uses_loopback_and_remote_callers_try_both_ip_families() {
        let cfg = direct_routes();
        let local = cfg.urls_for_client(Some("127.0.0.1"), Some("127.0.0.1".parse().unwrap()));
        assert_eq!(local[0], "ws://127.0.0.1:7880");
        let v6 = cfg.urls_for_client(Some("127.0.0.1"), Some("2800:40::9".parse().unwrap()));
        assert_eq!(v6, ["ws://[2800:810::123]:7880", "ws://203.0.113.5:7880"]);
        let v4 = cfg.urls_for_client(Some("127.0.0.1"), Some("198.51.100.9".parse().unwrap()));
        assert_eq!(v4, ["ws://203.0.113.5:7880", "ws://[2800:810::123]:7880"]);
        let relay = cfg.urls_for_client(Some("127.0.0.1"), None);
        assert!(
            !relay
                .iter()
                .any(|url| url.contains("127.0.0.1") || url.contains("192.168"))
        );
    }

    #[test]
    fn many_ipv6_addresses_do_not_push_ipv4_to_the_end() {
        let mut cfg = direct_routes();
        cfg.alternate_hosts = vec![
            "2800:810::1".parse().unwrap(),
            "2800:810::2".parse().unwrap(),
            "203.0.113.5".parse().unwrap(),
            "203.0.113.6".parse().unwrap(),
        ];
        let urls = cfg.urls_for_client(None, Some("2800:40::9".parse().unwrap()));
        assert_eq!(
            urls,
            [
                "ws://[2800:810::1]:7880",
                "ws://203.0.113.5:7880",
                "ws://[2800:810::2]:7880",
                "ws://203.0.113.6:7880"
            ]
        );
    }

    #[tokio::test]
    async fn failover_changes_urls_and_signing_together_for_all_config_clones() {
        let cfg = direct_routes();
        let other = cfg.clone();
        let (before, revision) = cfg.route();
        assert_eq!(revision, 0);
        assert!(before.minter.is_none());
        assert!(cfg.fallback.as_ref().unwrap().use_shared());
        assert!(!cfg.fallback.as_ref().unwrap().use_shared());
        let (after, revision) = other.route();
        assert_eq!(revision, 1);
        assert_eq!(after.urls_for_client(None, None), ["wss://last-option"]);
        assert_eq!(
            voice_token(&after, "friend", "Friend", Id::new_v4())
                .await
                .unwrap(),
            "delegated-friend"
        );
        assert!(after.admin_url().is_none());
        assert!(
            before.minter.is_none(),
            "an in-flight mint keeps its original signing authority"
        );
    }

    #[test]
    fn a_verified_local_session_cannot_be_moved_by_a_later_failure() {
        let cfg = direct_routes();
        let fallback = cfg.fallback.as_ref().unwrap();
        fallback.confirmed_local();
        assert!(!fallback.use_shared());
        assert_eq!(cfg.route().1, 0);
    }

    #[test]
    fn screen_grants_follow_the_connection() {
        use livekit_api::access_token::TokenVerifier;

        let cfg = LiveKitConfig {
            explicit_url: None,
            port: 7880,
            api_key: "devkey".into(),
            api_secret: "secret-long-enough-for-hs256-signing".into(),
            minter: None,
            ice_servers: Default::default(),
            lan_host: None,
            public_host: None,
            alternate_hosts: Vec::new(),
            fallback: None,
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
            ice_servers: Default::default(),
            lan_host: None,
            public_host: None,
            alternate_hosts: Vec::new(),
            fallback: None,
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
            ice_servers: Default::default(),
            lan_host: None,
            public_host: None,
            alternate_hosts: Vec::new(),
            fallback: None,
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
            ice_servers: Default::default(),
            lan_host: Some("192.168.0.61".into()),
            public_host: None,
            alternate_hosts: Vec::new(),
            fallback: None,
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
            ice_servers: Default::default(),
            lan_host: Some("192.168.0.61".into()),
            public_host: Some("203.0.113.5".into()),
            alternate_hosts: Vec::new(),
            fallback: None,
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
            ice_servers: Default::default(),
            lan_host: Some("192.168.0.61".into()),
            public_host: Some("203.0.113.5".into()),
            alternate_hosts: Vec::new(),
            fallback: None,
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
