use std::sync::Arc;
use std::time::{Duration, Instant};

use df::tract::{DfParams, DfTract, RuntimeParams};
use livekit::options::TrackPublishOptions;
use livekit::prelude::*;
use livekit::track::{LocalAudioTrack, LocalTrack, TrackSource};
use livekit::webrtc::audio_source::native::NativeAudioSource;
use livekit::webrtc::prelude::{AudioSourceOptions, RtcAudioSource};
use livekit_api::access_token::{AccessToken, VideoGrants};

const SAMPLE_RATE: u32 = 48_000;
const CHANNELS: u32 = 1;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.into())
}

fn token(identity: &str, room: &str, can_publish: bool) -> String {
    AccessToken::with_api_key(
        &env_or("LIVEKIT_API_KEY", "devkey"),
        &env_or(
            "LIVEKIT_API_SECRET",
            "secret-must-be-at-least-32-chars-long",
        ),
    )
    .with_identity(identity)
    .with_name(identity)
    .with_grants(VideoGrants {
        room_join: true,
        room: room.to_string(),
        can_publish,
        can_subscribe: true,
        can_publish_data: can_publish,
        ..Default::default()
    })
    .to_jwt()
    .expect("mint")
}

async fn publish_silence(room: &Room, source: TrackSource) -> Result<(), String> {
    let src = NativeAudioSource::new(AudioSourceOptions::default(), SAMPLE_RATE, CHANNELS, 1000);
    let track = LocalAudioTrack::create_audio_track("probe", RtcAudioSource::Native(src));
    room.local_participant()
        .publish_track(
            LocalTrack::Audio(track),
            TrackPublishOptions {
                source,
                ..Default::default()
            },
        )
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

async fn wait_for(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<RoomEvent>,
    label: &str,
    pred: impl Fn(&RoomEvent) -> bool,
) -> RoomEvent {
    let deadline = Duration::from_secs(15);
    tokio::time::timeout(deadline, async {
        loop {
            let ev = events.recv().await.expect("event stream closed");
            println!("[{label}] {ev:?}");
            if pred(&ev) {
                return ev;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("[{label}] timed out after {deadline:?}"))
}

#[tokio::test]
#[ignore = "needs a running LiveKit server; see the module docs"]
async fn subscribe_only_identity_still_receives_audio() {
    let url = env_or("LIVEKIT_URL", "ws://127.0.0.1:7880");
    let room_name = format!("screen-{}", uuid::Uuid::new_v4());
    let pubkey = "a".repeat(64);

    let (sharer, mut sharer_events) = Room::connect(
        &url,
        &token(&pubkey, &room_name, true),
        RoomOptions::default(),
    )
    .await
    .expect("sharer connects");

    let (listener, mut listener_events) = Room::connect(
        &url,
        &token(&format!("{pubkey}#audio"), &room_name, false),
        RoomOptions::default(),
    )
    .await
    .expect("the subscribe-only identity must still be allowed to connect");

    println!("both peers connected to {room_name}");

    publish_silence(&sharer, TrackSource::Screenshare)
        .await
        .expect("sharer publishes");

    wait_for(&mut listener_events, "listener", |ev| {
        matches!(ev, RoomEvent::TrackSubscribed { .. })
    })
    .await;

    let refused = publish_silence(&listener, TrackSource::Microphone).await;
    assert!(
        refused.is_err(),
        "the SFU accepted a publish from an identity minted without publish rights"
    );
    println!(
        "publish did not succeed, as required: {}",
        refused.unwrap_err()
    );

    let _ = sharer_events.recv().await;
    listener.close().await.ok();
    sharer.close().await.ok();
}

const FRAME: usize = 480;
const WARMUP_HOPS: usize = 100;
const TONE_HZ: f32 = 440.0;
const AMPLITUDE: f32 = 0.5;

fn tone_purity(samples: &[f32], hz: f32) -> f32 {
    const WINDOW: usize = 4800;
    if samples.len() < WINDOW {
        return goertzel_ratio(samples, hz);
    }
    let mut acc = 0.0f32;
    let mut n = 0usize;
    for chunk in samples.chunks_exact(WINDOW) {
        acc += goertzel_ratio(chunk, hz);
        n += 1;
    }
    acc / n as f32
}

fn goertzel_ratio(samples: &[f32], hz: f32) -> f32 {
    let n = samples.len() as f32;
    let k = (n * hz / SAMPLE_RATE as f32).round();
    let w = 2.0 * std::f32::consts::PI * k / n;
    let (cw, sw) = (w.cos(), w.sin());
    let coeff = 2.0 * cw;
    let (mut s1, mut s2) = (0.0f32, 0.0f32);
    let mut total = 0.0f32;
    for &x in samples {
        let s0 = x + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
        total += x * x;
    }
    let re = s1 - s2 * cw;
    let im = s2 * sw;
    let at_hz = 2.0 * (re * re + im * im) / n;
    if total <= f32::EPSILON {
        0.0
    } else {
        (at_hz / total).clamp(0.0, 1.0)
    }
}

fn band_energy(samples: &[f32], hz: f32, half_width: f32) -> f32 {
    const WINDOW: usize = 4800; // 10 Hz bins
    if samples.len() < WINDOW {
        return goertzel_ratio(samples, hz);
    }
    let step = SAMPLE_RATE as f32 / WINDOW as f32;
    let mut acc = 0.0f32;
    let mut n = 0usize;
    for chunk in samples.chunks_exact(WINDOW) {
        let mut f = hz - half_width;
        let mut sum = 0.0f32;
        while f <= hz + half_width {
            sum += goertzel_ratio(chunk, f);
            f += step;
        }
        acc += sum.min(1.0);
        n += 1;
    }
    acc / n as f32
}

fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

fn tone_samples(n: usize, amplitude: f32) -> Vec<f32> {
    let step = 2.0 * std::f32::consts::PI * TONE_HZ / SAMPLE_RATE as f32;
    let mut phase = 0.0f32;
    (0..n)
        .map(|_| {
            let v = phase.sin() * amplitude;
            phase = (phase + step) % (2.0 * std::f32::consts::PI);
            v
        })
        .collect()
}

fn spawn_tone(
    source: NativeAudioSource,
    noise: f32,
    amplitude: f32,
    denoise: Option<f32>,
) -> Feeder {
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    let rt = tokio::runtime::Handle::current();
    let handle = tokio::task::spawn_blocking(move || {
        let mut phase = 0.0f32;
        let step = 2.0 * std::f32::consts::PI * TONE_HZ / SAMPLE_RATE as f32;
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut model = denoise.map(|db| {
            let params = RuntimeParams::default_with_ch(1).with_atten_lim(db);
            DfTract::new(DfParams::default(), &params).expect("load DeepFilterNet")
        });
        let mut noisy = ndarray::Array2::<f32>::zeros((1, FRAME));
        let mut enh = ndarray::Array2::<f32>::zeros((1, FRAME));
        let mut applied = Applied::default();
        while !flag.load(std::sync::atomic::Ordering::Relaxed) {
            let mut hop: Vec<f32> = (0..FRAME)
                .map(|_| {
                    let mut v = phase.sin() * amplitude;
                    phase = (phase + step) % (2.0 * std::f32::consts::PI);
                    if noise > 0.0 {
                        seed = seed
                            .wrapping_mul(6364136223846793005)
                            .wrapping_add(1442695040888963407);
                        let u = (seed >> 40) as f32 / (1u32 << 23) as f32 - 1.0;
                        v += u * noise;
                    }
                    v.clamp(-1.0, 1.0)
                })
                .collect();

            if let Some(m) = model.as_mut() {
                let before = rms(&hop);
                noisy
                    .as_slice_mut()
                    .expect("contiguous")
                    .copy_from_slice(&hop);
                m.process(noisy.view(), enh.view_mut())
                    .expect("denoise hop");
                hop.copy_from_slice(enh.as_slice().expect("contiguous"));
                let after = rms(&hop);
                if applied.skipped < WARMUP_HOPS {
                    applied.skipped += 1;
                } else if before > 0.0 {
                    applied.energy_in += (before as f64) * (before as f64);
                    applied.energy_out += (after as f64) * (after as f64);
                    applied.hops += 1;
                }
            }

            let data: Vec<i16> = hop
                .iter()
                .map(|v| (v.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
                .collect();
            let frame = livekit::webrtc::audio_frame::AudioFrame {
                data: data.into(),
                sample_rate: SAMPLE_RATE,
                num_channels: CHANNELS,
                samples_per_channel: FRAME as u32,
            };
            if rt.block_on(source.capture_frame(&frame)).is_err() {
                return applied;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        applied
    });
    Feeder {
        handle: Some(handle),
        stop,
    }
}

struct Feeder {
    handle: Option<tokio::task::JoinHandle<Applied>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for Feeder {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

impl Feeder {
    async fn stop(mut self) -> Applied {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let handle = self.handle.take().expect("stop is called once");
        match handle.await {
            Ok(applied) => applied,
            Err(e) => panic!("the feeder died before the measurement finished: {e}"),
        }
    }
}

#[derive(Default)]
struct Applied {
    energy_in: f64,
    energy_out: f64,
    hops: usize,
    skipped: usize,
}

impl Applied {
    fn mean_db(&self) -> Option<f32> {
        (self.hops > 0).then(|| 10.0 * (self.energy_out / self.energy_in).log10() as f32)
    }
}

#[derive(Clone, Copy)]
struct Config {
    apm: bool,
    red: bool,
    dtx: bool,
    max_bitrate: Option<u64>,
    noise: f32,
    talker_key: Option<&'static str>,
    listener_key: Option<&'static str>,
    agc: Option<bool>,
    amplitude: f32,
    denoise: Option<f32>,
}

fn room_options(key: Option<&str>) -> RoomOptions {
    let mut opts = RoomOptions::default();
    if let Some(key) = key {
        let provider = livekit::e2ee::key_provider::KeyProvider::with_shared_key(
            livekit::e2ee::key_provider::KeyProviderOptions::default(),
            key.as_bytes().to_vec(),
        );
        opts.encryption = Some(livekit::e2ee::E2eeOptions {
            encryption_type: livekit::e2ee::EncryptionType::Gcm,
            key_provider: provider,
        });
    }
    opts
}

impl Config {
    fn transport_only() -> Self {
        Self {
            apm: false,
            red: true,
            dtx: true,
            max_bitrate: None,
            noise: 0.0,
            talker_key: None,
            listener_key: None,
            agc: None,
            amplitude: AMPLITUDE,
            denoise: None,
        }
    }

    fn quiet_with_agc(amplitude: f32, agc: bool) -> Self {
        Self {
            amplitude,
            agc: Some(agc),
            ..Self::transport_only()
        }
    }

    fn encrypted(talker: &'static str, listener: &'static str) -> Self {
        Self {
            talker_key: Some(talker),
            listener_key: Some(listener),
            ..Self::transport_only()
        }
    }
}

struct Metrics {
    frames: usize,
    samples: usize,
    rate: f32,
    r_in: f32,
    r_out: f32,
    db: f32,
    purity_in: f32,
    purity_out: f32,
    band_in: f32,
    band_out: f32,
    h2: f32,
    h3: f32,
    applied_db: Option<f32>,
}

async fn measure_round_trip(cfg: Config) -> Metrics {
    use futures_util::StreamExt as _;
    use livekit::webrtc::audio_stream::native::NativeAudioStream;

    let url = env_or("LIVEKIT_URL", "ws://127.0.0.1:7880");
    let room_name = format!("voice-{}", uuid::Uuid::new_v4());

    let (talker, _t_events) = Room::connect(
        &url,
        &token("talker", &room_name, true),
        room_options(cfg.talker_key),
    )
    .await
    .expect("talker connects");
    let (listener, mut listener_events) = Room::connect(
        &url,
        &token("listener", &room_name, true),
        room_options(cfg.listener_key),
    )
    .await
    .expect("listener connects");

    let opts = AudioSourceOptions {
        echo_cancellation: cfg.apm,
        noise_suppression: cfg.apm,
        auto_gain_control: cfg.agc.unwrap_or(cfg.apm),
    };
    let source = NativeAudioSource::new(opts, SAMPLE_RATE, CHANNELS, 1000);
    let track = LocalAudioTrack::create_audio_track("tone", RtcAudioSource::Native(source.clone()));
    talker
        .local_participant()
        .publish_track(
            LocalTrack::Audio(track),
            TrackPublishOptions {
                source: TrackSource::Microphone,
                red: cfg.red,
                dtx: cfg.dtx,
                audio_encoding: cfg
                    .max_bitrate
                    .map(|max_bitrate| livekit::options::AudioEncoding { max_bitrate }),
                ..Default::default()
            },
        )
        .await
        .expect("publish the tone");
    let feeder = spawn_tone(source, cfg.noise, cfg.amplitude, cfg.denoise);

    let ev = wait_for(&mut listener_events, "listener", |ev| {
        matches!(ev, RoomEvent::TrackSubscribed { .. })
    })
    .await;
    let RoomEvent::TrackSubscribed { track, .. } = ev else {
        unreachable!()
    };
    let RemoteTrack::Audio(audio) = track else {
        panic!("expected an audio track")
    };
    let mut stream = NativeAudioStream::new(audio.rtc_track(), SAMPLE_RATE as i32, CHANNELS as i32);

    const WARMUP: Duration = Duration::from_secs(1);
    const TOTAL: Duration = Duration::from_secs(5);
    let started = Instant::now();
    let mut received: Vec<f32> = Vec::new();
    let mut frames = 0usize;
    while started.elapsed() < TOTAL {
        let Ok(Some(f)) = tokio::time::timeout(Duration::from_secs(2), stream.next()).await else {
            break;
        };
        frames += 1;
        if started.elapsed() > WARMUP {
            received.extend(f.data.iter().map(|s| *s as f32 / i16::MAX as f32));
        }
    }
    let window = started.elapsed().saturating_sub(WARMUP).as_secs_f32();
    let applied = feeder.stop().await;

    let sent = tone_samples(received.len(), cfg.amplitude);
    let (r_in, r_out) = (rms(&sent), rms(&received));
    let m = Metrics {
        frames,
        samples: received.len(),
        rate: received.len() as f32 / CHANNELS as f32 / window,
        r_in,
        r_out,
        db: 20.0 * (r_out / r_in).log10(),
        purity_in: tone_purity(&sent, TONE_HZ),
        purity_out: tone_purity(&received, TONE_HZ),
        band_in: band_energy(&sent, TONE_HZ, 40.0),
        band_out: band_energy(&received, TONE_HZ, 40.0),
        h2: tone_purity(&received, TONE_HZ * 2.0),
        h3: tone_purity(&received, TONE_HZ * 3.0),
        applied_db: applied.mean_db(),
    };

    listener.close().await.ok();
    talker.close().await.ok();
    m
}

#[tokio::test]
#[ignore = "needs a running LiveKit server; see the module docs"]
async fn a_tone_survives_the_round_trip() {
    let m = measure_round_trip(Config::transport_only()).await;

    assert!(
        m.samples > SAMPLE_RATE as usize,
        "only {} samples in the steady-state window",
        m.samples
    );

    report("APM off (baseline)", &m);

    assert!(
        (m.rate - SAMPLE_RATE as f32).abs() < SAMPLE_RATE as f32 * 0.05,
        "effective rate {:.0} Hz is more than 5% off nominal",
        m.rate
    );
    assert!(
        m.db.abs() < 3.0,
        "level moved by {:+.2} dB end to end",
        m.db
    );
    assert!(
        m.band_out > 0.80,
        "only {:.1}% of the received energy is within 40 Hz of {TONE_HZ} — the \
         signal is not coming back as the tone that went in",
        m.band_out * 100.0
    );
    assert!(
        m.h2 < 0.02,
        "energy showed up at the second harmonic — something in the path is \
         distorting rather than merely coding"
    );
}

#[tokio::test]
#[ignore = "needs a running LiveKit server; see the module docs"]
async fn media_encryption_carries_audio_only_when_the_keys_agree() {
    let agreed = measure_round_trip(Config::encrypted(
        "a-shared-passphrase",
        "a-shared-passphrase",
    ))
    .await;
    report("E2EE, keys agree", &agreed);

    assert!(
        agreed.samples > SAMPLE_RATE as usize,
        "only {} samples came back with matching keys — encryption stopped the \
         audio rather than merely protecting it",
        agreed.samples
    );
    assert!(
        agreed.band_out > 0.80,
        "only {:.1}% of the received energy is the tone, with keys that match",
        agreed.band_out * 100.0
    );
    assert!(
        agreed.db.abs() < 3.0,
        "level moved by {:+.2} dB with encryption on",
        agreed.db
    );

    let split = measure_round_trip(Config::encrypted("one-passphrase", "another-passphrase")).await;
    report("E2EE, keys differ", &split);

    assert!(
        split.r_out < agreed.r_out * 0.01,
        "audio came through at {:.4} RMS with mismatched keys, against {:.4} \
         with matching ones — either the keys are not being applied or the \
         frames are not encrypted at all",
        split.r_out,
        agreed.r_out
    );
    assert!(
        split.band_out < 0.20,
        "{:.1}% of the received energy was still the tone with mismatched keys",
        split.band_out * 100.0
    );
}

#[tokio::test]
#[ignore = "needs a running LiveKit server; see the module docs"]
async fn agc_is_measured_against_the_assumption_the_gate_default_rests_on() {
    const QUIET: f32 = 0.04;

    let off = measure_round_trip(Config::quiet_with_agc(QUIET, false)).await;
    report("quiet tone, AGC off", &off);
    let on = measure_round_trip(Config::quiet_with_agc(QUIET, true)).await;
    report("quiet tone, AGC on", &on);

    println!("--- AGC ---");
    println!("level off / on     : {:+.2} dB / {:+.2} dB", off.db, on.db);
    println!("difference         : {:+.2} dB", on.db - off.db);

    assert!(
        off.samples > SAMPLE_RATE as usize && on.samples > SAMPLE_RATE as usize,
        "a run delivered too little to measure: {} off, {} on",
        off.samples,
        on.samples
    );
    assert!(
        off.band_out > 0.50 && on.band_out > 0.50,
        "the tone did not survive the trip at this level ({:.2} off, {:.2} on) — \
         measure the path before reading anything into the gain",
        off.band_out,
        on.band_out
    );

    let gain = on.db - off.db;
    assert!(
        gain.abs() < 1.0,
        "AGC moved the level by {gain:+.2} dB, having measured inert at ±0.03 dB. \
         If it now works, entry 63 in docs/OPEN.md is resting on a \
         different world than the one it was written in — read it before \
         widening this bound"
    );
}

fn report(label: &str, m: &Metrics) {
    println!("--- {label} ---");
    println!("frames decoded     : {}", m.frames);
    println!("samples analysed   : {}", m.samples);
    println!(
        "effective rate     : {:.0} Hz/ch (nominal {SAMPLE_RATE})",
        m.rate
    );
    println!(
        "RMS in / out       : {:.4} / {:.4}  ({:+.2} dB)",
        m.r_in, m.r_out, m.db
    );
    println!(
        "tone purity in/out : {:.4} / {:.4}",
        m.purity_in, m.purity_out
    );
    println!("energy in ±40 Hz   : {:.4} / {:.4}", m.band_in, m.band_out);
    println!(
        "harmonics out      : 880 Hz {:.4}, 1320 Hz {:.4}",
        m.h2, m.h3
    );
    if let Some(db) = m.applied_db {
        println!("DeepFilterNet      : {db:+.2} dB applied per hop, mean");
    }
}

#[tokio::test]
#[ignore = "needs a running LiveKit server; see the module docs"]
async fn the_knobs_that_shape_voice_quality_are_measured() {
    let base = Config::transport_only();
    let rows: Vec<(&str, Config)> = vec![
        ("APM off (baseline)", base),
        ("APM on (mic-realistic)", Config { apm: true, ..base }),
        ("red off", Config { red: false, ..base }),
        ("dtx off", Config { dtx: false, ..base }),
        (
            "bitrate 16 kbps",
            Config {
                max_bitrate: Some(16_000),
                ..base
            },
        ),
        (
            "bitrate 64 kbps",
            Config {
                max_bitrate: Some(64_000),
                ..base
            },
        ),
        (
            "noise 0.15, APM off",
            Config {
                noise: 0.15,
                ..base
            },
        ),
        (
            "noise 0.15, APM on",
            Config {
                noise: 0.15,
                apm: true,
                ..base
            },
        ),
        (
            "noise 0.15, DFN 30 dB",
            Config {
                noise: 0.15,
                denoise: Some(30.0),
                ..base
            },
        ),
        (
            "noise 0.15, DFN 12 dB",
            Config {
                noise: 0.15,
                denoise: Some(12.0),
                ..base
            },
        ),
        (
            "noise 0.15, DFN 100 dB",
            Config {
                noise: 0.15,
                denoise: Some(100.0),
                ..base
            },
        ),
    ];

    let mut measured = Vec::new();
    for (label, cfg) in rows {
        let m = measure_round_trip(cfg).await;
        report(label, &m);
        assert!(
            m.samples > SAMPLE_RATE as usize,
            "{label}: only {} samples — this configuration stopped delivering \
             audio, which is not a quality trade-off but a broken path",
            m.samples
        );
        measured.push((label, m));
    }

    println!();
    println!(
        "{:<24} {:>9} {:>10} {:>10} {:>9} {:>11}",
        "configuration", "level dB", "band ±40", "purity", "h2", "DFN applied"
    );
    for (label, m) in &measured {
        let applied = match m.applied_db {
            Some(db) => format!("{db:+.2} dB"),
            None => "—".to_string(),
        };
        println!(
            "{label:<24} {:>+9.2} {:>10.4} {:>10.4} {:>9.4} {applied:>11}",
            m.db, m.band_out, m.purity_out, m.h2
        );
    }
}
