use super::*;
use std::collections::VecDeque;
use std::time::Duration;

const FEEDBACK_TTL: Duration = Duration::from_secs(7);
const WINDOW: Duration = Duration::from_secs(5);

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub(super) fn spawn_video_budget(
    state: Signal<AppState>,
    track: LocalVideoTrack,
    limit: u64,
) -> Task {
    spawn(async move {
        let mut limited = false;
        loop {
            let constrained = state.peek().voice_uplink_congested;
            if constrained || limited {
                match track.set_network_bitrate_limit(constrained.then_some(limit)) {
                    Ok(()) => limited = constrained,
                    Err(error) => tracing::debug!(%error, "video bandwidth adjustment unavailable"),
                }
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    })
}

#[derive(Clone, Copy)]
struct Reception {
    at: Instant,
    received: u64,
    lost: u64,
    discarded: u64,
    concealed: u64,
    samples: u64,
}

#[derive(Clone, Copy)]
pub(super) struct RecentReception {
    pub loss_pct: f32,
    pub discarded_packets: u64,
    pub concealed_pct: f32,
}

impl RecentReception {
    fn needs_diagnostics(self, display: bool) -> bool {
        self.loss_pct > 0.0 || self.concealed_pct >= 1.0 || (display && self.discarded_packets > 0)
    }
}

#[derive(Default)]
pub(super) struct ReceiveWindow {
    stream: Option<(String, u32)>,
    samples: VecDeque<Reception>,
    logged_at: Option<Instant>,
}

impl ReceiveWindow {
    pub fn sample(
        &mut self,
        s: &livekit::webrtc::stats::InboundRtpStats,
        now: Instant,
    ) -> Option<RecentReception> {
        let current = Reception {
            at: now,
            received: s.received.packets_received,
            lost: s.received.packets_lost.max(0) as u64,
            discarded: s.inbound.packets_discarded,
            concealed: s.inbound.concealed_samples,
            samples: s.inbound.total_samples_received,
        };
        let stream = (s.rtc.id.clone(), s.stream.ssrc);
        if self.stream.as_ref() != Some(&stream)
            || self.samples.back().is_some_and(|last| {
                current.received < last.received
                    || current.discarded < last.discarded
                    || current.samples < last.samples
                    || now.duration_since(last.at) > FEEDBACK_TTL
            })
        {
            self.samples.clear();
            self.stream = Some(stream);
        }
        self.samples.push_back(current);
        while self.samples.len() > 1 && now.duration_since(self.samples[0].at) > WINDOW {
            self.samples.pop_front();
        }
        let previous = self.samples.front()?;
        let received = current.received - previous.received;
        let lost = current.lost.saturating_sub(previous.lost);
        let total = received + lost;
        if total == 0 {
            return None;
        }
        let samples = current.samples - previous.samples;
        Some(RecentReception {
            loss_pct: 100.0 * lost as f32 / total as f32,
            // NetEq also counts redundant RED blocks, so RTP packets are not a valid denominator.
            discarded_packets: current.discarded - previous.discarded,
            concealed_pct: if samples == 0 {
                0.0
            } else {
                (100.0 * current.concealed.saturating_sub(previous.concealed) as f32
                    / samples as f32)
                    .min(100.0)
            },
        })
    }
}

const LOSS_LOG_EVERY: Duration = Duration::from_secs(5);

#[derive(Default)]
struct UplinkFeedback {
    last_report: Option<(u32, i64)>,
    last_packets: Option<(u32, u64)>,
}

impl UplinkFeedback {
    /// Loss from a receiver report not seen before, while audio is flowing.
    fn sample(&mut self, stats: &[RtcStats]) -> Option<f32> {
        let out = stats.iter().find_map(|s| match s {
            RtcStats::OutboundRtp(s) => Some(s),
            _ => None,
        })?;
        let sent = (out.stream.ssrc, out.sent.packets_sent);
        let previous = self.last_packets.replace(sent);
        if previous.is_some_and(|(ssrc, packets)| ssrc != sent.0 || sent.1 < packets) {
            self.last_report = None;
        }
        // DTX sends a few packets a second in silence; a report over those says little.
        let active = previous
            .is_some_and(|(ssrc, packets)| ssrc == sent.0 && sent.1.saturating_sub(packets) >= 20);
        let report = stats.iter().find_map(|s| match s {
            RtcStats::RemoteInboundRtp(s)
                if s.remote_inbound.local_id == out.rtc.id && s.stream.ssrc == out.stream.ssrc =>
            {
                Some(s)
            }
            _ => None,
        })?;
        let key = (report.stream.ssrc, report.rtc.timestamp);
        if self
            .last_report
            .is_some_and(|last| key.0 == last.0 && key.1 <= last.1)
        {
            return None;
        }
        self.last_report = Some(key);
        let loss = report.remote_inbound.fraction_lost;
        (active && loss.is_finite() && (0.0..=1.0).contains(&loss)).then_some(loss as f32 * 100.0)
    }
}

/// What a `voice loss:` line compares against Discord: whether Opus FEC and NACK engage.
fn codec_line(stats: &[RtcStats], codec_id: &str) -> String {
    stats
        .iter()
        .find_map(|s| match s {
            RtcStats::Codec(c) if c.rtc.id == codec_id => {
                Some(format!("{} {}", c.codec.mime_type, c.codec.sdp_fmtp_line))
            }
            _ => None,
        })
        .unwrap_or_default()
}

pub(super) fn spawn_stats_task(
    mut state: Signal<AppState>,
    room: Arc<Room>,
    audio: LocalAudioTrack,
    self_pubkey: Option<String>,
    controls: AudioControls,
) -> Task {
    spawn(async move {
        let mut windows = HashMap::<String, ReceiveWindow>::new();
        let mut uplink = UplinkFeedback::default();
        let mut applied = None;
        let mut applied_ssrc = None;
        let mut prev_out = None;
        let mut latest_uplink_problem = None;
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let connected = state.peek().voice.phase == VoicePhase::Connected;
            let routes = if connected {
                match tokio::time::timeout(Duration::from_secs(2), room.get_stats()).await {
                    Ok(Ok(s)) => crate::connection_routes::selected_media_routes(
                        &s.publisher_stats,
                        &s.subscriber_stats,
                    ),
                    _ => (
                        state.peek().voice_send_route.clone(),
                        state.peek().voice_receive_route.clone(),
                    ),
                }
            } else {
                (None, None)
            };
            if state.peek().voice_send_route != routes.0
                || state.peek().voice_receive_route != routes.1
            {
                tracing::info!(
                    send = ?routes.0.as_ref().map(|r| format!("{} {}", r.label(), r.endpoint)),
                    receive = ?routes.1.as_ref().map(|r| format!("{} {}", r.label(), r.endpoint)),
                    "voice route: selected media path changed"
                );
                let mut s = state.write();
                s.voice_send_route = routes.0;
                s.voice_receive_route = routes.1;
            }
            if !connected {
                applied = None;
                applied_ssrc = None;
                latest_uplink_problem = None;
                uplink = UplinkFeedback::default();
                windows.clear();
                if state.peek().voice_uplink_congested {
                    state.write().voice_uplink_congested = false;
                }
                continue;
            }
            let Ok(Ok(stats)) =
                tokio::time::timeout(Duration::from_secs(1), audio.get_stats()).await
            else {
                continue;
            };
            let now = Instant::now();
            let out = stats.iter().find_map(|s| match s {
                RtcStats::OutboundRtp(s) => Some(s),
                _ => None,
            });
            let ssrc = out.map(|s| s.stream.ssrc);
            if ssrc != applied_ssrc {
                applied = None;
                applied_ssrc = ssrc;
                if let Some(out) = out {
                    tracing::info!(
                        codec = %codec_line(&stats, &out.stream.codec_id),
                        "voice loss: microphone codec negotiated"
                    );
                }
            }
            // Loss keeps the voice bitrate: Opus FEC and resends repair it, and a lower
            // bitrate sends as many packets, so it only cost quality (Discord holds it too).
            let ceiling = controls.bitrate_kbps.load(Ordering::Relaxed);
            if applied != Some(ceiling) {
                match audio.set_max_bitrate(u64::from(ceiling) * 1000) {
                    Ok(()) => applied = Some(ceiling),
                    Err(error) => tracing::warn!(%error, "could not apply the voice bitrate"),
                }
            }
            let upstream_loss = uplink.sample(&stats);
            if let (Some(loss), Some(out)) = (upstream_loss.filter(|l| *l > 0.0), out) {
                tracing::info!(
                    loss_pct = loss,
                    target_kbps = (out.outbound.target_bitrate / 1000.0).round(),
                    nacks = out.outbound.nack_count,
                    resent = out.outbound.retransmitted_packets_sent,
                    "voice loss: sending"
                );
            }
            if upstream_loss.is_some_and(|v| v >= 5.0) {
                latest_uplink_problem = Some(now);
            }
            let uplink_congested = latest_uplink_problem
                .is_some_and(|at| now.duration_since(at) < Duration::from_secs(20));
            if state.peek().voice_uplink_congested != uplink_congested {
                state.write().voice_uplink_congested = uplink_congested;
            }
            let display = controls.stats_polling.load(Ordering::Relaxed);
            let mut next = HashMap::new();
            let mut live = HashSet::new();
            let mut tracks = Vec::new();
            for (identity, participant) in room.remote_participants() {
                for publication in participant.track_publications().into_values() {
                    if publication.source() != TrackSource::Microphone
                        || publication.name() == SOUNDBOARD_TRACK
                    {
                        continue;
                    }
                    let Some(RemoteTrack::Audio(track)) = publication.track() else {
                        continue;
                    };
                    tracks.push((identity.clone(), publication.sid().to_string(), track));
                }
            }
            let mut requests = futures_util::stream::iter(tracks)
                .map(|(identity, sid, track)| async move {
                    let Ok(Ok(stats)) =
                        tokio::time::timeout(Duration::from_millis(500), track.get_stats()).await
                    else {
                        return None;
                    };
                    Some((identity, sid, stats))
                })
                .buffer_unordered(8);
            while let Some(result) = requests.next().await {
                let Some((identity, sid, track_stats)) = result else {
                    continue;
                };
                let Some(inbound) = track_stats.iter().find_map(|s| match s {
                    RtcStats::InboundRtp(s) => Some(s),
                    _ => None,
                }) else {
                    continue;
                };
                live.insert(sid.clone());
                let window = windows.entry(sid.clone()).or_default();
                let recent = window.sample(inbound, now);
                if let Some(recent) = recent.filter(|r| r.needs_diagnostics(display))
                    && window
                        .logged_at
                        .is_none_or(|at| now.duration_since(at) >= LOSS_LOG_EVERY)
                {
                    window.logged_at = Some(now);
                    let i = &inbound.inbound;
                    tracing::info!(
                        from = %crate::identity::truncate_pubkey(&identity.0),
                        loss_pct = recent.loss_pct,
                        discarded_recent = recent.discarded_packets,
                        concealed_pct = recent.concealed_pct,
                        packets_received = inbound.received.packets_received,
                        packets_discarded = i.packets_discarded,
                        concealment_events = i.concealment_events,
                        jitter_ms = inbound.received.jitter * 1000.0,
                        fec_received = i.fec_packets_received,
                        fec_used = i.fec_packets_received.saturating_sub(i.fec_packets_discarded),
                        nacks = i.nack_count,
                        resent = i.retransmitted_packets_received,
                        target_buffer_ms = if i.jitter_buffer_emitted_count == 0 {
                            0.0
                        } else {
                            (1000.0 * i.jitter_buffer_target_delay
                                / i.jitter_buffer_emitted_count as f64)
                                .round()
                        },
                        codec = %codec_line(&track_stats, &inbound.stream.codec_id),
                        "voice loss: receiving"
                    );
                }
                if display {
                    let mut view = inbound_stats(inbound);
                    if let TrackStats::Inbound {
                        recent_loss_pct,
                        discarded_packets,
                        concealed_pct,
                        ..
                    } = &mut view
                    {
                        *recent_loss_pct = recent.map(|r| r.loss_pct);
                        *discarded_packets = recent.map(|r| r.discarded_packets);
                        *concealed_pct = recent.map(|r| r.concealed_pct);
                    }
                    next.insert(identity.0.clone(), view);
                }
            }
            windows.retain(|sid, _| live.contains(sid));
            if display
                && let Some(pk) = &self_pubkey
                && let Some(out) = out
            {
                let rates =
                    outbound_rates(prev_out, out.sent.packets_sent, out.sent.bytes_sent, now);
                prev_out = Some((out.sent.packets_sent, out.sent.bytes_sent, now));
                next.insert(
                    pk.clone(),
                    TrackStats::Outbound {
                        bitrate_kbps: rates.map(|(_, b)| b),
                        packets_per_sec: rates.map(|(p, _)| p),
                        target_kbps: ceiling,
                    },
                );
            } else {
                prev_out = None;
            }
            if state.peek().voice_stats != next {
                state.write().voice_stats = next;
            }
        }
    })
}

#[cfg(test)]
#[path = "voice_network_tests.rs"]
mod tests;
