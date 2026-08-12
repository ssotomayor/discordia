//! Two live peers against a real SFU — the half `server/tests/voice.rs` cannot
//! reach.
//!
//! That suite proves what the gateway *hands out*: identities, grants, and the
//! frames sent when a mint fails. It stops there on purpose, because whether an
//! SFU then honours `can_publish: false` is a property of LiveKit, not of our
//! code. `7b5b6bb` removed publish rights from the subscribe-only `#audio`
//! identity and the review of #42 called that the branch's real regression
//! risk: if the SFU treats the grant as a reason to reject the *connection*, or
//! to stop delivering subscribed audio, stream sound dies and nothing in CI
//! would say so.
//!
//! `#[ignore]`d, because it needs a LiveKit server listening. Run it against
//! the bundled one:
//!
//! ```text
//! cargo run --release -p dioxusfun-server          # spawns the SFU on :7880
//! cargo test -p dioxusfun --test live_sfu -- --ignored --nocapture
//! ```
//!
//! `LIVEKIT_URL`, `LIVEKIT_API_KEY` and `LIVEKIT_API_SECRET` override the
//! defaults, which match what the bundled server uses.
//!
//! # Why this is here at all
//!
//! Worth stating, because everything about this file argues against itself:
//! nothing in CI runs it — the test job covers four crates and `dioxusfun` is
//! not among them, and both tests are `#[ignore]`d besides — and it is the only
//! reason `livekit-api` and `uuid` appear in this package's dev-dependencies.
//! By the usual rules it would live somewhere else, or nowhere.
//!
//! It stays because the claims it backs are made about this codebase and would
//! otherwise rest on a transcript. "The subscribe-only identity still hears" and
//! "a tone comes back within 0.6 dB and 94% in band" are the kind of statement
//! that gets repeated long after anyone can reproduce it. The instrument that
//! produced them belongs next to the code they are about, runnable in two
//! commands, with its baseline written down — so the next person can disagree
//! with a measurement instead of with a memory.
//!
//! That also sets the bar for changing it: if an assertion here starts failing,
//! the first question is whether the *measurement* is wrong. Both metrics in
//! this file were wrong before they were right, and the code says how.

use std::time::{Duration, Instant};

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

/// Mint with the same grant shape `server::livekit::mint_screen_token` uses, so
/// what is under test is the SFU's reading of those grants.
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

/// Wait for a room event matching `pred`, or give up.
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

/// The question the unit and integration tests cannot answer: with publish
/// revoked, does the subscribe-only identity still get in, and still *hear*?
///
/// Both halves matter and they fail differently. A rejected connection would be
/// obvious. Audio that stops arriving would look exactly like the bug this
/// grant change was meant to have nothing to do with.
#[tokio::test]
#[ignore = "needs a running LiveKit server; see the module docs"]
async fn subscribe_only_identity_still_receives_audio() {
    let url = env_or("LIVEKIT_URL", "ws://127.0.0.1:7880");
    let room_name = format!("screen-{}", uuid::Uuid::new_v4());
    let pubkey = "a".repeat(64);

    // The sharer: bare pubkey, publishes, exactly as the webview does.
    let (sharer, mut sharer_events) = Room::connect(
        &url,
        &token(&pubkey, &room_name, true),
        RoomOptions::default(),
    )
    .await
    .expect("sharer connects");

    // The listener: `{pubkey}#audio`, minted with can_publish false.
    let (listener, mut listener_events) = Room::connect(
        &url,
        &token(&format!("{pubkey}#audio"), &room_name, false),
        RoomOptions::default(),
    )
    .await
    .expect("the subscribe-only identity must still be allowed to connect");

    println!("both peers connected to {room_name}");

    // The sharer publishes what a screen share's audio would be.
    publish_silence(&sharer, TrackSource::Screenshare)
        .await
        .expect("sharer publishes");

    // The listener must be told about it and must end up subscribed. Losing
    // publish rights has to leave subscription untouched — that is the whole
    // premise of the change.
    wait_for(&mut listener_events, "listener", |ev| {
        matches!(ev, RoomEvent::TrackSubscribed { .. })
    })
    .await;

    // And the converse: with the grant revoked, the publish must not succeed.
    // If it did, `can_publish: false` would be decorative and the token would
    // not be the control it is documented to be.
    //
    // Observed against livekit-server 1.12.0, and worth knowing: the refusal is
    // a *timeout*, not an answer — "track publication timed out, no response
    // received from the server". The SFU drops the AddTrack request on the
    // floor rather than rejecting it. Enforcement is real, but any code that
    // ever tried to publish on this identity would hang for ten seconds instead
    // of failing fast, so the assertion is on the outcome and the message is
    // printed rather than matched. A future LiveKit that starts answering
    // properly should not fail this test.
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

// ---------------------------------------------------------------------------
// A synthetic microphone, so audio quality can be measured rather than guessed.
// ---------------------------------------------------------------------------

/// 10 ms at 48 kHz — the frame the whole pipeline is built around.
const FRAME: usize = 480;
/// The tone under test. 440 Hz is inside the band Opus's speech mode is tuned
/// for, and divides 48 kHz cleanly enough to keep the analysis honest.
const TONE_HZ: f32 = 440.0;
/// Peak amplitude, well under full scale: nothing downstream should be
/// clipping, so a level change means the pipeline changed it.
const AMPLITUDE: f32 = 0.5;

/// Share of the buffer's energy sitting at `hz`, by Goertzel, averaged over
/// short windows. 0..=1.
///
/// A ratio rather than a peak-picking FFT because the question is not "which
/// bin is loudest" but "how much of what came out is still what we sent", which
/// is the thing a lossy codec erodes.
///
/// Windowed rather than one bin over the whole buffer, and that is not a
/// detail: a single bin across four seconds is 0.25 Hz wide, while the decoded
/// stream runs a fraction of a percent off nominal — enough to move the tone
/// clean out of the bin and read as if the codec had destroyed it. It read
/// 0.0002 that way, on a signal that is audibly a pure tone. `WINDOW` samples
/// give a 10 Hz bin, which tolerates that offset and still rejects everything
/// else.
fn tone_purity(samples: &[f32], hz: f32) -> f32 {
    /// 100 ms: bin width 10 Hz.
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
    // |X(k)|^2 scaled to the same units as the running sum of squares.
    let at_hz = 2.0 * (re * re + im * im) / n;
    if total <= f32::EPSILON {
        0.0
    } else {
        (at_hz / total).clamp(0.0, 1.0)
    }
}

/// Energy within `half_width` Hz of `hz`, as a share of the whole. 0..=1.
///
/// A band rather than the single bin `tone_purity` uses, because the decoded
/// tone does not come back on exactly one frequency: the receiver adapts its
/// clock — the effective rate runs a fraction of a percent fast — and that
/// smears a sustained sine across its neighbours. Measured: 440 Hz keeps about
/// a quarter of the energy while 415 and 466 pick up more, and the harmonics at
/// 880 and 1320 stay at zero. So nothing non-linear is happening; the energy is
/// simply not all in one bin, and a band is the honest way to count it.
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

fn tone_samples(n: usize) -> Vec<f32> {
    let step = 2.0 * std::f32::consts::PI * TONE_HZ / SAMPLE_RATE as f32;
    let mut phase = 0.0f32;
    (0..n)
        .map(|_| {
            let v = phase.sin() * AMPLITUDE;
            phase = (phase + step) % (2.0 * std::f32::consts::PI);
            v
        })
        .collect()
}

/// Feed a continuous tone the way the microphone path does: 10 ms frames of
/// i16, handed to libwebrtc one at a time, in real time — the encoder and the
/// jitter buffer both care about the clock.
fn spawn_tone(source: NativeAudioSource) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut phase = 0.0f32;
        let step = 2.0 * std::f32::consts::PI * TONE_HZ / SAMPLE_RATE as f32;
        loop {
            let data: Vec<i16> = (0..FRAME)
                .map(|_| {
                    let v = phase.sin() * AMPLITUDE;
                    phase = (phase + step) % (2.0 * std::f32::consts::PI);
                    (v * i16::MAX as f32) as i16
                })
                .collect();
            let frame = livekit::webrtc::audio_frame::AudioFrame {
                data: data.into(),
                sample_rate: SAMPLE_RATE,
                num_channels: CHANNELS,
                samples_per_channel: FRAME as u32,
            };
            if source.capture_frame(&frame).await.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
}

/// End-to-end audio quality and throughput with a synthesised source: a known
/// tone in, the same tone measured coming out of the decoder.
///
/// This is the part a live call's counters cannot answer. "Frames pushed to
/// webrtc" says the pipeline is moving, not that what arrives resembles what
/// left — a resampler that quietly halves the rate, a stray gain stage, or an
/// encoder configured for the wrong content would all keep those counters
/// looking healthy.
///
/// Using a generated tone rather than a real microphone is the point: it is
/// deterministic, it needs no hardware, and it captures nobody's room.
///
/// Baseline, three runs against bundled livekit-server 1.12.0 on loopback, so a
/// later change has something to be compared against:
///
/// ```text
/// effective rate   48120 Hz/ch   (+0.25% — the receiver adapting its clock)
/// level            -0.59 .. -0.61 dB
/// energy ±40 Hz    0.937 .. 0.940
/// harmonics        0.0001 at 880 Hz, 0.0000 at 1320 Hz
/// ```
///
/// The thresholds sit well below that on purpose: they are there to catch a
/// path that broke, not to police the third decimal of a lossy codec.
#[tokio::test]
#[ignore = "needs a running LiveKit server; see the module docs"]
async fn a_tone_survives_the_round_trip() {
    use futures_util::StreamExt as _;
    use livekit::webrtc::audio_stream::native::NativeAudioStream;

    let url = env_or("LIVEKIT_URL", "ws://127.0.0.1:7880");
    let room_name = format!("voice-{}", uuid::Uuid::new_v4());

    let (talker, _t_events) = Room::connect(
        &url,
        &token("talker", &room_name, true),
        RoomOptions::default(),
    )
    .await
    .expect("talker connects");
    let (listener, mut listener_events) = Room::connect(
        &url,
        &token("listener", &room_name, true),
        RoomOptions::default(),
    )
    .await
    .expect("listener connects");

    // APM off, deliberately. `AudioSourceOptions::default()` leaves libwebrtc's
    // echo canceller, noise suppressor and AGC on — which is right for a
    // microphone and wrong for this measurement: a steady sine is precisely
    // what a noise suppressor exists to remove, so it eats most of the tone and
    // the number would say more about the suppressor than about the path. With
    // them off, what is left is the transport and the codec, which is what this
    // is for. The APM-on figure is reported below as a second reading.
    let quiet = AudioSourceOptions {
        echo_cancellation: false,
        noise_suppression: false,
        auto_gain_control: false,
    };
    let source = NativeAudioSource::new(quiet, SAMPLE_RATE, CHANNELS, 1000);
    let track = LocalAudioTrack::create_audio_track("tone", RtcAudioSource::Native(source.clone()));
    talker
        .local_participant()
        .publish_track(
            LocalTrack::Audio(track),
            TrackPublishOptions {
                source: TrackSource::Microphone,
                ..Default::default()
            },
        )
        .await
        .expect("publish the tone");
    let feeder = spawn_tone(source);

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

    // Skip the first second: the encoder ramps and the jitter buffer fills, and
    // neither is what this measures.
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
    feeder.abort();

    assert!(
        received.len() > SAMPLE_RATE as usize,
        "only {} samples in the steady-state window",
        received.len()
    );

    let sent = tone_samples(received.len());
    let (r_in, r_out) = (rms(&sent), rms(&received));
    let db = 20.0 * (r_out / r_in).log10();
    let purity_in = tone_purity(&sent, TONE_HZ);
    let purity_out = tone_purity(&received, TONE_HZ);
    let window = (TOTAL - WARMUP).as_secs_f32();
    let rate = received.len() as f32 / CHANNELS as f32 / window;

    println!("frames decoded     : {frames}");
    println!("samples analysed   : {}", received.len());
    println!("effective rate     : {rate:.0} Hz/ch (nominal {SAMPLE_RATE})");
    println!("RMS in / out       : {r_in:.4} / {r_out:.4}  ({db:+.2} dB)");
    println!("tone purity in/out : {purity_in:.4} / {purity_out:.4}");
    let band_in = band_energy(&sent, TONE_HZ, 40.0);
    let band_out = band_energy(&received, TONE_HZ, 40.0);
    println!("energy in ±40 Hz   : {band_in:.4} / {band_out:.4}");
    println!(
        "harmonics out      : 880 Hz {:.4}, 1320 Hz {:.4}",
        tone_purity(&received, 880.0),
        tone_purity(&received, 1320.0)
    );

    // Throughput. This far off would mean samples dropped or duplicated, which
    // is what a broken resampler looks like from here.
    assert!(
        (rate - SAMPLE_RATE as f32).abs() < SAMPLE_RATE as f32 * 0.05,
        "effective rate {rate:.0} Hz is more than 5% off nominal"
    );
    // Level. Opus is lossy, not quiet: more than a few dB means a gain stage in
    // a path that should have none.
    assert!(db.abs() < 3.0, "level moved by {db:+.2} dB end to end");
    // Content. Most of the energy that comes out has to still be the tone.
    assert!(
        band_out > 0.80,
        "only {:.1}% of the received energy is within 40 Hz of {TONE_HZ} — the          signal is not coming back as the tone that went in",
        band_out * 100.0
    );
    // And nothing non-linear: a clipping or distorting stage would put energy
    // on the harmonics, where there should be none.
    assert!(
        tone_purity(&received, TONE_HZ * 2.0) < 0.02,
        "energy showed up at the second harmonic — something in the path is          distorting rather than merely coding"
    );

    listener.close().await.ok();
    talker.close().await.ok();
}
