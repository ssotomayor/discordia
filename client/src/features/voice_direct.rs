use super::*;
use libwebrtc::peer_connection_factory::native::PeerConnectionFactoryExt;
use libwebrtc::{audio_track::RtcAudioTrack, peer_connection_factory::PeerConnectionFactory};

pub struct Audio {
    mic: MicCapture,
    playback: PlaybackMixer,
    controls: AudioControls,
    pub track: RtcAudioTrack,
    publisher: tokio::task::JoinHandle<()>,
    meter: Task,
    diagnostics: Task,
    receivers: Vec<tokio::task::JoinHandle<()>>,
}

impl Audio {
    pub fn start(factory: &PeerConnectionFactory, state: Signal<AppState>) -> Result<Self, String> {
        let controls = AudioControls::from_state(&state.peek());
        let source = NativeAudioSource::new(inert_apm_options(), SAMPLE_RATE, CHANNELS, 100);
        let track = factory.create_audio_track("dm-mic", source.clone());
        let (frame_tx, frame_rx) = crate::audio_queue::channel();
        let (gated_tx, gated_rx) = crate::audio_queue::channel();
        let (reference_tx, reference_rx) = crate::audio_queue::channel();
        let meter = Arc::new(MicMeter::default());
        let gate_stats = Arc::new(GateStats::default());
        let muted = Arc::new(AtomicBool::new(
            state.peek().voice.muted || state.peek().voice.deafened,
        ));
        track.set_enabled(!muted.load(Ordering::Relaxed));
        let playback = PlaybackMixer::start(state, controls.clone(), reference_tx)?;
        let mic = MicCapture::start(frame_tx, state, muted.clone(), gate_stats.clone())?;
        let dsp_controls = controls.clone();
        let dsp_meter = meter.clone();
        std::thread::Builder::new()
            .name("dm-mic-dsp".into())
            .spawn(move || {
                denoise_gate_loop(
                    frame_rx,
                    gated_tx,
                    dsp_controls,
                    dsp_meter,
                    muted,
                    gate_stats,
                    reference_rx,
                );
            })
            .map_err(|e| format!("Start call audio processing: {e}"))?;
        Ok(Self {
            mic,
            playback,
            controls,
            track,
            publisher: tokio::spawn(publish_loop(gated_rx, source)),
            meter: spawn_meter_task(state, meter),
            diagnostics: spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    let s = state.peek();
                    eprintln!(
                        "[dm-call] mic pre={} processed={} gain={}% agc={} noise_suppression={} threshold={} muted={} deafened={} playback_gain={:.2}",
                        peak_to_db_label(s.mic_level_pre),
                        peak_to_db_label(s.mic_level),
                        s.mic_volume,
                        s.auto_gain_control,
                        s.noise_cancellation,
                        peak_to_db_label(s.mic_sensitivity),
                        s.voice.muted,
                        s.voice.deafened,
                        s.dm_call
                            .as_ref()
                            .map_or(1.0, |call| s.voice_gain_of(&call.peer)),
                    );
                }
            }),
            receivers: Vec::new(),
        })
    }

    pub fn receive(&mut self, track: RtcAudioTrack, peer: String) {
        if !self.receivers.is_empty() {
            return;
        }
        let stream = NativeAudioStream::new(track, SAMPLE_RATE as i32, CHANNELS as i32);
        self.receivers.push(tokio::spawn(consume_remote_track(
            stream,
            self.playback.handle(),
            peer,
            TrackKind::Voice,
        )));
    }

    pub fn update(&self, state: &AppState) {
        let muted = state.voice.muted || state.voice.deafened;
        self.mic.muted.store(muted, Ordering::Relaxed);
        self.track.set_enabled(!muted);
        self.controls
            .deafened
            .store(state.voice.deafened, Ordering::Relaxed);
        self.controls
            .threshold
            .store(state.mic_sensitivity as i32, Ordering::Relaxed);
        self.controls
            .mic_gain_pct
            .store(state.mic_volume as i32, Ordering::Relaxed);
        self.controls
            .agc
            .store(state.auto_gain_control, Ordering::Relaxed);
        self.controls
            .denoise
            .store(state.noise_cancellation, Ordering::Relaxed);
        self.controls
            .atten_lim_db
            .store(state.denoise_atten_lim_db, Ordering::Relaxed);
        let mut gains = self.controls.gains.lock();
        gains.clear();
        for peer in state.user_volumes.keys().chain(state.user_muted.iter()) {
            gains.insert(peer.clone(), state.voice_gain_of(peer));
        }
    }
}

impl Drop for Audio {
    fn drop(&mut self) {
        self.track.set_enabled(false);
        self.publisher.abort();
        self.meter.cancel();
        self.diagnostics.cancel();
        for receiver in self.receivers.drain(..) {
            receiver.abort();
        }
    }
}
