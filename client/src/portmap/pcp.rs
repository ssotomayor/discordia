use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::sync::Mutex;

pub struct Client {
    socket: UdpSocket,
    client_ip: Ipv4Addr,
    mappings: Mutex<HashMap<(u8, u16), Mapping>>,
}

struct Mapping {
    nonce: [u8; 12],
    external: SocketAddr,
    renew_at: tokio::time::Instant,
}

impl Client {
    pub async fn new(local_ip: Ipv4Addr, gateway: SocketAddr) -> Result<Self, String> {
        let socket = UdpSocket::bind((local_ip, 0))
            .await
            .map_err(|e| e.to_string())?;
        socket.connect(gateway).await.map_err(|e| e.to_string())?;
        Ok(Self {
            socket,
            client_ip: local_ip,
            mappings: Mutex::new(HashMap::new()),
        })
    }

    pub async fn map(&self, protocol: u8, port: u16, lifetime: u32) -> Result<SocketAddr, String> {
        // PCP renewals and deletions must carry the nonce from the original MAP.
        let mut mappings = self.mappings.lock().await;
        let previous = mappings.get(&(protocol, port));
        let nonce = previous.map(|m| m.nonce).unwrap_or_else(rand::random);
        let external = previous.map(|m| m.external);
        let request = map_request(self.client_ip, protocol, port, lifetime, nonce, external);
        let mut response = [0; 1100];
        for wait in [Duration::from_secs(3), Duration::from_secs(6)] {
            self.socket
                .send(&request)
                .await
                .map_err(|e| e.to_string())?;
            let reply = async {
                loop {
                    let len = self
                        .socket
                        .recv(&mut response)
                        .await
                        .map_err(|e| e.to_string())?;
                    if let Some(result) =
                        map_response(&response[..len], protocol, port, lifetime, nonce)
                    {
                        return result;
                    }
                }
            };
            if let Ok(result) = tokio::time::timeout(wait, reply).await {
                let external = result?;
                if lifetime == 0 {
                    mappings.remove(&(protocol, port));
                } else {
                    let lifetime = Duration::from_secs(u32::from_be_bytes(
                        response[4..8].try_into().unwrap(),
                    ) as u64);
                    mappings.insert(
                        (protocol, port),
                        Mapping {
                            nonce,
                            external,
                            renew_at: tokio::time::Instant::now() + lifetime / 2,
                        },
                    );
                }
                return Ok(external);
            }
        }
        Err("no reply from the router to PCP MAP".into())
    }

    pub async fn renew_after(&self) -> Duration {
        self.mappings
            .lock()
            .await
            .values()
            .map(|m| {
                m.renew_at
                    .saturating_duration_since(tokio::time::Instant::now())
            })
            .min()
            .unwrap_or(Duration::from_secs(1800))
            .max(Duration::from_millis(100))
    }
}

fn map_request(
    client_ip: Ipv4Addr,
    protocol: u8,
    port: u16,
    lifetime: u32,
    nonce: [u8; 12],
    external: Option<SocketAddr>,
) -> [u8; 60] {
    let mut packet = [0; 60];
    packet[0] = 2;
    packet[1] = 1;
    packet[4..8].copy_from_slice(&lifetime.to_be_bytes());
    packet[8..24].copy_from_slice(&client_ip.to_ipv6_mapped().octets());
    packet[24..36].copy_from_slice(&nonce);
    packet[36] = protocol;
    packet[40..42].copy_from_slice(&port.to_be_bytes());
    packet[42..44].copy_from_slice(&external.map(|a| a.port()).unwrap_or(port).to_be_bytes());
    if let Some(addr) = external {
        let ip = match addr.ip() {
            IpAddr::V4(ip) => ip.to_ipv6_mapped(),
            IpAddr::V6(ip) => ip,
        };
        packet[44..60].copy_from_slice(&ip.octets());
    }
    packet
}

fn map_response(
    packet: &[u8],
    protocol: u8,
    port: u16,
    lifetime: u32,
    nonce: [u8; 12],
) -> Option<Result<SocketAddr, String>> {
    if packet.len() < 24 || packet[0] != 2 || packet[1] != 0x81 {
        return None;
    }
    if packet[3] != 0 {
        return Some(Err(format!(
            "router rejected PCP MAP (result {})",
            packet[3]
        )));
    }
    if packet.len() < 60
        || packet[24..36] != nonce
        || packet[36] != protocol
        || packet[40..42] != port.to_be_bytes()
    {
        return None;
    }
    let granted_lifetime = u32::from_be_bytes(packet[4..8].try_into().unwrap());
    if lifetime != 0 && granted_lifetime == 0 {
        return Some(Err("router granted PCP MAP with zero lifetime".into()));
    }
    let port = u16::from_be_bytes(packet[42..44].try_into().unwrap());
    let v6 = Ipv6Addr::from(<[u8; 16]>::try_from(&packet[44..60]).unwrap());
    let ip = v6
        .to_ipv4_mapped()
        .map(IpAddr::V4)
        .unwrap_or(IpAddr::V6(v6));
    if lifetime != 0 && (port == 0 || ip.is_unspecified() || ip.is_multicast()) {
        return Some(Err("router returned an unusable PCP endpoint".into()));
    }
    Some(Ok(SocketAddr::new(ip, port)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(request: &[u8]) -> Vec<u8> {
        let mut response = request.to_vec();
        response[1] = 0x81;
        response[8..24].fill(0);
        response[44..60].copy_from_slice(&Ipv4Addr::new(203, 0, 113, 9).to_ipv6_mapped().octets());
        response
    }

    #[test]
    fn ignores_unrelated_and_truncated_replies() {
        let nonce = [7; 12];
        let req = map_request(Ipv4Addr::LOCALHOST, 17, 9001, 3600, nonce, None);
        let mut reply = response(&req);
        assert!(map_response(&reply[..59], 17, 9001, 3600, nonce).is_none());
        reply[24] ^= 1;
        assert!(map_response(&reply, 17, 9001, 3600, nonce).is_none());
        let mut reply = response(&req);
        reply[3] = 8;
        assert!(
            map_response(&reply, 17, 9001, 3600, nonce)
                .unwrap()
                .is_err()
        );
    }

    #[tokio::test]
    async fn renewal_and_delete_reuse_the_nonce_and_granted_endpoint() {
        let router = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let client = Client::new(Ipv4Addr::LOCALHOST, router.local_addr().unwrap())
            .await
            .unwrap();
        let task = tokio::spawn(async move {
            let mut buf = [0; 100];
            let mut original = Vec::new();
            for lifetime in [3600u32, 3600, 0] {
                let (len, peer) = router.recv_from(&mut buf).await.unwrap();
                let request = &buf[..len];
                assert_eq!(request[4..8], lifetime.to_be_bytes());
                if original.is_empty() {
                    original = request.to_vec();
                } else {
                    assert_eq!(request[24..36], original[24..36]);
                    assert_eq!(request[42..44], 19001u16.to_be_bytes());
                    assert_eq!(&request[44..60], &response(&original)[44..60]);
                }
                let mut reply = response(request);
                if lifetime > 0 {
                    reply[4..8].copy_from_slice(&120u32.to_be_bytes());
                }
                reply[42..44].copy_from_slice(&19001u16.to_be_bytes());
                router.send_to(&reply, peer).await.unwrap();
            }
        });
        for lifetime in [3600, 3600, 0] {
            let endpoint = client.map(17, 9001, lifetime).await.unwrap();
            assert_eq!(endpoint, "203.0.113.9:19001".parse().unwrap());
            if lifetime > 0 {
                let renewal = client.renew_after().await;
                assert!(renewal <= Duration::from_secs(60) && renewal > Duration::from_secs(59));
            }
        }
        task.await.unwrap();
        assert!(client.mappings.lock().await.is_empty());
    }
}
