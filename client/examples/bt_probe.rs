//! Bisects a Bluetooth headset stuck in HFP after a call. Watch the headset's
//! format in Audio MIDI Setup at each prompt.
//!
//!     cargo run -p dioxusfun --example bt_probe            # cpal alone
//!     cargo run -p dioxusfun --example bt_probe -- livekit # plus LiveKit's runtime
//!     cargo run -p dioxusfun --example bt_probe -- room    # a real room, on a LiveKit it starts itself
//!
//! `room` spawns the bundled livekit-server on loopback with throwaway keys,
//! so nothing else needs to run. `RUST_LOG=libwebrtc=debug` shows libwebrtc's
//! own device log lines.

use std::sync::mpsc;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use livekit::e2ee::key_provider::{KeyProvider, KeyProviderOptions};
use livekit::e2ee::{E2eeOptions, EncryptionType};
use livekit::options::TrackPublishOptions;
use livekit::prelude::*;
use livekit::webrtc::audio_frame::AudioFrame;
use livekit::webrtc::audio_source::AudioSourceOptions;
use livekit::webrtc::audio_source::native::NativeAudioSource;
use livekit::webrtc::prelude::RtcAudioSource;
use livekit_api::access_token::{AccessToken, VideoGrants};

async fn pause(secs: u64, what: &str) {
    eprintln!("--- {what} — watching for {secs}s");
    for left in (1..=secs).rev() {
        if left % 5 == 0 {
            eprintln!("    {left}s");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// The default input, and optionally the default output, on their own
/// thread: a cpal stream must not cross one.
fn open_streams(with_output: bool) -> (mpsc::Sender<()>, std::thread::JoinHandle<()>) {
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let (ready_tx, ready_rx) = mpsc::channel::<()>();
    let handle = std::thread::spawn(move || {
        let host = cpal::default_host();
        let input = host
            .default_input_device()
            .expect("no default input device");
        eprintln!("input device: {}", input.name().unwrap_or_default());
        let cfg = input
            .default_input_config()
            .expect("no default input config");
        eprintln!(
            "input format: {:?} {} Hz",
            cfg.sample_format(),
            cfg.sample_rate().0
        );
        let mic = input
            .build_input_stream(
                &cfg.into(),
                |_data: &[f32], _| {},
                |e| eprintln!("mic error: {e}"),
                None,
            )
            .expect("input stream");
        mic.play().expect("play mic");

        let speaker = with_output.then(|| {
            let output = host
                .default_output_device()
                .expect("no default output device");
            eprintln!("output device: {}", output.name().unwrap_or_default());
            let cfg = output
                .default_output_config()
                .expect("no default output config");
            let s = output
                .build_output_stream(
                    &cfg.into(),
                    |data: &mut [f32], _| data.fill(0.0),
                    |e| eprintln!("speaker error: {e}"),
                    None,
                )
                .expect("output stream");
            s.play().expect("play speaker");
            s
        });

        let _ = ready_tx.send(());
        let _ = stop_rx.recv();
        drop(mic);
        drop(speaker);
        eprintln!("cpal streams dropped");
    });
    let _ = ready_rx.recv();
    (stop_tx, handle)
}

async fn cpal_only() {
    let (stop, handle) = open_streams(false);
    pause(5, "microphone OPEN: format should read 16 or 24 kHz now").await;
    let _ = stop.send(());
    let _ = handle.join();
    pause(25, "microphone CLOSED: does the format return to 44.1/48?").await;
}

async fn with_runtime() {
    eprintln!("creating LiveKit's runtime (peer connection factory + audio device module)");
    let runtime = livekit::rtc_engine::lk_runtime::LkRuntime::instance();
    pause(
        10,
        "runtime alive, microphone NOT open yet: format should still be 44.1/48 kHz",
    )
    .await;
    let (stop, handle) = open_streams(false);
    pause(5, "microphone OPEN: format should read 16 or 24 kHz now").await;
    let _ = stop.send(());
    let _ = handle.join();
    pause(
        25,
        "microphone CLOSED, runtime alive: does the format return?",
    )
    .await;
    drop(runtime);
    pause(20, "LiveKit runtime DROPPED: does the format return now?").await;
}

/// The bundled LiveKit on loopback with keys made up for this run; the
/// handle kills it on drop.
fn token_for(creds: &dioxusfun_server::livekit_bundle::Credentials, identity: &str) -> String {
    AccessToken::with_api_key(&creds.key, &creds.secret)
        .with_identity(identity)
        .with_name(identity)
        .with_grants(VideoGrants {
            room_join: true,
            room: "bt-probe".into(),
            can_publish: true,
            can_subscribe: true,
            ..Default::default()
        })
        .to_jwt()
        .expect("token")
}

/// The app's rooms are end-to-end encrypted with one shared key.
fn e2ee() -> Option<E2eeOptions> {
    Some(E2eeOptions {
        encryption_type: EncryptionType::Gcm,
        key_provider: KeyProvider::with_shared_key(KeyProviderOptions::default(), vec![7u8; 32]),
    })
}

async fn start_livekit() -> (
    String,
    dioxusfun_server::livekit_bundle::Credentials,
    dioxusfun_server::livekit_bundle::LivekitSubprocess,
) {
    use dioxusfun_server::livekit_bundle::{Credentials, ports, spawn_livekit};
    let creds = Credentials::generate();
    let data_dir = std::env::temp_dir().join(format!("bt-probe-{}", std::process::id()));
    std::fs::create_dir_all(&data_dir).expect("temp dir");
    eprintln!("starting the bundled livekit-server on loopback…");
    let child = spawn_livekit(None, &creds, &data_dir)
        .await
        .unwrap_or_else(|e| panic!("livekit: {e} (built with LIVEKIT_BUNDLE_SKIP?)"));
    let url = format!("ws://127.0.0.1:{}", ports().ws);
    (url, creds, child)
}

async fn in_a_room() {
    let (url, creds, livekit) = start_livekit().await;
    let token = token_for(&creds, "bt-probe");

    eprintln!("connecting to {url}");
    let mut options = RoomOptions::default();
    options.encryption = e2ee();
    let (room, mut events) = Room::connect(&url, &token, options)
        .await
        .unwrap_or_else(|e| panic!("room connect to {url}: {e}"));
    let events_task = tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            let name = format!("{event:?}");
            eprintln!(
                "room event: {}",
                name.split([' ', '(', '{']).next().unwrap_or("")
            );
        }
    });

    let source = NativeAudioSource::new(AudioSourceOptions::default(), 48_000, 1, 1000);
    let track = LocalAudioTrack::create_audio_track("mic", RtcAudioSource::Native(source.clone()));
    room.local_participant()
        .publish_track(
            LocalTrack::Audio(track),
            TrackPublishOptions {
                source: TrackSource::Microphone,
                ..Default::default()
            },
        )
        .await
        .expect("publish");
    eprintln!("mic track published");

    // The app also sits in the room a second time, as the screen-audio
    // subscriber, with auto-subscribe off.
    let mut sub_options = RoomOptions::default();
    sub_options.auto_subscribe = false;
    sub_options.encryption = e2ee();
    let (sub_room, mut sub_events) =
        Room::connect(&url, &token_for(&creds, "bt-probe#audio"), sub_options)
            .await
            .unwrap_or_else(|e| panic!("subscriber room connect: {e}"));
    let sub_events_task = tokio::spawn(async move { while sub_events.recv().await.is_some() {} });
    eprintln!("second (subscriber) room joined");
    let feeder = tokio::spawn({
        let source = source.clone();
        async move {
            let mut tick = tokio::time::interval(Duration::from_millis(10));
            loop {
                tick.tick().await;
                let frame = AudioFrame {
                    data: vec![0i16; 480].into(),
                    sample_rate: 48_000,
                    num_channels: 1,
                    samples_per_channel: 480,
                };
                if source.capture_frame(&frame).await.is_err() {
                    break;
                }
            }
        }
    });

    let (stop, handle) = open_streams(true);
    pause(
        8,
        "IN THE ROOM, mic and speaker open like the app: 16 or 24 kHz expected",
    )
    .await;

    let _ = stop.send(());
    let _ = handle.join();
    pause(
        15,
        "cpal streams DROPPED, room still connected: does the format return?",
    )
    .await;

    feeder.abort();
    if let Err(e) = sub_room.close().await {
        eprintln!("subscriber room close: {e}");
    }
    drop(sub_room);
    sub_events_task.abort();
    if let Err(e) = room.close().await {
        eprintln!("room close: {e}");
    }
    drop(room);
    drop(source);
    events_task.abort();
    // LiveKit keeps a renegotiation task alive ~10 s after close; the runtime
    // can only drop after it, so watch past that.
    pause(
        35,
        "room CLOSED and dropped: does the format return now? (LkRuntime::drop() should print within ~12s)",
    )
    .await;
    drop(livekit);
}

#[tokio::main]
async fn main() {
    use tracing_subscriber::EnvFilter;
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("warn,livekit=debug")),
        )
        .with_target(true)
        .try_init();

    eprintln!(
        "before starting: quit the app and confirm the headset reads 44.1/48 kHz in Audio MIDI Setup"
    );
    match std::env::args().nth(1).as_deref() {
        Some("livekit") => with_runtime().await,
        Some("room") => in_a_room().await,
        _ => cpal_only().await,
    }
    eprintln!("done");
}
