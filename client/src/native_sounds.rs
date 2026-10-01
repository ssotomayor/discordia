use std::f64::consts::PI;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, OnceLock, mpsc};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;

const NAMES: [&str; 15] = [
    "disconnect",
    "watch-start",
    "watch-stop",
    "notify",
    "dm",
    "connect",
    "server-disconnect",
    "peer-join",
    "peer-leave",
    "stream-start",
    "stream-stop",
    "peer-stream-start",
    "peer-stream-stop",
    "mute",
    "unmute",
];
const IDLE: Duration = Duration::from_millis(1500);
const COOLDOWN: Duration = Duration::from_millis(250);
const MAX_VOICES: usize = 32;

#[derive(Clone, Copy)]
struct Sound(usize);

enum Command {
    Play(Sound),
    Wake,
}

struct Control {
    volume: AtomicU32,
    output: Mutex<Option<String>>,
}

struct Service {
    tx: mpsc::SyncSender<Command>,
    control: Arc<Control>,
}

fn service() -> Option<&'static Service> {
    static SERVICE: OnceLock<Option<Service>> = OnceLock::new();
    SERVICE
        .get_or_init(|| {
            let (tx, rx) = mpsc::sync_channel(32);
            let control = Arc::new(Control {
                volume: AtomicU32::new(0.7_f32.to_bits()),
                output: Mutex::new(None),
            });
            let worker_control = Arc::clone(&control);
            match std::thread::Builder::new()
                .name("notification-audio".into())
                .spawn(move || run(rx, worker_control))
            {
                Ok(handle) => {
                    drop(handle);
                    Some(Service { tx, control })
                }
                Err(error) => {
                    tracing::warn!(%error, "Couldn't start notification audio");
                    None
                }
            }
        })
        .as_ref()
}

pub fn play(name: &str) {
    let Some(index) = NAMES.iter().position(|candidate| *candidate == name) else {
        return;
    };
    if let Some(service) = service() {
        match service.tx.try_send(Command::Play(Sound(index))) {
            Ok(()) | Err(mpsc::TrySendError::Full(_)) => {}
            Err(mpsc::TrySendError::Disconnected(_)) => {
                tracing::warn!("Notification audio worker stopped");
            }
        }
    }
}

pub fn configure(volume: u8, output: Option<String>) {
    if let Some(service) = service() {
        service.control.volume.store(
            (f32::from(volume.min(100)) / 100.0).to_bits(),
            Ordering::Relaxed,
        );
        *service.control.output.lock() = output;
        match service.tx.try_send(Command::Wake) {
            Ok(()) | Err(mpsc::TrySendError::Full(_)) => {}
            Err(mpsc::TrySendError::Disconnected(_)) => {
                tracing::warn!("Notification audio worker stopped");
            }
        }
    }
}

#[derive(Default)]
struct Cooldowns([Option<Duration>; NAMES.len()]);

impl Cooldowns {
    fn accept(&mut self, sound: Sound, now: Duration) -> bool {
        let last = &mut self.0[sound.0];
        if last.is_some_and(|last| now.saturating_sub(last) < COOLDOWN) {
            return false;
        }
        *last = Some(now);
        true
    }
}

fn run(rx: mpsc::Receiver<Command>, control: Arc<Control>) {
    let origin = Instant::now();
    let mut cooldowns = Cooldowns::default();
    let mut output: Option<AudioOutput> = None;
    let mut selected = None;
    let mut last_play = Instant::now();
    let mut bank = SoundBank::new(0);
    loop {
        let command = if output.is_some() {
            match rx.recv_timeout(IDLE.saturating_sub(last_play.elapsed())) {
                Ok(command) => command,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    output = None;
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match rx.recv() {
                Ok(command) => command,
                Err(_) => break,
            }
        };
        let wanted = control.output.lock().clone();
        if wanted != selected
            || output
                .as_ref()
                .is_some_and(|out| out.failed.load(Ordering::Relaxed))
        {
            output = None;
            selected = wanted;
        }
        let Command::Play(sound) = command else {
            continue;
        };
        if f32::from_bits(control.volume.load(Ordering::Relaxed)) == 0.0
            || !cooldowns.accept(sound, origin.elapsed())
        {
            continue;
        }
        if output.is_none() {
            match AudioOutput::open(selected.as_deref(), Arc::clone(&control)) {
                Ok(stream) => output = Some(stream),
                Err(error) => {
                    tracing::warn!(%error, "Couldn't play notification sound");
                    continue;
                }
            }
        }
        if let Some(output) = &output {
            if bank.rate != output.rate {
                bank = SoundBank::new(output.rate);
            }
            output.player.lock().push(bank.get(sound));
            last_play = Instant::now();
        }
    }
}

struct Voice {
    samples: Arc<[f32]>,
    position: usize,
}

#[derive(Default)]
struct Player {
    voices: Vec<Voice>,
}

impl Player {
    fn push(&mut self, samples: Arc<[f32]>) {
        if self.voices.len() >= MAX_VOICES {
            self.voices.remove(0);
        }
        self.voices.push(Voice {
            samples,
            position: 0,
        });
    }

    fn next(&mut self, volume: f32) -> f32 {
        let mut sample = 0.0;
        for voice in &mut self.voices {
            if let Some(value) = voice.samples.get(voice.position) {
                sample += value;
            }
            voice.position += 1;
        }
        self.voices
            .retain(|voice| voice.position < voice.samples.len());
        (sample * volume).clamp(-1.0, 1.0)
    }
}

struct AudioOutput {
    stream: cpal::Stream,
    player: Arc<Mutex<Player>>,
    failed: Arc<AtomicBool>,
    rate: u32,
}

impl Drop for AudioOutput {
    fn drop(&mut self) {
        // CPAL 0.15 can retain a CoreAudio stream after drop; pause releases the running audio unit.
        if let Err(error) = self.stream.pause() {
            tracing::warn!(%error, "Couldn't stop notification audio");
        }
    }
}

impl AudioOutput {
    fn open(selected: Option<&str>, control: Arc<Control>) -> Result<Self, String> {
        let host = cpal::default_host();
        let device = crate::features::voice::pick_device(
            selected,
            host.default_output_device(),
            host.output_devices().ok(),
        )
        .ok_or("No audio output device available.")?;
        let supported = device.default_output_config().map_err(|e| e.to_string())?;
        let format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();
        if config.channels == 0 || config.sample_rate.0 == 0 {
            return Err("Invalid audio output configuration.".into());
        }
        let player = Arc::new(Mutex::new(Player {
            voices: Vec::with_capacity(MAX_VOICES),
        }));
        let failed = Arc::new(AtomicBool::new(false));
        let stream = match format {
            cpal::SampleFormat::F32 => build::<f32>(&device, &config, &player, &control, &failed),
            cpal::SampleFormat::F64 => build::<f64>(&device, &config, &player, &control, &failed),
            cpal::SampleFormat::I8 => build::<i8>(&device, &config, &player, &control, &failed),
            cpal::SampleFormat::I16 => build::<i16>(&device, &config, &player, &control, &failed),
            cpal::SampleFormat::I32 => build::<i32>(&device, &config, &player, &control, &failed),
            cpal::SampleFormat::I64 => build::<i64>(&device, &config, &player, &control, &failed),
            cpal::SampleFormat::U8 => build::<u8>(&device, &config, &player, &control, &failed),
            cpal::SampleFormat::U16 => build::<u16>(&device, &config, &player, &control, &failed),
            cpal::SampleFormat::U32 => build::<u32>(&device, &config, &player, &control, &failed),
            cpal::SampleFormat::U64 => build::<u64>(&device, &config, &player, &control, &failed),
            _ => return Err(format!("Unsupported audio format: {format}")),
        }
        .map_err(|e| e.to_string())?;
        let output = Self {
            stream,
            player,
            failed,
            rate: config.sample_rate.0,
        };
        output.stream.play().map_err(|e| e.to_string())?;
        Ok(output)
    }
}

fn build<T: cpal::SizedSample + cpal::FromSample<f32>>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    player: &Arc<Mutex<Player>>,
    control: &Arc<Control>,
    failed: &Arc<AtomicBool>,
) -> Result<cpal::Stream, cpal::BuildStreamError> {
    let player = Arc::clone(player);
    let control = Arc::clone(control);
    let failed = Arc::clone(failed);
    let channels = usize::from(config.channels);
    device.build_output_stream(
        config,
        move |data: &mut [T], _| {
            if let Some(mut player) = player.try_lock() {
                let volume = f32::from_bits(control.volume.load(Ordering::Relaxed));
                write_samples(data, channels, &mut player, volume);
            } else {
                data.fill(T::from_sample(0.0));
            }
        },
        move |error| {
            failed.store(true, Ordering::Relaxed);
            tracing::warn!(%error, "Notification audio output failed");
        },
        None,
    )
}

fn write_samples<T: cpal::Sample + cpal::FromSample<f32>>(
    data: &mut [T],
    channels: usize,
    player: &mut Player,
    volume: f32,
) {
    for frame in data.chunks_mut(channels) {
        frame.fill(T::from_sample(player.next(volume)));
    }
}

struct SoundBank {
    rate: u32,
    samples: [Option<Arc<[f32]>>; NAMES.len()],
}

impl SoundBank {
    fn new(rate: u32) -> Self {
        Self {
            rate,
            samples: std::array::from_fn(|_| None),
        }
    }
    fn get(&mut self, sound: Sound) -> Arc<[f32]> {
        Arc::clone(self.samples[sound.0].get_or_insert_with(|| synthesize(sound, self.rate).into()))
    }
}

#[derive(Clone, Copy)]
enum Wave {
    Sine,
    Triangle,
    Saw,
    Square,
}

struct Tone {
    at: f64,
    f0: f64,
    f1: f64,
    duration: f64,
    peak: f64,
    wave: Wave,
    attack: f64,
}

const fn tone(at: f64, frequency: f64, duration: f64, peak: f64, wave: Wave) -> Tone {
    Tone {
        at,
        f0: frequency,
        f1: frequency,
        duration,
        peak,
        wave,
        attack: 0.012,
    }
}

fn tones(sound: Sound) -> &'static [Tone] {
    use Wave::*;
    const BANK: [&[Tone]; NAMES.len()] = [
        &[
            tone(0.0, 520.0, 0.14, 0.13, Sine),
            tone(0.10, 330.0, 0.22, 0.13, Sine),
        ],
        &[
            tone(0.0, 620.0, 0.09, 0.09, Triangle),
            tone(0.07, 880.0, 0.14, 0.09, Triangle),
        ],
        &[
            tone(0.0, 720.0, 0.09, 0.07, Triangle),
            tone(0.07, 480.0, 0.13, 0.07, Triangle),
        ],
        &[tone(0.0, 660.0, 0.3, 0.14, Sine)],
        &[
            tone(0.0, 784.0, 0.13, 0.11, Triangle),
            tone(0.11, 1046.0, 0.22, 0.10, Triangle),
        ],
        &[
            tone(0.0, 440.0, 0.12, 0.12, Sine),
            tone(0.09, 660.0, 0.18, 0.12, Sine),
        ],
        &[
            tone(0.0, 440.0, 0.15, 0.11, Sine),
            tone(0.12, 220.0, 0.25, 0.11, Sine),
        ],
        &[
            tone(0.0, 523.0, 0.06, 0.08, Triangle),
            tone(0.05, 659.0, 0.10, 0.08, Triangle),
        ],
        &[
            tone(0.0, 659.0, 0.06, 0.08, Triangle),
            tone(0.05, 523.0, 0.10, 0.08, Triangle),
        ],
        &[Tone {
            at: 0.0,
            f0: 300.0,
            f1: 900.0,
            duration: 0.18,
            peak: 0.10,
            wave: Saw,
            attack: 0.015,
        }],
        &[Tone {
            at: 0.0,
            f0: 900.0,
            f1: 300.0,
            duration: 0.18,
            peak: 0.10,
            wave: Saw,
            attack: 0.015,
        }],
        &[
            tone(0.0, 440.0, 0.07, 0.06, Triangle),
            tone(0.06, 880.0, 0.12, 0.06, Triangle),
        ],
        &[
            tone(0.0, 880.0, 0.07, 0.06, Triangle),
            tone(0.06, 440.0, 0.12, 0.06, Triangle),
        ],
        &[tone(0.0, 220.0, 0.06, 0.10, Square)],
        &[tone(0.0, 440.0, 0.06, 0.10, Square)],
    ];
    BANK.get(sound.0).copied().unwrap_or(&[])
}

fn synthesize(sound: Sound, rate: u32) -> Vec<f32> {
    if rate == 0 {
        return Vec::new();
    }
    let notes = tones(sound);
    let end = notes
        .iter()
        .map(|tone| tone.at + tone.duration + 0.02)
        .fold(0.0, f64::max);
    let mut samples = vec![0.0; (end * f64::from(rate)).ceil() as usize];
    for note in notes {
        let start = (note.at * f64::from(rate)).round() as usize;
        for (i, sample) in samples.iter_mut().enumerate().skip(start) {
            let time = (i - start) as f64 / f64::from(rate);
            if time >= note.duration + 0.02 {
                break;
            }
            let envelope = if time < note.attack {
                0.0001 * (note.peak / 0.0001).powf(time / note.attack)
            } else if time < note.duration {
                note.peak
                    * (0.0001 / note.peak)
                        .powf((time - note.attack) / (note.duration - note.attack))
            } else {
                0.0001 * (1.0 - (time - note.duration) / 0.02)
            };
            let sweep_time = time.min(note.duration);
            let slope = (note.f1 - note.f0) / note.duration;
            let cycles = note.f0 * sweep_time
                + 0.5 * slope * sweep_time * sweep_time
                + note.f1 * (time - sweep_time);
            let phase = 2.0 * PI * cycles;
            let frequency = note.f0 + slope * sweep_time;
            *sample += (oscillator(note.wave, phase, frequency, rate) * envelope) as f32;
        }
    }
    samples
}

fn oscillator(wave: Wave, phase: f64, frequency: f64, rate: u32) -> f64 {
    if matches!(wave, Wave::Sine) {
        return phase.sin();
    }
    let harmonics = ((f64::from(rate) * 0.5 / frequency).floor() as usize).min(32);
    let mut value = 0.0;
    for harmonic in 1..=harmonics {
        let n = harmonic as f64;
        let coefficient = match wave {
            Wave::Square if harmonic % 2 == 1 => 4.0 / (PI * n),
            Wave::Triangle if harmonic % 2 == 1 => {
                let sign = if (harmonic / 2).is_multiple_of(2) {
                    1.0
                } else {
                    -1.0
                };
                sign * 8.0 / (PI * PI * n * n)
            }
            Wave::Saw => -2.0 / (PI * n),
            _ => 0.0,
        };
        value += coefficient * (phase * n).sin();
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_sound_is_finite_nonempty_and_fades_out_at_supported_rates() {
        for rate in [16_000, 44_100, 48_000, 96_000] {
            for index in 0..NAMES.len() {
                let samples = synthesize(Sound(index), rate);
                assert!(!samples.is_empty());
                assert!(samples.len() <= (f64::from(rate) * 0.4).ceil() as usize);
                assert!(
                    samples
                        .iter()
                        .all(|sample| sample.is_finite() && sample.abs() < 0.4)
                );
                assert!(samples.iter().any(|sample| sample.abs() > 0.001));
                assert!(samples.last().unwrap().abs() < 0.00001);
            }
        }
    }

    #[test]
    fn cooldown_is_independent_per_sound_and_allows_the_exact_boundary() {
        let mut cooldowns = Cooldowns::default();
        assert!(cooldowns.accept(Sound(3), Duration::ZERO));
        assert!(!cooldowns.accept(Sound(3), Duration::from_millis(249)));
        assert!(cooldowns.accept(Sound(4), Duration::from_millis(249)));
        assert!(cooldowns.accept(Sound(3), Duration::from_millis(250)));
    }

    #[test]
    fn mixer_applies_live_volume_clamps_overlap_and_retires_finished_sounds() {
        let mut player = Player::default();
        player.push(vec![0.8, 0.8, 0.8].into());
        player.push(vec![0.8, 0.8, 0.8].into());
        assert_eq!(player.next(0.0), 0.0);
        assert!((player.next(0.25) - 0.4).abs() < 0.00001);
        assert_eq!(player.next(1.0), 1.0);
        assert!(player.voices.is_empty());
        assert_eq!(player.next(1.0), 0.0);
    }

    #[test]
    fn stereo_uses_one_mono_frame_and_unsigned_silence_is_centered() {
        let mut player = Player::default();
        player.push(vec![0.25, -0.25].into());
        let mut stereo = [0.0_f32; 4];
        write_samples(&mut stereo, 2, &mut player, 1.0);
        assert_eq!(stereo, [0.25, 0.25, -0.25, -0.25]);
        let mut silence = [0_u16; 4];
        write_samples(&mut silence, 2, &mut player, 1.0);
        assert_eq!(silence, [32768; 4]);
    }

    #[test]
    fn voice_count_is_bounded_and_sound_bank_reuses_samples() {
        let mut player = Player::default();
        for _ in 0..MAX_VOICES + 10 {
            player.push(vec![0.1].into());
        }
        assert_eq!(player.voices.len(), MAX_VOICES);
        let mut bank = SoundBank::new(48_000);
        let first = bank.get(Sound(3));
        assert!(Arc::ptr_eq(&first, &bank.get(Sound(3))));
    }

    #[test]
    #[ignore = "requires a real audio output device; plays a notification tone"]
    fn native_notification_output_plays_and_stops() {
        let control = Arc::new(Control {
            volume: AtomicU32::new(0.7_f32.to_bits()),
            output: Mutex::new(None),
        });
        let output = AudioOutput::open(None, control).expect("native audio output");
        let player = Arc::clone(&output.player);
        player.lock().push(synthesize(Sound(3), output.rate).into());
        let deadline = Instant::now() + Duration::from_secs(3);
        while !player.lock().voices.is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            player.lock().voices.is_empty(),
            "audio callback did not consume the tone"
        );
        eprintln!("Native notification played at {} Hz", output.rate);
        drop(output);
        player.lock().push(vec![0.1; 100].into());
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(
            player.lock().voices[0].position,
            0,
            "audio callback continued after drop"
        );
    }
}
