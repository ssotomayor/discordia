use livekit::webrtc::stats::{IceCandidateType, RtcStats};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaRoute {
    pub family: &'static str,
    pub relayed: bool,
    pub protocol: String,
    pub endpoint: String,
}

#[cfg(any(
    target_os = "windows",
    target_os = "macos",
    target_os = "android",
    test
))]
#[derive(Debug, Default, PartialEq)]
pub struct PublisherNetworkStats {
    pub estimated_uplink_bps: Option<f64>,
    pub transport_rtt_ms: Option<f64>,
    pub receiver_loss_percent: Option<f64>,
    pub receiver_rtt_ms: Option<f64>,
    pub receiver_report_timestamp: Option<i64>,
    pub receiver_rtt_measurements: Option<u64>,
}

#[cfg(any(
    target_os = "windows",
    target_os = "macos",
    target_os = "android",
    test
))]
pub fn publisher_network_stats(
    stats: &[RtcStats],
    outbound: &livekit::webrtc::stats::OutboundRtpStats,
) -> PublisherNetworkStats {
    let positive = |value: f64| (value.is_finite() && value > 0.0).then_some(value);
    let pair_id = stats.iter().find_map(|entry| match entry {
        RtcStats::Transport(t) if t.rtc.id == outbound.stream.transport_id => {
            Some(t.transport.selected_candidate_pair_id.as_str())
        }
        _ => None,
    });
    let pair = stats.iter().find_map(|entry| match entry {
        RtcStats::CandidatePair(p)
            if Some(p.rtc.id.as_str()) == pair_id
                && p.candidate_pair.state
                    != Some(livekit::webrtc::stats::IceCandidatePairState::Failed) =>
        {
            Some(&p.candidate_pair)
        }
        _ => None,
    });
    let feedback = stats.iter().find_map(|entry| match entry {
        RtcStats::RemoteInboundRtp(r)
            if r.remote_inbound.local_id == outbound.rtc.id
                && r.stream.ssrc == outbound.stream.ssrc =>
        {
            Some(r)
        }
        _ => None,
    });
    PublisherNetworkStats {
        estimated_uplink_bps: pair.and_then(|p| positive(p.available_outgoing_bitrate)),
        transport_rtt_ms: pair.and_then(|p| positive(p.current_round_trip_time * 1000.0)),
        receiver_loss_percent: feedback.and_then(|r| {
            let loss = r.remote_inbound.fraction_lost;
            (loss.is_finite() && (0.0..=1.0).contains(&loss)).then_some(loss * 100.0)
        }),
        receiver_rtt_ms: feedback.and_then(|r| {
            (r.remote_inbound.round_trip_time_measurements > 0)
                .then(|| positive(r.remote_inbound.round_trip_time * 1000.0))
                .flatten()
        }),
        receiver_report_timestamp: feedback.map(|r| r.rtc.timestamp),
        receiver_rtt_measurements: feedback.map(|r| r.remote_inbound.round_trip_time_measurements),
    }
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
    // libwebrtc marks the selected pair in-progress for every consent ping it sends, so
    // only a failed pair is lost: requiring succeeded showed "unknown" every few seconds.
    if pair.state == Some(livekit::webrtc::stats::IceCandidatePairState::Failed) {
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

pub fn active_round_trip_time(
    publisher: &[RtcStats],
    subscriber: &[RtcStats],
) -> Option<std::time::Duration> {
    fn from_stats(stats: &[RtcStats]) -> Option<std::time::Duration> {
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
        if pair.current_round_trip_time > 0.0 && pair.current_round_trip_time.is_finite() {
            Some(std::time::Duration::from_secs_f64(
                pair.current_round_trip_time,
            ))
        } else {
            None
        }
    }
    from_stats(publisher).or_else(|| from_stats(subscriber))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publisher_feedback_matches_the_video_stream_and_its_selected_transport() {
        let mut stats: Vec<RtcStats> = serde_json::from_value(serde_json::json!([
            {"type":"outbound-rtp", "id":"video", "ssrc":12, "transportId":"uplink"},
            {"type":"transport", "id":"other", "selectedCandidatePairId":"wrong"},
            {"type":"candidate-pair", "id":"wrong", "availableOutgoingBitrate":90000000},
            {"type":"transport", "id":"uplink", "selectedCandidatePairId":"selected"},
            {"type":"candidate-pair", "id":"selected", "state":"in-progress", "availableOutgoingBitrate":4600000, "currentRoundTripTime":0.025},
            {"type":"remote-inbound-rtp", "localId":"audio", "ssrc":12, "fractionLost":0.8},
            {"type":"remote-inbound-rtp", "localId":"video", "ssrc":99, "fractionLost":0.9},
            {"type":"remote-inbound-rtp", "localId":"video", "ssrc":12, "fractionLost":0.02, "roundTripTime":0.04, "roundTripTimeMeasurements":3, "timestamp":1234}
        ])).unwrap();
        let RtcStats::OutboundRtp(outbound) = stats[0].clone() else {
            panic!("outbound")
        };
        assert_eq!(
            publisher_network_stats(&stats, &outbound),
            PublisherNetworkStats {
                estimated_uplink_bps: Some(4_600_000.0),
                transport_rtt_ms: Some(25.0),
                receiver_loss_percent: Some(2.0),
                receiver_rtt_ms: Some(40.0),
                receiver_report_timestamp: Some(1234),
                receiver_rtt_measurements: Some(3),
            }
        );
        if let RtcStats::CandidatePair(pair) = &mut stats[4] {
            pair.candidate_pair.state = Some(livekit::webrtc::stats::IceCandidatePairState::Failed);
        }
        if let RtcStats::RemoteInboundRtp(report) = &mut stats[7] {
            report.remote_inbound.fraction_lost = f64::NAN;
            report.remote_inbound.round_trip_time_measurements = 0;
        }
        let snapshot = publisher_network_stats(&stats, &outbound);
        assert_eq!(snapshot.estimated_uplink_bps, None);
        assert_eq!(snapshot.transport_rtt_ms, None);
        assert_eq!(snapshot.receiver_loss_percent, None);
        assert_eq!(snapshot.receiver_rtt_ms, None);
        assert_eq!(
            publisher_network_stats(&[], &outbound),
            PublisherNetworkStats::default()
        );
    }

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
    fn a_consent_ping_in_flight_keeps_the_route() {
        let mut stats: Vec<RtcStats> = serde_json::from_value(serde_json::json!([
            {"type":"transport", "selectedCandidatePairId":"selected"},
            {"type":"candidate-pair", "id":"selected", "remoteCandidateId":"remote", "state":"in-progress"},
            {"type":"remote-candidate", "id":"remote", "address":"203.0.113.9", "port":7882, "protocol":"udp", "candidateType":"host"},
            {"type":"inbound-rtp"}
        ])).unwrap();
        let (send, receive) = selected_media_routes(&stats, &[]);
        assert_eq!(send.unwrap().label(), "IPv4 · UDP");
        assert_eq!(receive.unwrap().endpoint, "203.0.113.9:7882");
        if let RtcStats::CandidatePair(pair) = &mut stats[1] {
            pair.candidate_pair.state = Some(livekit::webrtc::stats::IceCandidatePairState::Failed);
        }
        assert_eq!(selected_media_routes(&stats, &[]), (None, None));
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

    #[test]
    fn extracts_round_trip_time_from_the_selected_candidate_pair() {
        let stats: Vec<RtcStats> = serde_json::from_value(serde_json::json!([
            {"type":"transport", "selectedCandidatePairId":"selected"},
            {"type":"candidate-pair", "id":"selected", "currentRoundTripTime":0.024}
        ]))
        .unwrap();
        assert_eq!(
            active_round_trip_time(&stats, &[]),
            Some(std::time::Duration::from_millis(24))
        );
    }
}
