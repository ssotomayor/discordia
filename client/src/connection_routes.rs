use livekit::webrtc::stats::{IceCandidateType, RtcStats};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaRoute {
    pub family: &'static str,
    pub relayed: bool,
    pub protocol: String,
    pub endpoint: String,
}

impl MediaRoute {
    pub fn label(&self) -> String {
        format!(
            "{}{} · {}",
            self.family,
            if self.relayed { " · TURN relay" } else { "" },
            if self.protocol.is_empty() {
                "UNKNOWN".into()
            } else {
                self.protocol.to_uppercase()
            }
        )
    }
}

pub fn selected_media_route(stats: &[RtcStats]) -> Option<MediaRoute> {
    let pair_id = stats.iter().find_map(|s| match s {
        RtcStats::Transport(t) if !t.transport.selected_candidate_pair_id.is_empty() => {
            Some(t.transport.selected_candidate_pair_id.as_str())
        }
        _ => None,
    })?;
    let pair = stats.iter().find_map(|s| match s {
        RtcStats::CandidatePair(p) if p.rtc.id == pair_id => Some(&p.candidate_pair),
        _ => None,
    })?;
    let remote = stats.iter().find_map(|s| match s {
        RtcStats::RemoteCandidate(c) if c.rtc.id == pair.remote_candidate_id => {
            Some(&c.remote_candidate)
        }
        _ => None,
    });
    if pair.state != Some(livekit::webrtc::stats::IceCandidatePairState::Succeeded) {
        return None;
    }
    let local = stats.iter().find_map(|s| match s {
        RtcStats::LocalCandidate(c) if c.rtc.id == pair.local_candidate_id => {
            Some(&c.local_candidate)
        }
        _ => None,
    });
    // libwebrtc omits a peer-reflexive remote's address, and sometimes the remote itself;
    // both ends of a pair share a family.
    let address = [remote, local]
        .into_iter()
        .flatten()
        .find_map(|c| c.address.parse::<std::net::IpAddr>().ok());
    let family = match address {
        Some(ip) if ip.is_loopback() => "local",
        Some(ip) if ip.is_ipv6() && ip.to_canonical().is_ipv4() => "IPv4",
        Some(ip) if ip.is_ipv6() => "IPv6",
        Some(_) => "IPv4",
        None => "IP unknown",
    };
    let protocol = [remote, local]
        .into_iter()
        .flatten()
        .map(|c| c.protocol.clone())
        .find(|p| !p.is_empty())
        .unwrap_or_default();
    Some(MediaRoute {
        family,
        relayed: [remote, local]
            .into_iter()
            .flatten()
            .any(|c| c.candidate_type == Some(IceCandidateType::Relay)),
        protocol,
        endpoint: match remote.map(|c| (c.address.parse::<std::net::IpAddr>(), c)) {
            Some((Ok(ip), c)) => std::net::SocketAddr::new(ip, c.port.try_into().ok()?).to_string(),
            Some((Err(_), c)) => c.address.clone(),
            None => String::new(),
        },
    })
}

pub fn selected_media_routes(
    publisher: &[RtcStats],
    subscriber: &[RtcStats],
) -> (Option<MediaRoute>, Option<MediaRoute>) {
    let send = selected_media_route(publisher);
    let receive = if subscriber.is_empty()
        && publisher
            .iter()
            .any(|s| matches!(s, RtcStats::InboundRtp(_)))
    {
        selected_media_route(publisher)
    } else {
        selected_media_route(subscriber)
    };
    (send, receive)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv6_and_turn_are_separate_and_single_pc_reception_requires_inbound_stats() {
        let mut stats: Vec<RtcStats> = serde_json::from_value(serde_json::json!([
            {"type":"transport", "selectedCandidatePairId":"selected"},
            {"type":"candidate-pair", "id":"selected", "remoteCandidateId":"remote", "state":"succeeded"},
            {"type":"remote-candidate", "id":"remote", "address":"2800:810::1", "port":7882, "protocol":"udp", "candidateType":"relay"}
        ])).unwrap();
        let (send, receive) = selected_media_routes(&stats, &[]);
        assert_eq!(send.unwrap().label(), "IPv6 · TURN relay · UDP");
        assert!(receive.is_none());
        stats.push(serde_json::from_value(serde_json::json!({"type":"inbound-rtp"})).unwrap());
        let (send, receive) = selected_media_routes(&stats, &[]);
        assert_eq!(send, receive);
        assert_eq!(receive.unwrap().endpoint, "[2800:810::1]:7882");
        if let RtcStats::CandidatePair(pair) = &mut stats[1] {
            pair.candidate_pair.state = Some(livekit::webrtc::stats::IceCandidatePairState::Failed);
        }
        assert!(selected_media_route(&stats).is_none());
    }

    #[test]
    fn uses_the_selected_pair_instead_of_an_available_ipv6_candidate() {
        let stats: Vec<RtcStats> = serde_json::from_value(serde_json::json!([
            {"type":"transport", "selectedCandidatePairId":"selected"},
            {"type":"candidate-pair", "id":"other", "remoteCandidateId":"v6"},
            {"type":"candidate-pair", "id":"selected", "remoteCandidateId":"v4", "localCandidateId":"local", "state":"succeeded"},
            {"type":"remote-candidate", "id":"v6", "address":"2800:810::1", "protocol":"udp"},
            {"type":"remote-candidate", "id":"v4", "address":"203.0.113.9", "port":7882, "protocol":"tcp", "candidateType":"host"},
            {"type":"local-candidate", "id":"local", "candidateType":"relay"}
        ])).unwrap();
        let route = selected_media_route(&stats).unwrap();
        assert_eq!(route.label(), "IPv4 · TURN relay · TCP");
        assert_eq!(route.endpoint, "203.0.113.9:7882");
        assert!(selected_media_route(&stats[1..]).is_none());
    }

    #[test]
    fn a_peer_reflexive_remote_without_an_address_or_stats_takes_the_local_family() {
        let stats: Vec<RtcStats> = serde_json::from_value(serde_json::json!([
            {"type":"transport", "selectedCandidatePairId":"selected"},
            {"type":"candidate-pair", "id":"selected", "remoteCandidateId":"remote", "localCandidateId":"local", "state":"succeeded"},
            {"type":"remote-candidate", "id":"remote", "candidateType":"prflx"},
            {"type":"local-candidate", "id":"local", "address":"192.168.1.20", "port":50000, "protocol":"udp", "candidateType":"host"}
        ])).unwrap();
        let route = selected_media_route(&stats).unwrap();
        assert_eq!(route.label(), "IPv4 · UDP");
        assert!(!route.relayed);
        let unlisted = selected_media_route(&[&stats[..2], &stats[3..]].concat()).unwrap();
        assert_eq!(unlisted.label(), "IPv4 · UDP");
        assert_eq!(unlisted.endpoint, "");
    }
}
