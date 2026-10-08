//! The TURN relay. A friend who cannot reach a host's SFU directly asks here
//! for a relayed address; the SFU sends to that public address, so the host's
//! own NAT type no longer matters. What passes is SRTP under the call's E2EE
//! key: the relay forwards ciphertext and holds no call state.

use std::net::{IpAddr, SocketAddr};
use std::ops::RangeInclusive;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dioxusfun_protocol::rendezvous::TurnCredentials;
use turn::auth::{LongTermAuthHandler, generate_long_term_credentials};
use turn::relay::relay_range::RelayAddressGeneratorRanges;
use turn::server::Server;
use turn::server::config::{ConnConfig, ServerConfig};
use util::vnet::net::Net;

pub const DEFAULT_PORT: u16 = 7702;
/// Relayed media is allocated from a fixed range so a container or firewall
/// can open it; about eight ports per relayed friend.
pub const DEFAULT_PORTS: RangeInclusive<u16> = 7710..=7809;
pub const REALM: &str = "discordia";
/// Long enough that a host renews a handful of times a day, short enough that
/// one that stopped registering stops relaying the same day.
pub const CREDENTIAL_TTL: Duration = Duration::from_secs(12 * 60 * 60);

/// Mints the time-limited credentials (coturn's REST scheme) the relay accepts.
/// Holds the secret, so only the rendezvous can mint; hosts receive credentials.
#[derive(Clone)]
pub struct Issuer {
    urls: Vec<String>,
    secret: String,
    ttl: Duration,
}

impl Issuer {
    pub fn new(urls: Vec<String>, secret: String) -> Self {
        Self {
            urls,
            secret,
            ttl: CREDENTIAL_TTL,
        }
    }

    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    pub fn urls(&self) -> &[String] {
        &self.urls
    }

    pub fn issue(&self) -> Option<TurnCredentials> {
        let (username, credential) = generate_long_term_credentials(&self.secret, self.ttl).ok()?;
        let expires_unix = username.parse().ok()?;
        Some(TurnCredentials {
            urls: self.urls.clone(),
            username,
            credential,
            expires_unix,
        })
    }

    pub fn auth_handler(&self) -> LongTermAuthHandler {
        LongTermAuthHandler::new(self.secret.clone())
    }
}

/// Binds the relay on `bind`; `relay_ip` is the public address allocations are
/// handed out on, which a bind on `0.0.0.0` cannot know by itself.
pub async fn spawn(
    bind: SocketAddr,
    relay_ip: IpAddr,
    ports: RangeInclusive<u16>,
    issuer: &Issuer,
) -> Result<Server, String> {
    let socket = tokio::net::UdpSocket::bind(bind)
        .await
        .map_err(|e| format!("turn bind {bind}: {e}"))?;
    let bound = socket.local_addr().map_err(|e| e.to_string())?;
    let server = Server::new(ServerConfig {
        conn_configs: vec![ConnConfig {
            conn: Arc::new(socket),
            relay_addr_generator: Box::new(RelayAddressGeneratorRanges {
                relay_address: relay_ip,
                min_port: *ports.start(),
                max_port: *ports.end(),
                max_retries: 0,
                address: "0.0.0.0".into(),
                net: Arc::new(Net::new(None)),
            }),
        }],
        realm: REALM.into(),
        auth_handler: Arc::new(issuer.auth_handler()),
        channel_bind_timeout: Duration::from_secs(0),
        alloc_close_notify: None,
    })
    .await
    .map_err(|e| format!("turn server: {e}"))?;
    tracing::info!(%bound, %relay_ip, ports = ?ports, "turn relay listening");
    Ok(server)
}

/// `turn:host:port?transport=udp` → `host:port`, the part a socket dials.
pub fn host_port(url: &str) -> Option<&str> {
    let rest = url
        .strip_prefix("turn:")
        .or_else(|| url.strip_prefix("turns:"))
        .or_else(|| url.strip_prefix("stun:"))?;
    let rest = rest.split('?').next()?;
    (!rest.is_empty()).then_some(rest)
}

/// `7710-7809`, both ends included.
pub fn parse_ports(spec: &str) -> Result<RangeInclusive<u16>, String> {
    let (lo, hi) = spec
        .split_once('-')
        .ok_or_else(|| format!("port range '{spec}' is not low-high"))?;
    let lo: u16 = lo.trim().parse().map_err(|e| format!("port '{lo}': {e}"))?;
    let hi: u16 = hi.trim().parse().map_err(|e| format!("port '{hi}': {e}"))?;
    if lo == 0 || hi < lo {
        return Err(format!("port range '{spec}' is empty"));
    }
    Ok(lo..=hi)
}

pub async fn resolve_relay_ip(url: &str) -> Result<IpAddr, String> {
    let target = host_port(url).ok_or_else(|| format!("not a turn: URL: {url}"))?;
    tokio::net::lookup_host(target)
        .await
        .map_err(|e| format!("resolve {target}: {e}"))?
        .map(|addr| addr.ip())
        .find(|ip| ip.is_ipv4())
        .ok_or_else(|| format!("{target} has no IPv4 address"))
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use turn::client::{Client, ClientConfig};

    /// The server's read loops end with the `Server`, so a test holds it.
    async fn relay() -> (Server, SocketAddr, Issuer) {
        let issuer = Issuer::new(vec!["turn:127.0.0.1:0".into()], "test-secret".into());
        let bind: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let socket = tokio::net::UdpSocket::bind(bind).await.unwrap();
        let addr = socket.local_addr().unwrap();
        drop(socket);
        let server = spawn(addr, addr.ip(), 49_000..=49_099, &issuer)
            .await
            .unwrap();
        (server, addr, issuer)
    }

    async fn allocate(addr: SocketAddr, username: &str, credential: &str) -> Result<(), String> {
        let conn = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let client = Client::new(ClientConfig {
            stun_serv_addr: String::new(),
            turn_serv_addr: addr.to_string(),
            username: username.into(),
            password: credential.into(),
            realm: REALM.into(),
            software: String::new(),
            rto_in_ms: 0,
            conn: Arc::new(conn),
            vnet: None,
        })
        .await
        .map_err(|e| e.to_string())?;
        client.listen().await.map_err(|e| e.to_string())?;
        let outcome = client
            .allocate()
            .await
            .map(|_| ())
            .map_err(|e| e.to_string());
        let _ = client.close().await;
        outcome
    }

    #[tokio::test]
    async fn issued_credentials_allocate_and_forged_or_expired_ones_do_not() {
        let (_server, addr, issuer) = relay().await;
        let creds = issuer.issue().unwrap();
        assert!(creds.expires_unix > now_unix());
        allocate(addr, &creds.username, &creds.credential)
            .await
            .expect("fresh credentials allocate");

        let forged = allocate(addr, &creds.username, "not-the-hmac").await;
        assert!(forged.is_err(), "a wrong credential is refused");

        let expired = Issuer::new(Vec::new(), "test-secret".into())
            .with_ttl(Duration::from_secs(0))
            .issue()
            .unwrap();
        tokio::time::sleep(Duration::from_millis(1100)).await;
        let stale = allocate(addr, &expired.username, &expired.credential).await;
        assert!(stale.is_err(), "an expired username is refused");
    }

    #[test]
    fn a_port_range_is_inclusive_and_must_not_be_empty() {
        assert_eq!(parse_ports("7710-7809").unwrap(), 7710..=7809);
        assert_eq!(parse_ports(" 5000 - 5000 ").unwrap(), 5000..=5000);
        assert!(parse_ports("7809-7710").is_err());
        assert!(parse_ports("0-10").is_err());
        assert!(parse_ports("7710").is_err());
    }

    #[test]
    fn a_turn_url_yields_the_address_to_dial() {
        assert_eq!(
            host_port("turn:rendezvous.example:7702?transport=udp"),
            Some("rendezvous.example:7702")
        );
        assert_eq!(host_port("stun:1.2.3.4:3478"), Some("1.2.3.4:3478"));
        assert_eq!(host_port("wss://not-turn"), None);
        assert_eq!(host_port("turn:"), None);
    }
}
