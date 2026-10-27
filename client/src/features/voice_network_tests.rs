use super::*;
use livekit::webrtc::stats::{InboundRtpStats, OutboundRtpStats, RemoteInboundRtpStats};

#[test]
fn silent_concealment_is_not_reported_as_lost_speech() {
    let now = Instant::now();
    let mut window = ReceiveWindow::default();
    let mut stats = InboundRtpStats::default();
    window.sample(&stats, now);
    stats.received.packets_received = 50;
    stats.inbound.total_samples_received = 48_000;
    stats.inbound.concealed_samples = 48_000;
    stats.inbound.silent_concealed_samples = 48_000;
    let silent = window.sample(&stats, now + Duration::from_secs(1)).unwrap();
    assert_eq!(silent.concealed_pct, 0.0);
    stats.received.packets_received += 50;
    stats.inbound.total_samples_received += 48_000;
    stats.inbound.concealed_samples += 12_000;
    let speech = window.sample(&stats, now + Duration::from_secs(2)).unwrap();
    assert_eq!(speech.concealed_pct, 25.0);
}

#[test]
fn recent_reception_distinguishes_loss_late_packets_and_concealment() {
    let now = Instant::now();
    let mut window = ReceiveWindow::default();
    let mut stats = InboundRtpStats::default();
    stats.received.packets_received = 750;
    stats.received.packets_lost = 250;
    assert!(window.sample(&stats, now).is_none());
    for second in 1..=7 {
        stats.received.packets_received += 50;
        stats.inbound.total_samples_received += 48_000;
        assert_eq!(
            window
                .sample(&stats, now + Duration::from_secs(second))
                .unwrap()
                .loss_pct,
            0.0
        );
    }
    stats.received.packets_received += 40;
    stats.received.packets_lost += 10;
    stats.inbound.packets_discarded += 5;
    stats.inbound.total_samples_received += 48_000;
    stats.inbound.concealed_samples += 24_000;
    let recent = window.sample(&stats, now + Duration::from_secs(8)).unwrap();
    assert_eq!(recent.loss_pct, 4.0);
    assert_eq!(recent.discarded_packets, 5);
    assert_eq!(recent.concealed_pct, 10.0);
    stats.stream.ssrc = 99;
    assert!(
        window
            .sample(&stats, now + Duration::from_secs(9))
            .is_none()
    );
}

#[test]
fn late_recovery_and_counter_resets_do_not_create_negative_loss() {
    let now = Instant::now();
    let mut window = ReceiveWindow::default();
    let mut stats = InboundRtpStats::default();
    stats.received.packets_received = 100;
    stats.received.packets_lost = 10;
    window.sample(&stats, now);
    stats.received.packets_received = 110;
    stats.received.packets_lost = 5;
    assert_eq!(
        window
            .sample(&stats, now + Duration::from_secs(1))
            .unwrap()
            .loss_pct,
        0.0
    );
    stats.received.packets_received = 2;
    assert!(
        window
            .sample(&stats, now + Duration::from_secs(2))
            .is_none()
    );
    assert!(
        window
            .sample(&stats, now + Duration::from_secs(30))
            .is_none()
    );
}

fn sender_stats(timestamp: i64, packets: u64, loss: f64) -> Vec<RtcStats> {
    let mut out = OutboundRtpStats::default();
    out.rtc.id = "mic".into();
    out.stream.ssrc = 1;
    out.sent.packets_sent = packets;
    let mut remote = RemoteInboundRtpStats::default();
    remote.stream.ssrc = 1;
    remote.rtc.timestamp = timestamp;
    remote.remote_inbound.local_id = "mic".into();
    remote.remote_inbound.fraction_lost = loss;
    vec![
        RtcStats::OutboundRtp(out),
        RtcStats::RemoteInboundRtp(remote),
    ]
}

#[test]
fn upstream_reports_must_be_fresh_match_the_sender_and_have_active_audio() {
    let mut feedback = UplinkFeedback::default();
    assert_eq!(feedback.sample(&sender_stats(1, 50, 0.25)), None);
    assert_eq!(feedback.sample(&sender_stats(2, 100, 0.25)), Some(25.0));
    assert_eq!(feedback.sample(&sender_stats(2, 150, 0.25)), None);
    assert_eq!(feedback.sample(&sender_stats(3, 150, 0.0)), None);
    let mut mismatched = sender_stats(4, 200, 0.25);
    if let RtcStats::RemoteInboundRtp(report) = &mut mismatched[1] {
        report.remote_inbound.local_id = "other-track".into();
    }
    assert_eq!(feedback.sample(&mismatched), None);
    assert_eq!(feedback.sample(&sender_stats(5, 250, 0.5)), Some(50.0));
}

#[test]
fn displayed_loss_requires_valid_feedback_for_the_current_microphone() {
    assert_eq!(reported_microphone_loss(&[]), None);
    assert_eq!(
        reported_microphone_loss(&sender_stats(1, 50, 0.0)),
        Some(0.0)
    );
    assert_eq!(
        reported_microphone_loss(&sender_stats(1, 50, 0.25)),
        Some(25.0)
    );
    for loss in [f64::NAN, f64::INFINITY, -0.1, 1.1] {
        assert_eq!(reported_microphone_loss(&sender_stats(1, 50, loss)), None);
    }
    let mut stats = sender_stats(1, 50, 0.25);
    if let RtcStats::RemoteInboundRtp(report) = &mut stats[1] {
        report.remote_inbound.local_id = "previous-microphone".into();
    }
    assert_eq!(reported_microphone_loss(&stats), None);
    if let RtcStats::RemoteInboundRtp(report) = &mut stats[1] {
        report.remote_inbound.local_id = "mic".into();
        report.stream.ssrc = 2;
    }
    assert_eq!(reported_microphone_loss(&stats), None);
    assert_eq!(reported_microphone_loss(&stats[..1]), None);
}

#[tokio::test]
#[ignore = "starts the bundled SFU; checks Opus FEC is negotiated and a new ceiling applies live"]
async fn a_live_sender_negotiates_fec_and_takes_a_new_ceiling_without_reconnecting() {
    use futures_util::StreamExt;
    use livekit::webrtc::audio_stream::native::NativeAudioStream;
    use livekit_api::access_token::{AccessToken, VideoGrants};

    let credentials = discordia_server::livekit_bundle::Credentials::generate();
    let directory = std::env::temp_dir().join(format!(
        "discordia-voice-resilience-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let _server = discordia_server::livekit_bundle::spawn_livekit(
        discordia_server::livekit_bundle::Advertise::Local,
        &credentials,
        &directory,
    )
    .await
    .unwrap();
    let url = format!(
        "ws://127.0.0.1:{}",
        discordia_server::livekit_bundle::ports().ws
    );
    let token = |identity| {
        AccessToken::with_api_key(&credentials.key, &credentials.secret)
            .with_identity(identity)
            .with_grants(VideoGrants {
                room_join: true,
                room: "resilience".into(),
                ..Default::default()
            })
            .to_jwt()
            .unwrap()
    };
    let (sender, _sender_events) = Room::connect(&url, &token("sender"), RoomOptions::default())
        .await
        .unwrap();
    let sender = Arc::new(sender);
    let (listener, mut listener_events) =
        Room::connect(&url, &token("listener"), RoomOptions::default())
            .await
            .unwrap();
    let source = NativeAudioSource::new(inert_apm_options(), SAMPLE_RATE, CHANNELS, 0);
    let audio = LocalAudioTrack::create_audio_track("mic", RtcAudioSource::Native(source.clone()));
    let publication = sender
        .local_participant()
        .publish_track(
            LocalTrack::Audio(audio.clone()),
            TrackPublishOptions {
                source: TrackSource::Microphone,
                dtx: true,
                audio_encoding: Some(AudioEncoding {
                    max_bitrate: 64_000,
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let frames = Arc::new(AtomicU64::new(0));
    let decoded = frames.clone();
    let listen = tokio::spawn(async move {
        while let Some(event) = listener_events.recv().await {
            if let RoomEvent::TrackSubscribed {
                track: RemoteTrack::Audio(audio),
                ..
            } = event
            {
                let mut stream =
                    NativeAudioStream::new(audio.rtc_track(), SAMPLE_RATE as i32, CHANNELS as i32);
                while stream.next().await.is_some() {
                    decoded.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    });
    let gate_closed = Arc::new(AtomicBool::new(false));
    let closed = gate_closed.clone();
    let pump = tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(10));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut seed = 42_u32;
        let mut lead = activity::LeadIn::new();
        let config = crate::mic_profile::Config::from_state(&AppState::empty());
        let mut output = Vec::new();
        loop {
            tick.tick().await;
            let samples = std::array::from_fn(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                (seed as f32 / u32::MAX as f32 - 0.5) * 0.5
            });
            lead.push(
                crate::audio_queue::Frame::new(samples),
                if closed.load(Ordering::Relaxed) {
                    GateAction::Drop
                } else {
                    GateAction::Pass
                },
                config,
                false,
                &mut output,
            );
            for frame in output.drain(..) {
                let samples = frame.frame.samples.map(|s| (s * i16::MAX as f32) as i16);
                source.capture_frame(&pcm_frame(&samples)).await.unwrap();
            }
        }
    });
    let mut dom = VirtualDom::new(|| rsx! {});
    dom.rebuild_in_place();
    let (controls, task) = dom.in_scope(ScopeId::ROOT, || {
        let mut initial = AppState::empty();
        initial.voice.phase = VoicePhase::Connected;
        initial.voice_bitrate_kbps = 64;
        let controls = AudioControls::from_state(&initial);
        let task = spawn_stats_task(
            Signal::new(initial),
            sender.clone(),
            audio.clone(),
            None,
            controls.clone(),
        );
        (controls, task)
    });
    let started = Instant::now();
    let mut before = None;
    let mut lowered_at = None;
    let mut after = None;
    while started.elapsed() < Duration::from_secs(12) {
        tokio::select! {
            _ = dom.wait_for_work() => { dom.render_immediate_to_vec(); }
            _ = tokio::time::sleep(Duration::from_millis(50)) => {}
        }
        if before.is_none() && started.elapsed() > Duration::from_secs(3) {
            assert_eq!(audio.max_bitrate(), Some(64_000));
            before = Some(frames.load(Ordering::Relaxed));
            controls.bitrate_kbps.store(24, Ordering::Relaxed);
        }
        if lowered_at.is_none() && audio.max_bitrate() == Some(24_000) {
            lowered_at = Some(Instant::now());
        }
        if lowered_at.is_some_and(|at| at.elapsed() > Duration::from_secs(2)) {
            after = Some(frames.load(Ordering::Relaxed));
            break;
        }
    }
    let stats = audio.get_stats().await.unwrap();
    let out = stats
        .iter()
        .find_map(|s| match s {
            RtcStats::OutboundRtp(s) => Some(s.clone()),
            _ => None,
        })
        .unwrap();
    let codec = codec_line(&stats, &out.stream.codec_id);
    println!(
        "codec={codec}, sender target={} bps, frames before={before:?}, after={after:?}",
        out.outbound.target_bitrate
    );
    assert!(
        codec.contains("useinbandfec=1"),
        "Opus FEC was not negotiated: {codec}"
    );
    assert!(lowered_at.is_some(), "the new ceiling was never applied");
    assert!(
        out.outbound.target_bitrate > 0.0 && out.outbound.target_bitrate <= 24_000.0,
        "encoder target did not follow the ceiling: {}",
        out.outbound.target_bitrate
    );
    assert!(
        after.unwrap() > before.unwrap() + 50,
        "decoded audio stalled during the live update"
    );
    assert_eq!(audio.sid(), publication.sid());
    let source_duration = |stats: &[RtcStats]| {
        stats
            .iter()
            .find_map(|s| match s {
                RtcStats::MediaSource(s) if s.source.kind == "audio" => {
                    Some(s.audio.total_samples_duration)
                }
                _ => None,
            })
            .unwrap()
    };
    let before_silence = source_duration(&stats);
    gate_closed.store(true, Ordering::Relaxed);
    tokio::time::sleep(Duration::from_secs(3)).await;
    let silent_stats = audio.get_stats().await.unwrap();
    let silent_packets = silent_stats
        .iter()
        .find_map(|s| match s {
            RtcStats::OutboundRtp(s) => Some(s.sent.packets_sent),
            _ => None,
        })
        .unwrap();
    assert!(
        silent_packets >= out.sent.packets_sent + 3,
        "closing the gate stopped the live RTP clock: {silent_packets} vs {}",
        out.sent.packets_sent
    );
    assert!(
        source_duration(&silent_stats) >= before_silence + 2.5,
        "DTX must reduce packets without stopping the source clock"
    );
    dom.in_scope(ScopeId::ROOT, || task.cancel());
    pump.abort();
    listen.abort();
    listener.close().await.unwrap();
    sender.close().await.unwrap();
}

#[path = "../../../vendor/livekit/src/room/track/network_bitrate.rs"]
mod video_budget;

/// Opus FEC carries the frame before, so only a packet after speech has one;
/// Opus's own voice detector calls a few faded syllable tails silence.
#[test]
fn every_packet_after_speech_carries_fec_before_any_loss_is_reported() {
    let pcm: Vec<i16> = crate::agc::tests::speech(6.0, -20.0, 1)
        .iter()
        .map(|s| (s * i16::MAX as f32) as i16)
        .collect();
    let frames = pcm.as_chunks::<960>().0;
    let after_speech = frames
        .windows(2)
        .filter(|pair| pair[0].iter().any(|s| s.unsigned_abs() > 64))
        .count() as u32;
    for bitrate in [32_000, 64_000, 96_000] {
        let probe = webrtc_sys::voice_audio_encoder::ffi::probe_voice_encoder(&pcm, false, bitrate);
        assert!(probe.voice_encoder);
        assert_eq!(probe.packets as usize, frames.len());
        assert!(
            probe.with_fec * 100 >= after_speech * 97,
            "{bitrate} bps: FEC in {} packets, {after_speech} follow speech",
            probe.with_fec
        );
    }
    let stream = webrtc_sys::voice_audio_encoder::ffi::probe_voice_encoder(&pcm, true, 64_000);
    assert!(
        !stream.voice_encoder,
        "stereo stream audio keeps libwebrtc's encoder"
    );
    assert_eq!(stream.with_fec, 0);
}
