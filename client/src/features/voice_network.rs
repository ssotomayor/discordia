use super::*;
use std::collections::VecDeque;
use std::time::Duration;

pub(super) const FEEDBACK_TOPIC: &str = "discordia.voice-quality.v1";
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

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ListenerReport {
    track: String,
    sequence: u64,
    loss_pct: f32,
}

#[derive(Default)]
pub(super) struct ListenerFeedback {
    reports: HashMap<String, (u64, f32, Instant)>,
    revision: u64,
}

impl ListenerFeedback {
    pub fn receive(&mut self, identity: &str, track: &str, payload: &[u8], now: Instant) {
        if payload.len() > 256 {
            return;
        }
        let Ok(report) = serde_json::from_slice::<ListenerReport>(payload) else {
            return;
        };
        if report.track != track
            || !report.loss_pct.is_finite()
            || !(0.0..=100.0).contains(&report.loss_pct)
        {
            return;
        }
        if let Some((sequence, _, at)) = self.reports.get(identity)
            && (report.sequence <= *sequence || now.duration_since(*at) < Duration::from_secs(1))
        {
            return;
        }
        if self.reports.len() >= 256 && !self.reports.contains_key(identity) {
            return;
        }
        self.reports
            .insert(identity.into(), (report.sequence, report.loss_pct, now));
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn remove(&mut self, identity: &str) {
        self.reports.remove(identity);
    }

    fn recent(&mut self, now: Instant) -> (u64, Option<f32>) {
        self.reports
            .retain(|_, (_, _, at)| now.duration_since(*at) <= FEEDBACK_TTL);
        (
            self.revision,
            self.reports
                .values()
                .map(|(_, loss, _)| *loss)
                .reduce(f32::max),
        )
    }
}

pub(super) struct AdaptiveBitrate {
    target: u32,
    healthy_since: Option<Instant>,
    last_sample: Option<Instant>,
    last_reduction: Option<Instant>,
    bad_reports: u8,
}

impl AdaptiveBitrate {
    pub fn new(ceiling: u32) -> Self {
        Self {
            target: ceiling,
            healthy_since: None,
            last_sample: None,
            last_reduction: None,
            bad_reports: 0,
        }
    }
    pub fn target(&self) -> u32 {
        self.target
    }

    pub fn update(&mut self, ceiling: u32, loss: Option<f32>, now: Instant) {
        self.target = self.target.min(ceiling);
        if self
            .last_sample
            .is_some_and(|last| now.duration_since(last) > FEEDBACK_TTL)
        {
            self.healthy_since = None;
            self.bad_reports = 0;
        }
        let Some(loss) = loss.filter(|v| v.is_finite() && (0.0..=100.0).contains(v)) else {
            return;
        };
        self.last_sample = Some(now);
        if loss >= 5.0 {
            self.healthy_since = None;
            self.bad_reports = self.bad_reports.saturating_add(1);
            if loss >= 15.0
                || (self.bad_reports >= 2
                    && self
                        .last_reduction
                        .is_none_or(|at| now.duration_since(at) >= Duration::from_secs(5)))
            {
                self.target = if loss >= 15.0 { 24 } else { self.target / 2 }
                    .max(24)
                    .min(ceiling);
                self.last_reduction = Some(now);
                self.bad_reports = 0;
            }
        } else {
            self.bad_reports = 0;
            if loss > 1.0 {
                self.healthy_since = None;
            } else if now.duration_since(*self.healthy_since.get_or_insert(now))
                >= Duration::from_secs(20)
            {
                self.target = (self.target + 8).min(ceiling);
                self.healthy_since = Some(now);
            }
        }
    }
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
    pub late_pct: f32,
    pub concealed_pct: f32,
}

impl RecentReception {
    fn pressure(&self) -> f32 {
        (self.loss_pct + self.late_pct).min(100.0)
    }
}

#[derive(Default)]
pub(super) struct ReceiveWindow {
    stream: Option<(String, u32)>,
    samples: VecDeque<Reception>,
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
            late_pct: (100.0 * (current.discarded - previous.discarded) as f32 / total as f32)
                .min(100.0),
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

#[derive(Default)]
struct UplinkFeedback {
    last_report: Option<(u32, i64)>,
    last_packets: Option<(u32, u64)>,
    recent: Option<(f32, Instant)>,
}

impl UplinkFeedback {
    fn sample(&mut self, stats: &[RtcStats], now: Instant) -> (bool, bool, Option<f32>) {
        let Some(out) = stats.iter().find_map(|s| match s {
            RtcStats::OutboundRtp(s) => Some(s),
            _ => None,
        }) else {
            return (false, false, None);
        };
        let sent = (out.stream.ssrc, out.sent.packets_sent);
        let previous = self.last_packets.replace(sent);
        if previous.is_some_and(|(ssrc, packets)| ssrc != sent.0 || sent.1 < packets) {
            self.recent = None;
            self.last_report = None;
        }
        let active = previous
            .is_some_and(|(ssrc, packets)| ssrc == sent.0 && sent.1.saturating_sub(packets) >= 20);
        let mut fresh = false;
        if let Some(report) = stats.iter().find_map(|s| match s {
            RtcStats::RemoteInboundRtp(s)
                if s.remote_inbound.local_id == out.rtc.id && s.stream.ssrc == out.stream.ssrc =>
            {
                Some(s)
            }
            _ => None,
        }) {
            let key = (report.stream.ssrc, report.rtc.timestamp);
            if self
                .last_report
                .is_none_or(|last| key.0 != last.0 || key.1 > last.1)
            {
                self.last_report = Some(key);
                let loss = report.remote_inbound.fraction_lost;
                if active && loss.is_finite() && (0.0..=1.0).contains(&loss) {
                    self.recent = Some((loss as f32 * 100.0, now));
                    fresh = true;
                }
            }
        }
        let loss = self
            .recent
            .filter(|(_, at)| now.duration_since(*at) <= FEEDBACK_TTL)
            .map(|(loss, _)| loss);
        (fresh, active, loss)
    }
}

pub(super) fn spawn_stats_task(
    mut state: Signal<AppState>,
    room: Arc<Room>,
    audio: LocalAudioTrack,
    self_pubkey: Option<String>,
    controls: AudioControls,
    feedback: Arc<Mutex<ListenerFeedback>>,
) -> Task {
    spawn(async move {
        let mut windows = HashMap::<String, ReceiveWindow>::new();
        let mut uplink = UplinkFeedback::default();
        let mut adaptive = AdaptiveBitrate::new(controls.bitrate_kbps.load(Ordering::Relaxed));
        let mut applied = None;
        let mut applied_ssrc = None;
        let mut listener_revision = 0;
        let mut sequence = 0_u64;
        let mut prev_out = None;
        let mut last_feedback_sent = Instant::now();
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
                feedback.lock().reports.clear();
                if state.peek().voice_adaptive_bitrate.is_some()
                    || state.peek().voice_uplink_congested
                {
                    let mut s = state.write();
                    s.voice_adaptive_bitrate = None;
                    s.voice_uplink_congested = false;
                }
                continue;
            }
            let Ok(Ok(stats)) =
                tokio::time::timeout(Duration::from_secs(1), audio.get_stats()).await
            else {
                continue;
            };
            let now = Instant::now();
            let ssrc = stats.iter().find_map(|s| match s {
                RtcStats::OutboundRtp(s) => Some(s.stream.ssrc),
                _ => None,
            });
            if ssrc != applied_ssrc {
                applied = None;
                applied_ssrc = ssrc;
            }
            let (fresh, active, upstream_loss) = uplink.sample(&stats, now);
            let (revision, listener_loss) = feedback.lock().recent(now);
            let loss = match (upstream_loss, listener_loss) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (a, b) => a.or(b),
            };
            let ceiling = controls.bitrate_kbps.load(Ordering::Relaxed);
            adaptive.update(
                ceiling,
                if active && (fresh || revision != listener_revision) {
                    loss
                } else {
                    None
                },
                now,
            );
            listener_revision = revision;
            if fresh && upstream_loss.is_some_and(|v| v >= 5.0) {
                latest_uplink_problem = Some(now);
            }
            let uplink_congested = latest_uplink_problem
                .is_some_and(|at| now.duration_since(at) < Duration::from_secs(20));
            if applied != Some(adaptive.target()) {
                match audio.set_max_bitrate(u64::from(adaptive.target()) * 1000) {
                    Ok(()) => {
                        applied = Some(adaptive.target());
                        tracing::info!(
                            kbps = adaptive.target(),
                            ceiling,
                            ?loss,
                            "voice bandwidth adjusted"
                        );
                    }
                    Err(error) => tracing::warn!(%error, "could not adjust voice bandwidth"),
                }
            }
            if state.peek().voice_adaptive_bitrate != applied
                || state.peek().voice_uplink_congested != uplink_congested
            {
                let mut s = state.write();
                s.voice_adaptive_bitrate = applied;
                s.voice_uplink_congested = uplink_congested;
            }
            let display = controls.stats_polling.load(Ordering::Relaxed);
            let send_feedback = now.duration_since(last_feedback_sent) >= Duration::from_secs(2);
            let mut next = HashMap::new();
            let mut live = HashSet::new();
            let mut reports = Vec::new();
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
                let recent = windows.entry(sid.clone()).or_default().sample(inbound, now);
                if send_feedback
                    && !state.peek().voice.deafened
                    && state.peek().voice_gain_of(&identity.0) > 0.0
                    && let Some(recent) = recent
                {
                    sequence = sequence.wrapping_add(1);
                    let report = ListenerReport {
                        track: sid,
                        sequence,
                        loss_pct: recent.pressure(),
                    };
                    if let Ok(payload) = serde_json::to_vec(&report) {
                        reports.push((identity.clone(), payload));
                    }
                }
                if display {
                    let mut view = inbound_stats(inbound);
                    if let TrackStats::Inbound {
                        recent_loss_pct,
                        late_pct,
                        concealed_pct,
                        ..
                    } = &mut view
                    {
                        *recent_loss_pct = recent.map(|r| r.loss_pct);
                        *late_pct = recent.map(|r| r.late_pct);
                        *concealed_pct = recent.map(|r| r.concealed_pct);
                    }
                    next.insert(identity.0.clone(), view);
                }
            }
            futures_util::stream::iter(reports)
                .for_each_concurrent(8, |(identity, payload)| {
                    let room = &room;
                    async move {
                        let local = room.local_participant();
                        let send = local.publish_data(livekit::DataPacket {
                            payload,
                            topic: Some(FEEDBACK_TOPIC.into()),
                            destination_identities: vec![identity],
                            ..Default::default()
                        });
                        if let Ok(Err(error)) =
                            tokio::time::timeout(Duration::from_millis(250), send).await
                        {
                            tracing::debug!(%error, "voice quality feedback unavailable");
                        }
                    }
                })
                .await;
            windows.retain(|sid, _| live.contains(sid));
            if send_feedback {
                last_feedback_sent = now;
            }
            if display
                && let Some(pk) = &self_pubkey
                && let Some(out) = stats.iter().find_map(|s| match s {
                    RtcStats::OutboundRtp(s) => Some(s),
                    _ => None,
                })
            {
                let rates =
                    outbound_rates(prev_out, out.sent.packets_sent, out.sent.bytes_sent, now);
                prev_out = Some((out.sent.packets_sent, out.sent.bytes_sent, now));
                next.insert(
                    pk.clone(),
                    TrackStats::Outbound {
                        bitrate_kbps: rates.map(|(_, b)| b),
                        packets_per_sec: rates.map(|(p, _)| p),
                        target_kbps: applied.unwrap_or(ceiling),
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
