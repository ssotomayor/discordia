use super::*;
use livekit::webrtc::stats::{InboundRtpStats, OutboundRtpStats, RemoteInboundRtpStats};

#[test]
fn loss_profiles_reduce_bandwidth_and_recover_without_exceeding_the_preference() {
    for loss in [5.0, 10.0, 25.0] {
        let start = Instant::now();
        let mut control = AdaptiveBitrate::new(64);
        for second in 0..10 {
            control.update(64, Some(loss), start + Duration::from_secs(second));
        }
        assert_eq!(control.target(), 24, "loss={loss}");
        for second in 10..30 {
            control.update(48, Some(0.0), start + Duration::from_secs(second));
            assert_eq!(control.target(), 24);
        }
        control.update(48, Some(0.0), start + Duration::from_secs(30));
        assert_eq!(control.target(), 32);
        for second in 31..100 {
            control.update(48, Some(0.0), start + Duration::from_secs(second));
        }
        assert_eq!(control.target(), 48);
        control.update(24, None, start + Duration::from_secs(100));
        assert_eq!(control.target(), 24);
    }
}

#[test]
fn isolated_loss_silence_and_missing_feedback_do_not_cause_oscillation() {
    let now = Instant::now();
    let mut control = AdaptiveBitrate::new(64);
    control.update(64, Some(8.0), now);
    control.update(64, Some(0.0), now + Duration::from_secs(1));
    assert_eq!(control.target(), 64);
    control.update(64, Some(25.0), now + Duration::from_secs(2));
    assert_eq!(control.target(), 24);
    control.update(64, Some(0.0), now + Duration::from_secs(3));
    control.update(64, None, now + Duration::from_secs(40));
    control.update(64, Some(0.0), now + Duration::from_secs(41));
    assert_eq!(control.target(), 24);
    for invalid in [f32::NAN, -1.0, 101.0] {
        control.update(64, Some(invalid), now + Duration::from_secs(42));
    }
    assert_eq!(control.target(), 24);
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
    assert_eq!(recent.late_pct, 2.0);
    assert_eq!(recent.concealed_pct, 10.0);
    assert_eq!(recent.pressure(), 6.0);
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

#[test]
fn jitter_alone_does_not_reduce_quality_but_late_packet_bursts_do() {
    let now = Instant::now();
    let mut window = ReceiveWindow::default();
    let mut control = AdaptiveBitrate::new(64);
    let mut stats = InboundRtpStats::default();
    stats.received.jitter = 0.180;
    window.sample(&stats, now);
    for second in 1..=10 {
        stats.received.packets_received += 50;
        if (4..=6).contains(&second) {
            stats.inbound.packets_discarded += 20;
        }
        let at = now + Duration::from_secs(second);
        let recent = window.sample(&stats, at).unwrap();
        control.update(64, Some(recent.pressure()), at);
        if second < 4 {
            assert_eq!(control.target(), 64);
        }
    }
    assert_eq!(control.target(), 24);
    assert_eq!(stats.received.packets_lost, 0);
}

fn report(track: &str, sequence: u64, loss_pct: f32) -> Vec<u8> {
    serde_json::to_vec(&ListenerReport {
        track: track.into(),
        sequence,
        loss_pct,
    })
    .unwrap()
}

#[test]
fn listener_feedback_is_track_scoped_bounded_ordered_and_expires() {
    let now = Instant::now();
    let mut feedback = ListenerFeedback::default();
    feedback.receive("peer", "mic", &report("old-mic", 1, 50.0), now);
    feedback.receive("peer", "mic", &report("mic", 1, 101.0), now);
    feedback.receive("peer", "mic", &[0; 257], now);
    assert_eq!(feedback.recent(now).1, None);
    feedback.receive("peer", "mic", &report("mic", 2, 25.0), now);
    assert_eq!(feedback.recent(now).1, Some(25.0));
    feedback.receive(
        "peer",
        "mic",
        &report("mic", 1, 0.0),
        now + Duration::from_secs(2),
    );
    assert_eq!(feedback.recent(now).1, Some(25.0));
    feedback.receive(
        "peer",
        "mic",
        &report("mic", 3, 0.0),
        now + Duration::from_secs(2),
    );
    assert_eq!(feedback.recent(now).1, Some(0.0));
    assert_eq!(feedback.recent(now + Duration::from_secs(10)).1, None);
    feedback.receive(
        "peer",
        "mic",
        &report("mic", 4, 25.0),
        now + Duration::from_secs(11),
    );
    feedback.remove("peer");
    assert_eq!(feedback.recent(now + Duration::from_secs(11)).1, None);
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
    let now = Instant::now();
    let mut feedback = UplinkFeedback::default();
    assert!(!feedback.sample(&sender_stats(1, 50, 0.25), now).0);
    assert_eq!(
        feedback.sample(&sender_stats(2, 100, 0.25), now),
        (true, true, Some(25.0))
    );
    assert!(!feedback.sample(&sender_stats(2, 150, 0.25), now).0);
    assert!(!feedback.sample(&sender_stats(3, 150, 0.0), now).0);
    let mut mismatched = sender_stats(4, 200, 0.25);
    if let RtcStats::RemoteInboundRtp(report) = &mut mismatched[1] {
        report.remote_inbound.local_id = "other-track".into();
    }
    assert!(!feedback.sample(&mismatched, now).0);
    assert_eq!(
        feedback
            .sample(&sender_stats(3, 150, 0.0), now + Duration::from_secs(8))
            .2,
        None
    );
}

#[tokio::test]
#[ignore = "starts the bundled SFU; measures live bitrate changes and continued decoded audio"]
async fn listener_feedback_reduces_a_live_sender_without_reconnecting() {
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
    let (sender, mut sender_events) = Room::connect(&url, &token("sender"), RoomOptions::default())
        .await
        .unwrap();
    let sender = Arc::new(sender);
    let (listener, mut listener_events) =
        Room::connect(&url, &token("listener"), RoomOptions::default())
            .await
            .unwrap();
    let source = NativeAudioSource::new(inert_apm_options(), SAMPLE_RATE, CHANNELS, 100);
    let audio = LocalAudioTrack::create_audio_track("mic", RtcAudioSource::Native(source.clone()));
    let publication = sender
        .local_participant()
        .publish_track(
            LocalTrack::Audio(audio.clone()),
            TrackPublishOptions {
                source: TrackSource::Microphone,
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
    let pump = tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(10));
        let mut seed = 42_u32;
        loop {
            tick.tick().await;
            let samples: Vec<i16> = (0..FRAME_SAMPLES)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    ((seed as f64 / u32::MAX as f64 - 0.5) * 16_000.0) as i16
                })
                .collect();
            source
                .capture_frame(&AudioFrame {
                    data: std::borrow::Cow::Owned(samples),
                    sample_rate: SAMPLE_RATE,
                    num_channels: CHANNELS,
                    samples_per_channel: FRAME_SAMPLES as u32,
                })
                .await
                .unwrap();
        }
    });
    let mut dom = VirtualDom::new(|| rsx! {});
    dom.rebuild_in_place();
    let feedback = Arc::new(Mutex::new(ListenerFeedback::default()));
    let (state, task) = dom.in_scope(ScopeId::ROOT, || {
        let mut initial = AppState::empty();
        initial.voice.phase = VoicePhase::Connected;
        initial.voice_bitrate_kbps = 64;
        let controls = AudioControls::from_state(&initial);
        let state = Signal::new(initial);
        let task = spawn_stats_task(
            state,
            sender.clone(),
            audio.clone(),
            None,
            controls,
            feedback.clone(),
        );
        (state, task)
    });
    let started = Instant::now();
    let mut sent = false;
    let mut before = None;
    let mut after = None;
    let mut received_feedback = false;
    let mut remote_received = None;
    while started.elapsed() < Duration::from_secs(12) {
        tokio::select! {
            _ = dom.wait_for_work() => { dom.render_immediate_to_vec(); }
            _ = tokio::time::sleep(Duration::from_millis(50)) => {}
            Some(event) = sender_events.recv() => {
                if let RoomEvent::DataReceived { topic, payload, participant: Some(peer), .. } = event
                    && topic.as_deref() == Some(FEEDBACK_TOPIC)
                {
                    feedback.lock().receive(&peer.identity().0, &publication.sid().to_string(), &payload, Instant::now());
                    received_feedback = true;
                }
            }
        }
        if !sent && started.elapsed() > Duration::from_secs(3) {
            assert_eq!(audio.max_bitrate(), Some(64_000));
            before = Some(frames.load(Ordering::Relaxed));
            listener
                .local_participant()
                .publish_data(livekit::DataPacket {
                    payload: report(&publication.sid().to_string(), 1, 25.0),
                    topic: Some(FEEDBACK_TOPIC.into()),
                    destination_identities: vec!["sender".into()],
                    ..Default::default()
                })
                .await
                .unwrap();
            sent = true;
        }
        if state.peek().voice_adaptive_bitrate == Some(24) && after.is_none() {
            after = Some(Instant::now());
        }
        if after.is_some_and(|at| at.elapsed() > Duration::from_secs(2)) {
            remote_received = Some(frames.load(Ordering::Relaxed));
            break;
        }
    }
    let bitrate = audio
        .get_stats()
        .await
        .unwrap()
        .iter()
        .find_map(|s| match s {
            RtcStats::OutboundRtp(s) => Some(s.outbound.target_bitrate),
            _ => None,
        })
        .unwrap();
    println!(
        "feedback delivered={received_feedback}, sender target={bitrate} bps, frames before={before:?}, after={remote_received:?}"
    );
    assert!(received_feedback);
    assert_eq!(state.peek().voice_adaptive_bitrate, Some(24));
    assert_eq!(audio.max_bitrate(), Some(24_000));
    assert!(
        bitrate > 0.0 && bitrate <= 24_000.0,
        "encoder target was not reduced: {bitrate}"
    );
    assert!(
        remote_received.unwrap() > before.unwrap() + 50,
        "decoded audio stalled during the live update"
    );
    assert_eq!(audio.sid(), publication.sid());
    audio.set_max_bitrate(48_000).unwrap();
    assert_eq!(audio.max_bitrate(), Some(48_000));
    dom.in_scope(ScopeId::ROOT, || task.cancel());
    pump.abort();
    listen.abort();
    listener.close().await.unwrap();
    sender.close().await.unwrap();
}

#[path = "../../../vendor/livekit/src/room/track/network_bitrate.rs"]
mod video_budget;
