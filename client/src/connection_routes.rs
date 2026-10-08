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
    })?;
    if pair.state != Some(livekit::webrtc::stats::IceCandidatePairState::Succeeded) {
        return None;
    }
    let local_relay = stats.iter().any(|s| {
        matches!(s,
        RtcStats::LocalCandidate(c) if c.rtc.id == pair.local_candidate_id
            && c.local_candidate.candidate_type == Some(IceCandidateType::Relay))
    });
    let family = match remote.address.parse::<std::net::IpAddr>() {
        Ok(ip) if ip.is_loopback() => "local",
        Ok(ip) if ip.is_ipv6() && ip.to_canonical().is_ipv4() => "IPv4",
        Ok(ip) if ip.is_ipv6() => "IPv6",
        Ok(_) => "IPv4",
        Err(_) => "IP unknown",
    };
    Some(MediaRoute {
        family,
        relayed: local_relay || remote.candidate_type == Some(IceCandidateType::Relay),
        protocol: remote.protocol.clone(),
        endpoint: match remote.address.parse::<std::net::IpAddr>() {
            Ok(ip) => std::net::SocketAddr::new(ip, remote.port.try_into().ok()?).to_string(),
            Err(_) => remote.address.clone(),
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
}
