use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs, UdpSocket};
use std::time::Duration;

const SERVER: &str = "stun.cloudflare.com:3478";
const COOKIE: [u8; 4] = [0x21, 0x12, 0xA4, 0x42];

/// This machine's IPv4 as the internet sees it, for a box whose interfaces hold only a private one.
pub fn public_ipv4() -> Option<IpAddr> {
    let server = SERVER.to_socket_addrs().ok()?.find(|a| a.is_ipv4())?;
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    let txid: [u8; 12] = rand::random();
    let mut request = vec![0x00, 0x01, 0x00, 0x00];
    request.extend_from_slice(&COOKIE);
    request.extend_from_slice(&txid);
    let mut buf = [0u8; 512];
    for _ in 0..3 {
        socket.send_to(&request, server).ok()?;
        if let Ok((n, _)) = socket.recv_from(&mut buf) {
            return mapped_address(&buf[..n], &txid);
        }
    }
    None
}

fn mapped_address(msg: &[u8], txid: &[u8; 12]) -> Option<IpAddr> {
    if msg.len() < 20 || msg[0..2] != [0x01, 0x01] || msg[4..8] != COOKIE || &msg[8..20] != txid {
        return None;
    }
    let mut rest = &msg[20..];
    let mut plain = None;
    while rest.len() >= 4 {
        let kind = u16::from_be_bytes([rest[0], rest[1]]);
        let len = u16::from_be_bytes([rest[2], rest[3]]) as usize;
        let value = rest.get(4..4 + len)?;
        match kind {
            0x0020 => return address(value, true, txid),
            0x0001 => plain = address(value, false, txid),
            _ => {}
        }
        rest = rest.get(4 + len.next_multiple_of(4)..).unwrap_or_default();
    }
    plain
}

fn address(value: &[u8], xored: bool, txid: &[u8; 12]) -> Option<IpAddr> {
    let mask: Vec<u8> = COOKIE.iter().chain(txid).copied().collect();
    let unmask = |bytes: &[u8]| -> Vec<u8> {
        bytes
            .iter()
            .zip(&mask)
            .map(|(b, m)| if xored { b ^ m } else { *b })
            .collect()
    };
    match value.get(1)? {
        0x01 => {
            let b = unmask(value.get(4..8)?);
            Some(IpAddr::V4(Ipv4Addr::new(b[0], b[1], b[2], b[3])))
        }
        0x02 => {
            let b: [u8; 16] = unmask(value.get(4..20)?).try_into().ok()?;
            Some(IpAddr::V6(Ipv6Addr::from(b)))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_xor_mapped_address() {
        let txid = [7u8; 12];
        let ip = [203u8, 0, 113, 7];
        let mut msg = vec![0x01, 0x01, 0x00, 0x0c];
        msg.extend_from_slice(&COOKIE);
        msg.extend_from_slice(&txid);
        msg.extend_from_slice(&[0x00, 0x20, 0x00, 0x08, 0x00, 0x01, 0x00, 0x00]);
        msg.extend(ip.iter().zip(COOKIE).map(|(b, m)| b ^ m));
        assert_eq!(
            mapped_address(&msg, &txid),
            Some(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7)))
        );
        assert_eq!(mapped_address(&msg, &[8u8; 12]), None);
    }
}
