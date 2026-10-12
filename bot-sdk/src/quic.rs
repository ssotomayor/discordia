use std::net::SocketAddr;
use std::time::Duration;

use iroh::endpoint::{Connection, RecvStream, SendStream, presets};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMap, RelayMode, RelayUrl, TransportAddr};

use crate::BotError;
use crate::protocol::{GATEWAY_ALPN, QuicShare};

pub(crate) type GatewayIo = tokio::io::Join<RecvStream, SendStream>;

/// The gateway's HTTP router answers the upgrade; the QUIC key already chose the host.
pub(crate) const HANDSHAKE_URL: &str = "ws://127.0.0.1/gateway";

/// Long enough for a hole punch through a relay, as the desktop client allows.
const ATTEMPT: Duration = Duration::from_secs(15);

pub(crate) struct Link {
    endpoint: Endpoint,
    conn: Connection,
}

impl Drop for Link {
    // Without a CONNECTION_CLOSE the gateway sees the bot leave only at the idle timeout.
    fn drop(&mut self) {
        self.conn.close(0_u32.into(), b"bot leaving");
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            let endpoint = self.endpoint.clone();
            rt.spawn(async move { endpoint.close().await });
        }
    }
}

pub(crate) fn transport_addrs(share: &QuicShare) -> Vec<TransportAddr> {
    share
        .addrs
        .iter()
        .filter_map(|raw| {
            if let Ok(sock) = raw.parse::<SocketAddr>() {
                return Some(TransportAddr::Ip(sock));
            }
            raw.parse::<RelayUrl>().ok().map(TransportAddr::Relay)
        })
        .collect()
}

/// The relay named in the share string is the one the host is connected to,
/// so it is the one that can introduce us.
pub(crate) fn relay_mode(addrs: &[TransportAddr]) -> RelayMode {
    addrs
        .iter()
        .find_map(|a| match a {
            TransportAddr::Relay(url) => {
                Some(RelayMode::Custom(RelayMap::from_iter([url.clone()])))
            }
            _ => None,
        })
        .unwrap_or(RelayMode::Disabled)
}

pub(crate) async fn dial(share: &QuicShare) -> Result<(GatewayIo, Link), BotError> {
    let endpoint_id = share
        .key
        .parse::<EndpointId>()
        .map_err(|e| BotError::Quic(format!("bad endpoint key: {e}")))?;
    let addrs = transport_addrs(share);
    if addrs.is_empty() {
        return Err(BotError::Quic(
            "the share string names no address to reach the host at".into(),
        ));
    }
    let endpoint = Endpoint::builder(presets::Minimal)
        .relay_mode(relay_mode(&addrs))
        .bind()
        .await
        .map_err(|e| BotError::Quic(format!("bind: {e}")))?;
    let addr = EndpointAddr::new(endpoint_id).with_addrs(addrs);
    let conn = match tokio::time::timeout(ATTEMPT, endpoint.connect(addr, GATEWAY_ALPN)).await {
        Ok(Ok(conn)) => conn,
        Ok(Err(e)) => {
            endpoint.close().await;
            return Err(BotError::Quic(format!("connect: {e}")));
        }
        Err(_) => {
            endpoint.close().await;
            return Err(BotError::Quic(format!(
                "no answer from the host within {ATTEMPT:?}"
            )));
        }
    };
    let link = Link { endpoint, conn };
    let (send, recv) = link
        .conn
        .open_bi()
        .await
        .map_err(|e| BotError::Quic(format!("stream: {e}")))?;
    Ok((tokio::io::join(recv, send), link))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::parse_quic_share;

    fn key() -> String {
        iroh::SecretKey::generate().public().to_string()
    }

    #[test]
    fn a_relay_in_the_share_string_coordinates_and_junk_is_skipped() {
        let share = parse_quic_share(&format!(
            "quic://{}@192.168.1.5:4433;not an addr;https://relay.example/",
            key()
        ))
        .unwrap();
        let addrs = transport_addrs(&share);
        assert_eq!(addrs.len(), 2);
        assert!(matches!(relay_mode(&addrs), RelayMode::Custom(_)));
        assert!(matches!(relay_mode(&addrs[..1]), RelayMode::Disabled));
        assert!(share.key.parse::<EndpointId>().is_ok());
    }

    #[tokio::test]
    async fn a_share_string_without_an_address_is_refused_before_dialing() {
        let share = parse_quic_share(&format!("quic://{}", key())).unwrap();
        assert!(matches!(dial(&share).await, Err(BotError::Quic(_))));
    }
}
