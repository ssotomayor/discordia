use super::*;
use std::collections::VecDeque;
use std::time::Duration;

const FEEDBACK_TTL: Duration = Duration::from_secs(7);
const WINDOW: Duration = Duration::from_secs(5);

#[cfg(any(target_os = "android", target_os = "macos", target_os = "windows"))]
pub(super) fn spawn_video_budget(
    state: Signal<AppState>,
    track: LocalVideoTrack,
    limit: u64,
) -> Task {
    crate::service_task::spawn(async move {
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
    silent_concealed: u64,
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
            silent_concealed: s.inbound.silent_concealed_samples,
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
        let silent = current
            .silent_concealed
            .saturating_sub(previous.silent_concealed);
        let samples = (current.samples - previous.samples).saturating_sub(silent);
        let concealed = current
            .concealed
            .saturating_sub(previous.concealed)
            .saturating_sub(silent);
        Some(RecentReception {
            loss_pct: 100.0 * lost as f32 / total as f32,
            // NetEq also counts redundant RED blocks, so RTP packets are not a valid denominator.
            discarded_packets: current.discarded - previous.discarded,
            concealed_pct: if samples == 0 {
                0.0
            } else {
                (100.0 * concealed as f32 / samples as f32).min(100.0)
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

fn microphone_feedback(
    stats: &[RtcStats],
) -> Option<&livekit::webrtc::stats::RemoteInboundRtpStats> {
    let out = stats.iter().find_map(|s| match s {
        RtcStats::OutboundRtp(s) => Some(s),
        _ => None,
    })?;
    stats.iter().find_map(|s| match s {
        RtcStats::RemoteInboundRtp(s)
            if s.remote_inbound.local_id == out.rtc.id && s.stream.ssrc == out.stream.ssrc =>
        {
            Some(s)
        }
        _ => None,
    })
}

fn reported_microphone_loss(stats: &[RtcStats]) -> Option<f32> {
    let loss = microphone_feedback(stats)?.remote_inbound.fraction_lost;
    (loss.is_finite() && (0.0..=1.0).contains(&loss)).then_some(loss as f32 * 100.0)
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
        let report = microphone_feedback(stats)?;
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
        let mut audio_log_at = Instant::now();
        let mut latest_uplink_problem = None;
        // Counters are cumulative as libwebrtc reports them; the reader takes the deltas.
        let mut send_csv = crate::voice_record::Csv::new(
            "net-send",
            "ssrc,packets_sent,bytes_sent,resent,nacks,target_kbps,upstream_loss_pct",
        );
        let mut recv_csv = crate::voice_record::Csv::new(
            "net-recv",
            "from,ssrc,packets_received,packets_lost,discarded,jitter_ms,jb_delay_s,jb_target_delay_s,jb_min_delay_s,jb_emitted,total_samples,concealed,silent_concealed,concealment_events,inserted_decel,removed_accel,audio_level,total_audio_energy,fec_received,fec_discarded,nacks,resent",
        );
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let connected = state.peek().voice.phase == VoicePhase::Connected;
            let (routes, room_rtt) = if connected {
                match tokio::time::timeout(Duration::from_secs(2), room.get_stats()).await {
                    Ok(Ok(s)) => (
                        crate::connection_routes::selected_media_routes(
                            &s.publisher_stats,
                            &s.subscriber_stats,
                        ),
                        crate::connection_routes::active_round_trip_time(
                            &s.publisher_stats,
                            &s.subscriber_stats,
                        ),
                    ),
                    _ => (
                        (
                            state.peek().voice_send_route.clone(),
                            state.peek().voice_receive_route.clone(),
                        ),
                        None,
                    ),
                }
            } else {
                ((None, None), None)
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
                if state.peek().voice_ping_ms.is_some() {
                    state.write().voice_ping_ms = None;
                }
                continue;
            }
            let Ok(Ok(stats)) =
                tokio::time::timeout(Duration::from_secs(1), audio.get_stats()).await
            else {
                continue;
            };
            let now = Instant::now();
            let log_audio = now.duration_since(audio_log_at) >= LOSS_LOG_EVERY;
            if log_audio {
                audio_log_at = now;
                for report in &stats {
                    match report {
                        RtcStats::MediaSource(source) if source.source.kind == "audio" => {
                            tracing::info!(
                                level = source.audio.audio_level,
                                energy = source.audio.total_audio_energy,
                                duration = source.audio.total_samples_duration,
                                "voice audio: encoder input"
                            );
                        }
                        RtcStats::OutboundRtp(out) => {
                            tracing::info!(
                                packets = out.sent.packets_sent,
                                bytes = out.sent.bytes_sent,
                                "voice audio: RTP sent"
                            );
                        }
                        _ => {}
                    }
                }
            }
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
            let remote_rtt = microphone_feedback(&stats).and_then(|r| {
                let seconds = r.remote_inbound.round_trip_time;
                (seconds > 0.0)
                    .then(|| Duration::try_from_secs_f64(seconds).ok())
                    .flatten()
            });
            let ping_ms = room_rtt
                .or(remote_rtt)
                .map(|d| (d.as_secs_f64() * 1000.0).round() as u32);
            if state.peek().voice_ping_ms != ping_ms {
                state.write().voice_ping_ms = ping_ms;
            }
            let current_loss = reported_microphone_loss(&stats);
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
            if let Some(out) = out {
                send_csv.row(|| {
                    format!(
                        "{},{},{},{},{},{:.0},{}",
                        out.stream.ssrc,
                        out.sent.packets_sent,
                        out.sent.bytes_sent,
                        out.outbound.retransmitted_packets_sent,
                        out.outbound.nack_count,
                        out.outbound.target_bitrate / 1000.0,
                        upstream_loss.map(|l| format!("{l:.2}")).unwrap_or_default(),
                    )
                });
            }
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
            let display = controls.stats_polling.load(Ordering::Relaxed) || connected;
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
                    live.insert(publication.sid().to_string());
                    if display {
                        next.insert(identity.0.clone(), TrackStats::Unavailable);
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
                recv_csv.row(|| {
                    let i = &inbound.inbound;
                    format!(
                        "{},{},{},{},{},{:.1},{:.3},{:.3},{:.3},{},{},{},{},{},{},{},{:.4},{:.4},{},{},{},{}",
                        crate::identity::truncate_pubkey(&identity.0),
                        inbound.stream.ssrc,
                        inbound.received.packets_received,
                        inbound.received.packets_lost,
                        i.packets_discarded,
                        inbound.received.jitter * 1000.0,
                        i.jitter_buffer_delay,
                        i.jitter_buffer_target_delay,
                        i.jitter_buffer_minimum_delay,
                        i.jitter_buffer_emitted_count,
                        i.total_samples_received,
                        i.concealed_samples,
                        i.silent_concealed_samples,
                        i.concealment_events,
                        i.inserted_samples_for_deceleration,
                        i.removed_samples_for_acceleration,
                        i.audio_level,
                        i.total_audio_energy,
                        i.fec_packets_received,
                        i.fec_packets_discarded,
                        i.nack_count,
                        i.retransmitted_packets_received,
                    )
                });
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
            if display && let Some(pk) = &self_pubkey {
                let rates = out.and_then(|o| {
                    let r = outbound_rates(prev_out, o.sent.packets_sent, o.sent.bytes_sent, now);
                    prev_out = Some((o.sent.packets_sent, o.sent.bytes_sent, now));
                    r
                });
                next.insert(
                    pk.clone(),
                    TrackStats::Outbound {
                        bitrate_kbps: rates.map(|(_, b)| b),
                        packets_per_sec: rates.map(|(p, _)| p),
                        target_kbps: ceiling,
                        loss_pct: current_loss,
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
