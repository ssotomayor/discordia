use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use discordia_server::livekit_bundle::{Credentials, ports, spawn_livekit};
use futures_util::StreamExt;
use livekit::options::TrackPublishOptions;
use livekit::prelude::*;
use livekit::track::{LocalAudioTrack, LocalTrack, LocalVideoTrack, TrackSource};
use livekit::webrtc::audio_source::native::NativeAudioSource;
use livekit::webrtc::prelude::{
    AudioSourceOptions, RtcAudioSource, RtcVideoSource, VideoResolution,
};
use livekit::webrtc::video_frame::{I420Buffer, VideoFrame, VideoRotation};
use livekit::webrtc::video_source::native::NativeVideoSource;
use livekit::webrtc::video_stream::native::NativeVideoStream;
use livekit_api::access_token::{AccessToken, VideoGrants};

#[cfg(target_os = "windows")]
fn process_memory() -> (f64, f64) {
    #[repr(C)]
    struct ProcessMemoryCounters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }
    unsafe extern "system" {
        fn GetCurrentProcess() -> isize;
        fn K32GetProcessMemoryInfo(
            process: isize,
            counters: *mut ProcessMemoryCounters,
            cb: u32,
        ) -> i32;
    }
    unsafe {
        let mut counters: ProcessMemoryCounters = std::mem::zeroed();
        counters.cb = std::mem::size_of::<ProcessMemoryCounters>() as u32;
        if K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) != 0 {
            (
                counters.pagefile_usage as f64 / (1024.0 * 1024.0),
                counters.working_set_size as f64 / (1024.0 * 1024.0),
            )
        } else {
            (0.0, 0.0)
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn process_memory() -> (f64, f64) {
    (0.0, 0.0)
}

#[tokio::test(flavor = "multi_thread")]
async fn ten_multimedia_cycles_stabilize_without_leak() {
    let dir = std::env::temp_dir().join(format!("discordia-mem-test-{}", uuid::Uuid::new_v4()));
    let credentials = Credentials::generate();
    let sfu = spawn_livekit(
        discordia_server::livekit_bundle::Advertise::Mapped("127.0.0.1".parse().unwrap()),
        &credentials,
        &dir,
    )
    .await
    .unwrap();
    let url = format!("ws://127.0.0.1:{}", ports().ws);

    let (base_priv, base_ws) = process_memory();
    println!("\n=== MULTIMEDIA 10-CYCLE MEMORY AUDIT ===");
    println!("Baseline: Private Commit: {base_priv:.2} MB, Working Set: {base_ws:.2} MB");

    let mut samples: Vec<(f64, f64)> = Vec::new();

    for cycle in 1..=10 {
        let room_name = format!("cycle-room-{cycle}");
        let token = |identity: &str| {
            AccessToken::with_api_key(&credentials.key, &credentials.secret)
                .with_identity(identity)
                .with_grants(VideoGrants {
                    room_join: true,
                    room: room_name.clone(),
                    can_publish: true,
                    can_subscribe: true,
                    ..Default::default()
                })
                .to_jwt()
                .unwrap()
        };

        // 1. Connect publisher and listener rooms
        let (pub_room, mut pub_events) =
            Room::connect(&url, &token("publisher"), RoomOptions::default())
                .await
                .unwrap();
        let (sub_room, mut sub_events) =
            Room::connect(&url, &token("subscriber"), RoomOptions::default())
                .await
                .unwrap();

        let drain = tokio::spawn(async move { while pub_events.recv().await.is_some() {} });

        // 2. Publish native audio track
        let audio_source = NativeAudioSource::new(AudioSourceOptions::default(), 48000, 1, 0);
        let audio_track = LocalAudioTrack::create_audio_track(
            "mic",
            RtcAudioSource::Native(audio_source.clone()),
        );
        let audio_pub = pub_room
            .local_participant()
            .publish_track(
                LocalTrack::Audio(audio_track.clone()),
                TrackPublishOptions::default(),
            )
            .await
            .unwrap();

        // 3. Publish native camera track
        let camera_source = NativeVideoSource::new(
            VideoResolution {
                width: 640,
                height: 360,
            },
            false,
        );
        let camera_track = LocalVideoTrack::create_video_track(
            "cam",
            RtcVideoSource::Native(camera_source.clone()),
        );
        let camera_pub = pub_room
            .local_participant()
            .publish_track(
                LocalTrack::Video(camera_track.clone()),
                TrackPublishOptions {
                    source: TrackSource::Camera,
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        // 4. Publish screen share track
        let screen_source = NativeVideoSource::new(
            VideoResolution {
                width: 640,
                height: 360,
            },
            true,
        );
        let screen_track = LocalVideoTrack::create_video_track(
            "scr",
            RtcVideoSource::Native(screen_source.clone()),
        );
        let screen_pub = pub_room
            .local_participant()
            .publish_track(
                LocalTrack::Video(screen_track.clone()),
                TrackPublishOptions {
                    source: TrackSource::Screenshare,
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        // 5. Decode video in subscriber
        let frames_received = Arc::new(AtomicU64::new(0));
        let frames_counter = frames_received.clone();
        let receiver = tokio::spawn(async move {
            let mut readers = tokio::task::JoinSet::new();
            while let Some(event) = sub_events.recv().await {
                if let RoomEvent::TrackSubscribed {
                    track: RemoteTrack::Video(v),
                    ..
                } = event
                {
                    let counter = frames_counter.clone();
                    readers.spawn(async move {
                        let mut stream = NativeVideoStream::new(v.rtc_track());
                        while stream.next().await.is_some() {
                            counter.fetch_add(1, Ordering::Relaxed);
                        }
                    });
                }
            }
        });

        // 6. Push frames for ~100ms
        for _ in 0..5 {
            let mut buf = I420Buffer::new(640, 360);
            let (y, u, v) = buf.data_mut();
            y.fill(128);
            u.fill(128);
            v.fill(128);
            let frame = VideoFrame {
                rotation: VideoRotation::VideoRotation0,
                timestamp_us: 0,
                frame_metadata: None,
                buffer: buf,
            };
            camera_source.capture_frame(&frame);
            screen_source.capture_frame(&frame);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        let (during_priv, during_ws) = process_memory();

        // 7. Unpublish tracks
        let _ = pub_room
            .local_participant()
            .unpublish_track(&audio_pub.sid())
            .await;
        let _ = pub_room
            .local_participant()
            .unpublish_track(&camera_pub.sid())
            .await;
        let _ = pub_room
            .local_participant()
            .unpublish_track(&screen_pub.sid())
            .await;

        // 8. Close rooms and abort tasks
        pub_room.close().await.unwrap();
        sub_room.close().await.unwrap();
        drain.abort();
        receiver.abort();

        let (after_priv, after_ws) = process_memory();
        samples.push((after_priv, after_ws));

        println!(
            "Cycle {cycle:2}: during=({during_priv:.2} MB priv, {during_ws:.2} MB ws), after=({after_priv:.2} MB priv, {after_ws:.2} MB ws)"
        );
    }

    tokio::time::sleep(Duration::from_secs(2)).await;
    let (final_priv, final_ws) = process_memory();
    println!(
        "Stabilized (after 2s): Private Commit: {final_priv:.2} MB, Working Set: {final_ws:.2} MB"
    );

    // Check drift between cycle 3 and cycle 10
    let drift_priv = samples[9].0 - samples[2].0;
    let drift_ws = samples[9].1 - samples[2].1;
    println!(
        "Drift (Cycle 10 vs Cycle 3): Private: {drift_priv:+.2} MB, Working Set: {drift_ws:+.2} MB"
    );

    // Must not grow unboundedly
    assert!(
        drift_priv < 35.0,
        "Private commit must stabilize, observed drift: {drift_priv} MB"
    );

    drop(sfu);
}

#[tokio::test(flavor = "multi_thread")]
async fn ten_abrupt_disconnect_and_reconnect_cycles() {
    let dir = std::env::temp_dir().join(format!("discordia-abrupt-test-{}", uuid::Uuid::new_v4()));
    let credentials = Credentials::generate();
    let sfu = spawn_livekit(
        discordia_server::livekit_bundle::Advertise::Mapped("127.0.0.1".parse().unwrap()),
        &credentials,
        &dir,
    )
    .await
    .unwrap();
    let url = format!("ws://127.0.0.1:{}", ports().ws);

    let (base_priv, base_ws) = process_memory();
    println!("\n=== ABRUPT DISCONNECT & RECONNECT 10-CYCLE AUDIT ===");
    println!("Baseline: Private Commit: {base_priv:.2} MB, Working Set: {base_ws:.2} MB");

    let mut samples: Vec<(f64, f64)> = Vec::new();

    for cycle in 1..=10 {
        let room_name = format!("abrupt-room-{cycle}");
        let token = |identity: &str| {
            AccessToken::with_api_key(&credentials.key, &credentials.secret)
                .with_identity(identity)
                .with_grants(VideoGrants {
                    room_join: true,
                    room: room_name.clone(),
                    can_publish: true,
                    can_subscribe: true,
                    ..Default::default()
                })
                .to_jwt()
                .unwrap()
        };

        // 1. Connect
        let (pub_room, mut pub_events) =
            Room::connect(&url, &token("publisher"), RoomOptions::default())
                .await
                .unwrap();
        let drain = tokio::spawn(async move { while pub_events.recv().await.is_some() {} });

        // 2. Publish audio and video
        let audio_source = NativeAudioSource::new(AudioSourceOptions::default(), 48000, 1, 0);
        let audio_track = LocalAudioTrack::create_audio_track(
            "mic",
            RtcAudioSource::Native(audio_source.clone()),
        );
        let _ = pub_room
            .local_participant()
            .publish_track(
                LocalTrack::Audio(audio_track),
                TrackPublishOptions::default(),
            )
            .await
            .unwrap();

        let camera_source = NativeVideoSource::new(
            VideoResolution {
                width: 640,
                height: 360,
            },
            false,
        );
        let camera_track = LocalVideoTrack::create_video_track(
            "cam",
            RtcVideoSource::Native(camera_source.clone()),
        );
        let _ = pub_room
            .local_participant()
            .publish_track(
                LocalTrack::Video(camera_track),
                TrackPublishOptions::default(),
            )
            .await
            .unwrap();

        let (during_priv, during_ws) = process_memory();

        // 3. Abrupt disconnect handler: ActiveVoice::shutdown calls room.close() under timeout
        let _ = tokio::time::timeout(Duration::from_secs(1), pub_room.close()).await;
        drain.abort();

        let (after_priv, after_ws) = process_memory();
        samples.push((after_priv, after_ws));

        println!(
            "Abrupt Cycle {cycle:2}: during=({during_priv:.2} MB priv, {during_ws:.2} MB ws), after=({after_priv:.2} MB priv, {after_ws:.2} MB ws)"
        );
    }

    tokio::time::sleep(Duration::from_secs(2)).await;
    let (final_priv, final_ws) = process_memory();
    println!(
        "Stabilized (after 2s): Private Commit: {final_priv:.2} MB, Working Set: {final_ws:.2} MB"
    );

    let drift_priv = samples[9].0 - samples[2].0;
    println!("Abrupt Drift (Cycle 10 vs Cycle 3): Private: {drift_priv:+.2} MB");
    assert!(drift_priv < 35.0, "Abrupt reconnect must not leak memory");

    drop(sfu);
}

#[tokio::test(flavor = "multi_thread")]
async fn streaming_isolation_scenarios_and_cycles() {
    let dir = std::env::temp_dir().join(format!("discordia-stream-test-{}", uuid::Uuid::new_v4()));
    let credentials = Credentials::generate();
    let sfu = spawn_livekit(
        discordia_server::livekit_bundle::Advertise::Mapped("127.0.0.1".parse().unwrap()),
        &credentials,
        &dir,
    )
    .await
    .unwrap();
    let url = format!("ws://127.0.0.1:{}", ports().ws);

    let (base_priv, base_ws) = process_memory();
    println!("\n=======================================================");
    println!("=== STREAMING MEMORY AUDIT: 4 ISOLATED SCENARIOS ===");
    println!("=======================================================");
    println!("0. Baseline (Initial process): Private={base_priv:.2} MB, WS={base_ws:.2} MB");

    let token = |room: &str, identity: &str| {
        AccessToken::with_api_key(&credentials.key, &credentials.secret)
            .with_identity(identity)
            .with_grants(VideoGrants {
                room_join: true,
                room: room.to_string(),
                can_publish: true,
                can_subscribe: true,
                ..Default::default()
            })
            .to_jwt()
            .unwrap()
    };

    let room_id = "isolation-room";
    let (room_local, mut events_local) = Room::connect(
        &url,
        &token(room_id, "local-client"),
        RoomOptions::default(),
    )
    .await
    .unwrap();
    let (room_remote, mut events_remote) =
        Room::connect(&url, &token(room_id, "remote-peer"), RoomOptions::default())
            .await
            .unwrap();

    let drain_remote = tokio::spawn(async move { while events_remote.recv().await.is_some() {} });

    // Scenario 1: Reposo (Audio connected, idle)
    let audio_source = NativeAudioSource::new(AudioSourceOptions::default(), 48000, 1, 0);
    let audio_track =
        LocalAudioTrack::create_audio_track("mic", RtcAudioSource::Native(audio_source.clone()));
    let audio_pub = room_local
        .local_participant()
        .publish_track(
            LocalTrack::Audio(audio_track.clone()),
            TrackPublishOptions::default(),
        )
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;
    let (reposo_priv, reposo_ws) = process_memory();
    println!(
        "1. Reposo (Audio idle): Private={reposo_priv:.2} MB, WS={reposo_ws:.2} MB (Delta vs Base: {:+.2} MB)",
        reposo_priv - base_priv
    );

    // Scenario 2: Transmitiendo unicamente (Screen share Tx only)
    let tx_source = NativeVideoSource::new(
        VideoResolution {
            width: 1280,
            height: 720,
        },
        true,
    );
    let tx_track =
        LocalVideoTrack::create_video_track("screen-tx", RtcVideoSource::Native(tx_source.clone()));
    let tx_pub = room_local
        .local_participant()
        .publish_track(
            LocalTrack::Video(tx_track.clone()),
            TrackPublishOptions {
                source: TrackSource::Screenshare,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    // Pump 5 frames of 720p video
    for _ in 0..5 {
        let mut buf = I420Buffer::new(1280, 720);
        let (y, u, v) = buf.data_mut();
        y.fill(160);
        u.fill(128);
        v.fill(128);
        let frame = VideoFrame {
            rotation: VideoRotation::VideoRotation0,
            timestamp_us: 0,
            frame_metadata: None,
            buffer: buf,
        };
        tx_source.capture_frame(&frame);
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let (tx_priv, tx_ws) = process_memory();
    println!(
        "2. Transmitiendo (Tx 720p only): Private={tx_priv:.2} MB, WS={tx_ws:.2} MB (Delta vs Reposo: {:+.2} MB)",
        tx_priv - reposo_priv
    );

    // Stop Tx
    let _ = room_local
        .local_participant()
        .unpublish_track(&tx_pub.sid())
        .await;

    // Scenario 3: Recibiendo unicamente (Screen share Rx only)
    let remote_source = NativeVideoSource::new(
        VideoResolution {
            width: 1280,
            height: 720,
        },
        true,
    );
    let remote_track = LocalVideoTrack::create_video_track(
        "remote-screen",
        RtcVideoSource::Native(remote_source.clone()),
    );
    let remote_pub = room_remote
        .local_participant()
        .publish_track(
            LocalTrack::Video(remote_track.clone()),
            TrackPublishOptions {
                source: TrackSource::Screenshare,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let rx_frames = Arc::new(AtomicU64::new(0));
    let rx_counter = rx_frames.clone();
    let rx_join = tokio::spawn(async move {
        while let Some(event) = events_local.recv().await {
            if let RoomEvent::TrackSubscribed {
                track: RemoteTrack::Video(v),
                ..
            } = event
            {
                let cnt = rx_counter.clone();
                tokio::spawn(async move {
                    let mut stream = NativeVideoStream::new(v.rtc_track());
                    while stream.next().await.is_some() {
                        cnt.fetch_add(1, Ordering::Relaxed);
                    }
                });
            }
        }
    });

    for _ in 0..5 {
        let mut buf = I420Buffer::new(1280, 720);
        let (y, u, v) = buf.data_mut();
        y.fill(200);
        u.fill(128);
        v.fill(128);
        let frame = VideoFrame {
            rotation: VideoRotation::VideoRotation0,
            timestamp_us: 0,
            frame_metadata: None,
            buffer: buf,
        };
        remote_source.capture_frame(&frame);
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let (rx_priv, rx_ws) = process_memory();
    println!(
        "3. Recibiendo (Rx 720p only): Private={rx_priv:.2} MB, WS={rx_ws:.2} MB (Delta vs Reposo: {:+.2} MB)",
        rx_priv - reposo_priv
    );

    // Scenario 4: Transmitiendo + Recibiendo simultaneamente (Bidireccional)
    let bidi_tx_pub = room_local
        .local_participant()
        .publish_track(
            LocalTrack::Video(tx_track.clone()),
            TrackPublishOptions {
                source: TrackSource::Screenshare,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    for _ in 0..5 {
        let mut buf = I420Buffer::new(1280, 720);
        let (y, u, v) = buf.data_mut();
        y.fill(100);
        u.fill(128);
        v.fill(128);
        let frame = VideoFrame {
            rotation: VideoRotation::VideoRotation0,
            timestamp_us: 0,
            frame_metadata: None,
            buffer: buf,
        };
        tx_source.capture_frame(&frame);
        remote_source.capture_frame(&frame);
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let (bidi_priv, bidi_ws) = process_memory();
    println!(
        "4. Transmitiendo + Recibiendo (Bidireccional Tx+Rx): Private={bidi_priv:.2} MB, WS={bidi_ws:.2} MB (Delta vs Reposo: {:+.2} MB)",
        bidi_priv - reposo_priv
    );

    // Teardown: Stop both transmissions
    let _ = room_local
        .local_participant()
        .unpublish_track(&bidi_tx_pub.sid())
        .await;
    let _ = room_remote
        .local_participant()
        .unpublish_track(&remote_pub.sid())
        .await;

    let (stopped_priv, stopped_ws) = process_memory();
    println!(
        "5a. Tras detener transmisiones (inmediato T+0s): Private={stopped_priv:.2} MB, WS={stopped_ws:.2} MB"
    );

    tokio::time::sleep(Duration::from_secs(2)).await;
    let (stabilized_priv, stabilized_ws) = process_memory();
    println!(
        "5b. Estabilizado (T+2s): Private={stabilized_priv:.2} MB, WS={stabilized_ws:.2} MB (Retencion residual vs Reposo: {:+.2} MB)",
        stabilized_priv - reposo_priv
    );

    rx_join.abort();
    drain_remote.abort();
    let _ = room_local
        .local_participant()
        .unpublish_track(&audio_pub.sid())
        .await;
    room_local.close().await.unwrap();
    room_remote.close().await.unwrap();

    println!("\n==========================================================================");
    println!("=== 10 CICLOS BIDIRECCIONALES CON MEDICION DE ESTABILIZACION POR CICLO ===");
    println!("==========================================================================");

    #[allow(dead_code)]
    struct CycleRecord {
        during: (f64, f64),
        t0: (f64, f64),
        t2: (f64, f64),
        t5: (f64, f64),
    }

    let mut cycle_records: Vec<CycleRecord> = Vec::new();

    for c in 1..=10 {
        let cycle_room = format!("bidi-cycle-room-{c}");
        let (loc_rm, mut loc_ev) =
            Room::connect(&url, &token(&cycle_room, "loc"), RoomOptions::default())
                .await
                .unwrap();
        let (rem_rm, mut rem_ev) =
            Room::connect(&url, &token(&cycle_room, "rem"), RoomOptions::default())
                .await
                .unwrap();

        let drn_rem = tokio::spawn(async move { while rem_ev.recv().await.is_some() {} });

        let loc_src = NativeVideoSource::new(
            VideoResolution {
                width: 1280,
                height: 720,
            },
            true,
        );
        let loc_trk =
            LocalVideoTrack::create_video_track("loc-scr", RtcVideoSource::Native(loc_src.clone()));
        let loc_pub = loc_rm
            .local_participant()
            .publish_track(
                LocalTrack::Video(loc_trk),
                TrackPublishOptions {
                    source: TrackSource::Screenshare,
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let rem_src = NativeVideoSource::new(
            VideoResolution {
                width: 1280,
                height: 720,
            },
            true,
        );
        let rem_trk =
            LocalVideoTrack::create_video_track("rem-scr", RtcVideoSource::Native(rem_src.clone()));
        let rem_pub = rem_rm
            .local_participant()
            .publish_track(
                LocalTrack::Video(rem_trk),
                TrackPublishOptions {
                    source: TrackSource::Screenshare,
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let rx_counter_c = Arc::new(AtomicU64::new(0));
        let rx_c = rx_counter_c.clone();
        let drn_loc = tokio::spawn(async move {
            while let Some(event) = loc_ev.recv().await {
                if let RoomEvent::TrackSubscribed {
                    track: RemoteTrack::Video(v),
                    ..
                } = event
                {
                    let cnt = rx_c.clone();
                    tokio::spawn(async move {
                        let mut stream = NativeVideoStream::new(v.rtc_track());
                        while stream.next().await.is_some() {
                            cnt.fetch_add(1, Ordering::Relaxed);
                        }
                    });
                }
            }
        });

        for _ in 0..5 {
            let mut buf = I420Buffer::new(1280, 720);
            let (y, u, v) = buf.data_mut();
            y.fill(120);
            u.fill(128);
            v.fill(128);
            let frame = VideoFrame {
                rotation: VideoRotation::VideoRotation0,
                timestamp_us: 0,
                frame_metadata: None,
                buffer: buf,
            };
            loc_src.capture_frame(&frame);
            rem_src.capture_frame(&frame);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        let during = process_memory();

        let _ = loc_rm
            .local_participant()
            .unpublish_track(&loc_pub.sid())
            .await;
        let _ = rem_rm
            .local_participant()
            .unpublish_track(&rem_pub.sid())
            .await;
        loc_rm.close().await.unwrap();
        rem_rm.close().await.unwrap();
        drn_loc.abort();
        drn_rem.abort();

        let t0 = process_memory();
        tokio::time::sleep(Duration::from_secs(2)).await;
        let t2 = process_memory();
        tokio::time::sleep(Duration::from_secs(3)).await;
        let t5 = process_memory();

        println!(
            "Ciclo {c:2}: Durante=({:.2} MB priv, {:.2} MB ws) | T+0s=({:.2} MB priv) | T+2s=({:.2} MB priv) | T+5s=({:.2} MB priv, {:.2} MB ws)",
            during.0, during.1, t0.0, t2.0, t5.0, t5.1
        );

        cycle_records.push(CycleRecord { during, t0, t2, t5 });
    }

    let drift_t2 = cycle_records[9].t2.0 - cycle_records[0].t2.0;
    let drift_t5 = cycle_records[9].t5.0 - cycle_records[0].t5.0;
    println!("\n=== RESUMEN DE ESTABILIZACION Y DERIVA ===");
    println!(
        "Ciclo  1 estabilizado (T+5s): {:.2} MB priv, {:.2} MB ws",
        cycle_records[0].t5.0, cycle_records[0].t5.1
    );
    println!(
        "Ciclo 10 estabilizado (T+5s): {:.2} MB priv, {:.2} MB ws",
        cycle_records[9].t5.0, cycle_records[9].t5.1
    );
    println!("Deriva neta T+2s (Ciclo 10 vs Ciclo 1): {drift_t2:+.2} MB");
    println!("Deriva neta T+5s (Ciclo 10 vs Ciclo 1): {drift_t5:+.2} MB");

    assert!(
        drift_t5.abs() < 25.0,
        "La memoria estabilizada debe formar una meseta sin crecimiento acumulativo: deriva={drift_t5} MB"
    );

    drop(sfu);
}
