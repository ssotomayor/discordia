use super::*;

pub(super) struct CameraPublisher {
    room: Arc<Room>,
    sid: TrackSid,
    capture: crate::syscamera::Capture,
    fatal_task: Task,
    pub key: Option<String>,
}

impl CameraPublisher {
    pub async fn connect(
        room: Arc<Room>,
        key: Option<String>,
        mut state: Signal<AppState>,
        mut settings: Signal<crate::settings::ClientSettings>,
        gateway: crate::state::GatewayTx,
    ) -> Result<Self, String> {
        let epoch = state.read().voice_session_epoch;
        let source = NativeVideoSource::new(
            VideoResolution {
                width: 1280,
                height: 720,
            },
            false,
        );
        let (fatal_tx, mut fatal_rx) = unbounded_channel();
        let capture = tokio::select! {
            capture = crate::syscamera::start(key, source.clone(), fatal_tx) => capture?,
            _ = async {
                loop {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    let s = state.read();
                    if s.voice_session_epoch != epoch || !(s.camera_on || s.camera_starting) { break; }
                }
            } => return Err("camera startup was cancelled".into()),
        };
        let track = LocalVideoTrack::create_video_track("camera", RtcVideoSource::Native(source));
        let publication = match room
            .local_participant()
            .publish_track(LocalTrack::Video(track), options())
            .await
        {
            Ok(p) => p,
            Err(e) => {
                capture.stop().await;
                return Err(format!("publishing the camera failed: {e}"));
            }
        };
        // A queued stop or a newer call can supersede startup while the device opens.
        if state.read().voice_session_epoch != epoch
            || !(state.read().camera_starting || state.read().camera_on)
        {
            capture.stop().await;
            if let Err(e) = room
                .local_participant()
                .unpublish_track(&publication.sid())
                .await
            {
                eprintln!("[camera] cancelled publication cleanup failed: {e}");
            }
            return Err("camera startup was cancelled".into());
        }
        let mut next = settings.read().clone();
        next.camera_device_id = Some(capture.device.id.clone());
        next.camera_device_label = Some(capture.device.label.clone());
        settings.set(next.clone());
        crate::settings::save(&next);
        {
            let mut s = state.write();
            s.camera_on = true;
            s.camera_starting = false;
        }
        gateway.send(crate::protocol::ClientMessage::SetCamera { on: true });
        let fatal_task = dioxus::prelude::spawn(async move {
            if let Some(e) = fatal_rx.recv().await {
                let mut s = state.write();
                if s.voice_session_epoch == epoch && s.camera_on {
                    s.camera_on = false;
                    s.camera_starting = false;
                    s.error_toast = Some(format!("Your camera stopped: {e}"));
                    gateway.send(crate::protocol::ClientMessage::SetCamera { on: false });
                }
            }
        });
        let key = Some(capture.device.id.clone());
        Ok(Self {
            room,
            sid: publication.sid(),
            capture,
            fatal_task,
            key,
        })
    }

    pub async fn shutdown(self) {
        self.fatal_task.cancel();
        self.capture.stop().await;
        if let Err(e) = self
            .room
            .local_participant()
            .unpublish_track(&self.sid)
            .await
        {
            eprintln!("[camera] unpublish failed: {e}");
        }
    }
}

pub(super) fn options() -> TrackPublishOptions {
    TrackPublishOptions {
        source: TrackSource::Camera,
        video_codec: VideoCodec::H264,
        video_encoder: VideoEncoderBackend::Hardware,
        simulcast: false,
        video_encoding: Some(VideoEncoding {
            max_framerate: 30.0,
            max_bitrate: 1_200_000,
        }),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use livekit::webrtc::video_frame::I420Buffer;
    use livekit::webrtc::video_stream::native::NativeVideoStream;
    use std::time::Duration;

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "starts a bundled SFU and native camera/screen encoders"]
    async fn camera_and_screen_decode_and_stop_independently() {
        let _subscriber = tracing_subscriber::fmt()
            .with_env_filter("livekit=warn")
            .with_test_writer()
            .try_init();
        use dioxusfun_server::livekit_bundle::{Credentials, ports, spawn_livekit};
        use livekit_api::access_token::{AccessToken, VideoGrants};
        let dir =
            std::env::temp_dir().join(format!("discordia-camera-test-{}", uuid::Uuid::new_v4()));
        let credentials = Credentials::generate();
        let sfu = spawn_livekit(
            dioxusfun_server::livekit_bundle::Advertise::Mapped("127.0.0.1".parse().unwrap()),
            &credentials,
            &dir,
        )
        .await
        .unwrap();
        let name = format!("screen-{}", uuid::Uuid::new_v4());
        let token = |identity| {
            AccessToken::with_api_key(&credentials.key, &credentials.secret)
                .with_identity(identity)
                .with_grants(VideoGrants {
                    room_join: true,
                    room: name.clone(),
                    can_publish: true,
                    can_subscribe: true,
                    ..Default::default()
                })
                .to_jwt()
                .unwrap()
        };
        let encrypted = || {
            let mut options = RoomOptions::default();
            options.encryption = Some(livekit::e2ee::E2eeOptions {
                encryption_type: livekit::e2ee::EncryptionType::Gcm,
                key_provider: livekit::e2ee::key_provider::KeyProvider::with_shared_key(
                    livekit::e2ee::key_provider::KeyProviderOptions::default(),
                    vec![9; crate::mediakey::KEY_LEN],
                ),
            });
            options
        };
        let url = format!("ws://127.0.0.1:{}", ports().ws);
        let (publisher, mut events) = Room::connect(&url, &token("self#video"), encrypted())
            .await
            .unwrap();
        let (listener, mut received) = Room::connect(&url, &token("viewer"), encrypted())
            .await
            .unwrap();
        let drain = tokio::spawn(async move { while events.recv().await.is_some() {} });
        let camera_frames = Arc::new(AtomicU64::new(0));
        let screen_frames = Arc::new(AtomicU64::new(0));
        let counts = [camera_frames.clone(), screen_frames.clone()];
        let decoder = tokio::spawn(async move {
            let mut readers = tokio::task::JoinSet::new();
            while let Some(event) = received.recv().await {
                if let RoomEvent::TrackSubscribed {
                    track: RemoteTrack::Video(video),
                    publication,
                    ..
                } = event
                {
                    let index = match publication.source() {
                        TrackSource::Camera => 0,
                        TrackSource::Screenshare => 1,
                        _ => panic!("unexpected video source"),
                    };
                    let count = counts[index].clone();
                    readers.spawn(async move {
                        let mut stream = NativeVideoStream::new(video.rtc_track());
                        while let Some(frame) = stream.next().await {
                            assert!(frame.buffer.width() > 0 && frame.buffer.height() > 0);
                            count.fetch_add(1, Ordering::Relaxed);
                        }
                    });
                }
            }
        });
        let source = |screencast| {
            NativeVideoSource::new(
                VideoResolution {
                    width: 640,
                    height: 360,
                },
                screencast,
            )
        };
        let camera = source(false);
        let screen = source(true);
        let camera_track =
            LocalVideoTrack::create_video_track("camera", RtcVideoSource::Native(camera.clone()));
        let screen_track =
            LocalVideoTrack::create_video_track("screen", RtcVideoSource::Native(screen.clone()));
        let camera_pub = publisher
            .local_participant()
            .publish_track(LocalTrack::Video(camera_track.clone()), options())
            .await
            .unwrap();
        let mut settings = crate::features::screenshare::native_settings("smooth");
        settings.width = 640;
        settings.height = 360;
        settings.fps = 30;
        let screen_pub = publisher
            .local_participant()
            .publish_track(
                LocalTrack::Video(screen_track.clone()),
                screen_video_options(settings),
            )
            .await
            .unwrap();
        let pump = |sources: Vec<NativeVideoSource>| async move {
            for i in 0..90 {
                for source in &sources {
                    let mut buffer = I420Buffer::new(640, 360);
                    let (y, u, v) = buffer.data_mut();
                    y.fill(16 + i % 200);
                    u.fill(128);
                    v.fill(128);
                    source.capture_frame(&VideoFrame {
                        rotation: VideoRotation::VideoRotation0,
                        timestamp_us: 0,
                        frame_metadata: None,
                        buffer,
                    });
                }
                tokio::time::sleep(Duration::from_millis(33)).await;
            }
        };
        for _ in 0..4 {
            pump(vec![camera.clone(), screen.clone()]).await;
            if camera_frames.load(Ordering::Relaxed) > 10
                && screen_frames.load(Ordering::Relaxed) > 10
            {
                break;
            }
        }
        assert!(
            camera_frames.load(Ordering::Relaxed) > 10,
            "camera must decode"
        );
        assert!(
            screen_frames.load(Ordering::Relaxed) > 10,
            "screen must decode"
        );
        publisher
            .local_participant()
            .unpublish_track(&camera_pub.sid())
            .await
            .unwrap();
        let before = screen_frames.load(Ordering::Relaxed);
        pump(vec![screen.clone()]).await;
        assert!(
            screen_frames.load(Ordering::Relaxed) > before + 10,
            "screen must survive camera stop"
        );
        let camera_track =
            LocalVideoTrack::create_video_track("camera", RtcVideoSource::Native(camera.clone()));
        let camera_pub = publisher
            .local_participant()
            .publish_track(LocalTrack::Video(camera_track), options())
            .await
            .unwrap();
        pump(vec![camera.clone(), screen]).await;
        publisher
            .local_participant()
            .unpublish_track(&screen_pub.sid())
            .await
            .unwrap();
        let before = camera_frames.load(Ordering::Relaxed);
        pump(vec![camera]).await;
        assert!(
            camera_frames.load(Ordering::Relaxed) > before + 10,
            "camera must survive screen stop"
        );
        publisher
            .local_participant()
            .unpublish_track(&camera_pub.sid())
            .await
            .unwrap();
        publisher.close().await.unwrap();
        listener.close().await.unwrap();
        drain.abort();
        decoder.abort();
        drop(sfu);
        for attempt in 0..20 {
            match std::fs::remove_dir_all(&dir) {
                Ok(()) => break,
                Err(e) if attempt == 19 => panic!("SFU cleanup failed: {e}"),
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
        eprintln!("Encrypted native camera and screen decoded; each survived stopping the other.");
    }
}
