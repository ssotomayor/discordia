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
//! Three tests, two jobs. `subscribe_only_identity_still_receives_audio` and
//! `a_tone_survives_the_round_trip` assert: they have thresholds and they fail.
//! `the_knobs_that_shape_voice_quality_are_measured` mostly reports — it runs
//! the same round trip under APM, `red`, `dtx`, bitrate and DeepFilterNet
//! ceiling settings and prints them side by side, asserting only that each one
//! still delivers audio, since a knob that silences the path is a bug while a
//! knob that costs 0.3 dB is a choice. Read its output rather than its result.
//!
//! One caveat that belongs at the top, because it decides what the sweep can be
//! read to mean: the excitation is a 440 Hz sine, optionally plus white noise.
//! That is a fine probe for a codec and a transport, and a poor one for
//! anything trained on speech. The DeepFilterNet rows demonstrate it — the
//! model saturates at whatever ceiling it is given, because it hears nothing it
//! recognises as voice.
//!
//! # Why this is here at all
//!
//! Worth stating, because everything about this file argues against itself:
//! nothing in CI runs it — the test job covers four crates and `dioxusfun` is
//! not among them, and every test here is `#[ignore]`d besides — and it is the only
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
/// `noise` mixes white noise in at that peak amplitude, from a plain LCG so the
/// sequence is fixed and two runs are comparable.
///
/// Not because it avoids a dependency — `rand` is already a direct dependency
/// of this crate and offers seeded generators that would do as well. It is here
/// because the seed is visible in the six lines below, and a sweep whose rows
/// are compared against each other should not have its input hidden behind a
/// generator whose stream could change with a version bump.
fn spawn_tone(source: NativeAudioSource, noise: f32, denoise: Option<f32>) -> Feeder {
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    // A blocking task rather than `tokio::spawn`, because `DfTract` is not
    // `Send` and a spawned future may not hold it across an await. Nothing here
    // wants to be on the async runtime anyway: it is a fixed 10 ms loop that
    // must not share a worker with the receive side.
    let rt = tokio::runtime::Handle::current();
    let handle = tokio::task::spawn_blocking(move || {
        let mut phase = 0.0f32;
        let step = 2.0 * std::f32::consts::PI * TONE_HZ / SAMPLE_RATE as f32;
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        // One model for the whole run, built here rather than passed in: it is
        // stateful across hops (STFT overlap and GRU state), so it belongs to
        // exactly one continuous stream — the same rule `Denoiser`'s own doc
        // comment states. ~200 ms to load, once, before the first frame.
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
                    let mut v = phase.sin() * AMPLITUDE;
                    phase = (phase + step) % (2.0 * std::f32::consts::PI);
                    if noise > 0.0 {
                        seed = seed
                            .wrapping_mul(6364136223846793005)
                            .wrapping_add(1442695040888963407);
                        // Top 24 bits to [-1, 1): the low bits of an LCG are
                        // the ones with visible period.
                        let u = (seed >> 40) as f32 / (1u32 << 23) as f32 - 1.0;
                        v += u * noise;
                    }
                    v.clamp(-1.0, 1.0)
                })
                .collect();

            // Where the model sits in production: on the publish task, one hop
            // at a time, *before* the frame reaches libwebrtc. Putting it
            // anywhere else here would measure an arrangement nothing ships.
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
                if before > 0.0 && after > 0.0 {
                    applied.sum_db += 20.0 * (after / before).log10();
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
    Feeder { handle, stop }
}

/// The feeding side of a round trip, and the switch that turns it off.
///
/// It used to be a bare `JoinHandle` the caller `abort()`ed. A blocking task
/// cannot be aborted once it is running — `abort` only stops it being polled —
/// so the flag is not decoration: without it the feeder would run on into the
/// next row of the sweep, publishing into a room the test has finished with.
struct Feeder {
    handle: tokio::task::JoinHandle<Applied>,
    stop: Arc<std::sync::atomic::AtomicBool>,
}

impl Feeder {
    /// Stop feeding, wait for the loop to notice — which takes one hop — and
    /// take what it measured on the way.
    ///
    /// The stats come back as the task's value rather than through a shared
    /// cell. Nothing reads them until this point, so ownership does the whole
    /// job a `Mutex` was doing, and a panicking hop stops being invisible: it
    /// arrives here as a `JoinError` instead of being discarded, which is the
    /// difference between "the denoiser failed on hop 214" and the round trip
    /// later reporting that it stopped delivering audio.
    async fn stop(self) -> Applied {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        match self.handle.await {
            Ok(applied) => applied,
            Err(e) => panic!("the feeder died before the measurement finished: {e}"),
        }
    }
}

/// How much the denoiser actually pulled the signal down, accumulated by the
/// feeder because that is the only place both sides of the hop exist, and
/// handed back when it stops.
///
/// The number `TODO.md` and `ClientSettings::denoise_atten_lim_db` both cite —
/// "−3.9 dB applied at a 30 dB ceiling, −2.7 dB at 12" — came from a live
/// session on one machine and could not be re-run from the repo. This is where
/// it comes from now.
#[derive(Default)]
struct Applied {
    sum_db: f32,
    hops: usize,
}

impl Applied {
    /// Mean dB applied per hop, or `None` when the model was not in the path.
    fn mean_db(&self) -> Option<f32> {
        (self.hops > 0).then(|| self.sum_db / self.hops as f32)
    }
}

/// What a round trip is run with.
///
/// `red`, `dtx` and `max_bitrate` are knobs the client sets as such. `apm` is
/// not — see its own note.
#[derive(Clone, Copy)]
struct Config {
    /// libwebrtc's echo canceller, noise suppressor and AGC, all three at once.
    ///
    /// Deliberately coarser than the client, which never sets them together:
    /// `apm_options` pins `echo_cancellation` on always, derives
    /// `noise_suppression` from the *inverse* of the DeepFilterNet toggle, and
    /// passes `auto_gain_control` straight through from the user's own switch.
    /// Three independent inputs, and no combination of them turns AEC off.
    ///
    /// All-on against all-off is still the right probe for the question this
    /// sweep asks, which is whether the APM does anything on this path at all.
    /// A difference between the client's actual arrangements would be smaller
    /// and harder to read; if even the extremes are indistinguishable, no
    /// arrangement between them can be doing much. What it cannot do is stand
    /// in for a configuration someone ships — this row is an instrument
    /// setting, not a product one.
    apm: bool,
    /// Opus redundancy: a copy of the previous frame in every packet. On by
    /// default in the SDK, which is why the client's measured send rate reads
    /// as double its nominal one.
    red: bool,
    /// Discontinuous transmission: stop sending during silence.
    dtx: bool,
    /// `None` leaves the encoder to choose.
    max_bitrate: Option<u64>,
    /// Peak amplitude of white noise mixed in with the tone, 0 for none.
    ///
    /// The discriminator the first version of this sweep lacked. A pure sine
    /// is the one signal that tells you nothing about a noise suppressor: it
    /// measured identical with the APM on and off, which has two explanations
    /// — the suppressor leaves a strong steady tone alone, or the options do
    /// not reach this capture path at all — and no way to tell them apart.
    /// Noise separates them, because a working suppressor must remove it.
    noise: f32,
    /// DeepFilterNet's attenuation ceiling in dB, or `None` to leave the model
    /// out of the path entirely.
    ///
    /// The client's own default is 30 (`denoise::ATTEN_LIM_DB`) with a user
    /// control down to 12, and the argument for both is a pair of numbers from
    /// a session nobody can reproduce. This is the dimension that makes them
    /// reproducible. It is deliberately *not* the same knob as `apm`: that one
    /// is libwebrtc's suppressor, this one is ours, and the client runs exactly
    /// one of the two at a time.
    denoise: Option<f32>,
}

impl Config {
    /// The baseline: APM off so what is measured is the transport and the
    /// codec, SDK defaults for everything else.
    fn transport_only() -> Self {
        Self {
            apm: false,
            red: true,
            dtx: true,
            max_bitrate: None,
            noise: 0.0,
            denoise: None,
        }
    }
}

/// One round trip's worth of numbers. Named rather than a tuple because the
/// sweep prints them side by side and mixing two up would be silent.
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
    /// Mean dB DeepFilterNet applied per hop on the way in, `None` when it was
    /// not in the path. Measured at the source, unlike everything above it,
    /// because by the time audio comes back the model's effect and the codec's
    /// are the same number.
    applied_db: Option<f32>,
}

/// Publish a tone into a fresh room under `cfg`, decode it back, and measure.
///
/// One implementation on purpose. The sweep exists to compare configurations
/// against the baseline, and two measurement loops that drifted apart would
/// compare nothing — the file's own rule is that a failing assertion means
/// asking whether the measurement is wrong first, which only works while there
/// is one measurement to ask about.
async fn measure_round_trip(cfg: Config) -> Metrics {
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

    // A steady sine is precisely what a noise suppressor exists to remove, so
    // with the APM on the number says more about the suppressor than about the
    // path. That is a reading worth having — it is what a real microphone
    // publishes through — but it is not the baseline.
    let opts = AudioSourceOptions {
        echo_cancellation: cfg.apm,
        noise_suppression: cfg.apm,
        auto_gain_control: cfg.apm,
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
    let feeder = spawn_tone(source, cfg.noise, cfg.denoise);

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
    // Measured, not `TOTAL - WARMUP`: the loop also exits on a 2s stall or a
    // stream that ends, and dividing a short sample count by the full nominal
    // window would report the dropout as a rate error — pointing the failure at
    // the resampler when the stream is what broke.
    let window = started.elapsed().saturating_sub(WARMUP).as_secs_f32();
    let applied = feeder.stop().await;

    let sent = tone_samples(received.len());
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
    let m = measure_round_trip(Config::transport_only()).await;

    assert!(
        m.samples > SAMPLE_RATE as usize,
        "only {} samples in the steady-state window",
        m.samples
    );

    report("APM off (baseline)", &m);

    // Throughput. This far off would mean samples dropped or duplicated, which
    // is what a broken resampler looks like from here.
    assert!(
        (m.rate - SAMPLE_RATE as f32).abs() < SAMPLE_RATE as f32 * 0.05,
        "effective rate {:.0} Hz is more than 5% off nominal",
        m.rate
    );
    // Level. Opus is lossy, not quiet: more than a few dB means a gain stage in
    // a path that should have none.
    assert!(
        m.db.abs() < 3.0,
        "level moved by {:+.2} dB end to end",
        m.db
    );
    // Content. Most of the energy that comes out has to still be the tone.
    assert!(
        m.band_out > 0.80,
        "only {:.1}% of the received energy is within 40 Hz of {TONE_HZ} — the \
         signal is not coming back as the tone that went in",
        m.band_out * 100.0
    );
    // And nothing non-linear: a clipping or distorting stage would put energy
    // on the harmonics, where there should be none.
    assert!(
        m.h2 < 0.02,
        "energy showed up at the second harmonic — something in the path is \
         distorting rather than merely coding"
    );
}

/// The baseline test's own output, factored out so a sweep row and the
/// baseline are printed by the same code and can be read against each other.
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

/// The knobs, measured rather than argued about.
///
/// This exists because the baseline test's comment promised "the APM-on figure
/// is reported below as a second reading" and no such reading was ever written
/// — the claim sat in the file with nothing behind it. It now has a number, and
/// so do the three encoder settings the client ships with but has never
/// measured: Opus redundancy (`red`, on by default, and the reason the client's
/// send rate reads as double its nominal), discontinuous transmission, and a
/// capped bitrate.
///
/// Deliberately not an optimiser. Each row is one 5-second run on loopback, so
/// the differences it can resolve are the large ones — a suppressor eating the
/// signal, a bitrate that stops carrying the tone. Small differences between
/// rows are noise, and treating them as a ranking would be reading the third
/// decimal of a lossy codec, which the baseline's own thresholds refuse to do.
///
/// The only assertion is that every configuration still delivers audio. A knob
/// that silences the path is a bug; a knob that costs 0.3 dB is a choice.
///
/// ```text
/// cargo test -p dioxusfun --test live_sfu -- --ignored --nocapture the_knobs
/// ```
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
        // The pair that has to be read together: same noisy input, APM off
        // then on. If the suppressor runs on this path, the second row keeps a
        // larger share of its energy in the tone's band, because the noise
        // spread across everything else is what got removed. If the two rows
        // match, the APM is not reaching this capture path and the baseline's
        // reason for switching it off is about something that never happened.
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
        // The ceiling, which is the one knob whose numbers were quoted from a
        // session nobody could re-run. Read these three together and against
        // "noise 0.15, APM off" directly above, which is the same input with no
        // suppressor of any kind. Two questions at once: does our own model
        // remove this noise where libwebrtc's did not, and does moving the
        // ceiling 30 → 12 change what it removes.
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
        // Not a shipping value — the client's control stops at 30. It is here
        // as the instrument's own scale, and it is the row that exposes the
        // limit of this input: it takes the signal to −97 dB and 0.8% of energy
        // left in band. Read together with the two above, where applied lands
        // on the ceiling to within 0.3 dB, it says the model is saturated —
        // against a sine plus white noise it hears no speech and removes
        // everything it is allowed to. Which is why these rows cannot answer
        // "does 30 vs 12 matter for speech": no speech is in the path. See
        // TODO.md, where that is now the open half.
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
