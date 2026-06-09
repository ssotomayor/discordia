//! LiveKit access-token minting and URL resolution.

use livekit_api::access_token::{AccessToken, VideoGrants};

use crate::protocol::Id;

#[derive(Clone)]
pub struct LiveKitConfig {
    /// Explicit URL that overrides per-connection resolution. Set this when
    /// LiveKit lives at a known address (LiveKit Cloud, a separate host,
    /// behind a reverse proxy, etc.).
    pub explicit_url: Option<String>,
    /// LiveKit WebSocket port. Used when deriving the URL from the host the
    /// client used to dial the gateway (self-host / same-machine case).
    pub port: u16,
    pub api_key: String,
    pub api_secret: String,
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
        format!("ws://{host}:{}", self.port)
    }
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

pub fn room_name(channel_id: Id) -> String {
    format!("voice-{channel_id}")
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
        };
        assert_eq!(
            cfg.url_for_client(Some("192.168.0.5:9000")),
            "ws://192.168.0.5:7880"
        );
        assert_eq!(cfg.url_for_client(None), "ws://127.0.0.1:7880");
    }
}
