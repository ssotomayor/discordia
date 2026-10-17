use super::*;
use crate::mic_profile::{Calibration, Config, Context};

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Mode {
    #[default]
    Idle,
    Monitor,
    Record,
    Analyze,
    Ready,
    Before,
    After,
}

struct Recording {
    _mic: MicCapture,
    rx: crate::audio_queue::AudioReceiver<f32>,
    faults: UnboundedReceiver<AudioFault>,
    start: Instant,
    checked: Instant,
    context: Context,
    config: Config,
    frames: Vec<calibration::Hop>,
}

impl Recording {
    fn start(state: Signal<AppState>) -> Result<Self, String> {
        let (tx, rx) = crate::audio_queue::channel();
        let (fault_tx, faults) = unbounded_channel();
        let mic = MicCapture::start(
            tx,
            state,
            Arc::new(AtomicBool::new(false)),
            Arc::new(GateStats::default()),
            fault_tx,
        )?;
        let context = calibration::context(&state.peek())?;
        Ok(Self {
            _mic: mic,
            rx,
            faults,
            start: Instant::now(),
            checked: Instant::now(),
            context,
            config: Config::from_state(&state.peek()),
            frames: Vec::with_capacity(1500),
        })
    }

    fn collect(&mut self, state: &AppState) -> Result<(), String> {
        if self.faults.try_recv().is_ok() {
            return Err(
                "The microphone became unavailable. Reconnect it and repeat the adjustment.".into(),
            );
        }
        if self.checked.elapsed() > std::time::Duration::from_millis(500) {
            if calibration::context(state)? != self.context {
                return Err("The audio device changed. Repeat the microphone adjustment.".into());
            }
            self.checked = Instant::now();
        }
        while let Ok(frame) = self.rx.try_recv() {
            let Some(elapsed) = frame.captured.checked_duration_since(self.start) else {
                continue;
            };
            if !frame.is_fresh() || elapsed.as_secs_f32() >= calibration::SECONDS as f32 {
                continue;
            }
            if self.frames.len() >= 1600 {
                return Err(
                    "The microphone returned invalid audio. Try another input device.".into(),
                );
            }
            self.frames.push(calibration::Hop {
                at: elapsed.as_secs_f32(),
                samples: frame.samples,
            });
        }
        Ok(())
    }
}

struct Prepared {
    analysis: calibration::Analysis,
    context: Context,
    original: Config,
}

struct Job {
    handle: tokio::task::JoinHandle<Result<Prepared, String>>,
    cancel: Arc<AtomicBool>,
}

impl Drop for Job {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.handle.abort();
    }
}

struct Player {
    _playback: PlaybackMixer,
    task: tokio::task::JoinHandle<()>,
    faults: UnboundedReceiver<AudioFault>,
}

impl Drop for Player {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Player {
    fn start(state: Signal<AppState>, data: Arc<Prepared>, after: bool) -> Result<Self, String> {
        let controls = AudioControls::from_state(&state.peek());
        controls.deafened.store(false, Ordering::Relaxed);
        controls.gains.lock().clear();
        let (reference, _rx) = crate::audio_queue::channel();
        let (fault_tx, faults) = unbounded_channel();
        let playback = PlaybackMixer::start(state, controls, reference, fault_tx);
        if !playback.is_open() {
            return Err("No audio output is available for the microphone test.".into());
        }
        let handle = playback.handle();
        let track = handle.add_track("mic-comparison".into(), TrackKind::Voice);
        let task = tokio::spawn(async move {
            let samples = if after {
                &data.analysis.recommended
            } else {
                &data.analysis.current
            };
            let rate = handle.rate();
            let mut resampler = AudioResampler::new(SAMPLE_RATE, rate);
            let mut output = Vec::new();
            let mut tick = tokio::time::interval(std::time::Duration::from_millis(10));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            for frame in samples.chunks(FRAME_SAMPLES) {
                tick.tick().await;
                let frame = if let Some(resampler) = &mut resampler {
                    resampler.process_into(frame, &mut output);
                    &output[..]
                } else {
                    frame
                };
                handle.push(track, frame, rate as usize / PLAYBACK_CAP_DIVISOR as usize);
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            handle.remove_track(track);
        });
        Ok(Self {
            _playback: playback,
            task,
            faults,
        })
    }
}

#[component]
pub fn MicrophoneTestPanel(mut exclusive: Signal<bool>) -> Element {
    let mut state = crate::state::use_app_state();
    let mut settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let voice = use_voice_tx();
    let mut mode = use_signal(Mode::default);
    let mut elapsed = use_signal(|| 0_u64);
    let mut level = use_signal(|| (0_u32, false));
    let mut error = use_signal(|| None::<String>);
    let mut prepared = use_signal(|| None::<Arc<Prepared>>);
    use_future(move || async move {
        let mut monitor: Option<MicrophoneTest> = None;
        let mut recording: Option<Recording> = None;
        let mut job: Option<Job> = None;
        let mut player: Option<Player> = None;
        let mut playing = Mode::Idle;
        let mut context_checked = Instant::now();
        loop {
            if (!state.peek().audio_settings || in_call(&state.peek()))
                && *mode.peek() != Mode::Idle
            {
                mode.set(Mode::Idle);
            }
            let selected = *mode.peek();
            let locked = !matches!(selected, Mode::Idle | Mode::Monitor);
            if *exclusive.peek() != locked {
                exclusive.set(locked);
            }
            if selected != Mode::Monitor {
                monitor = None;
            }
            if selected != Mode::Record {
                recording = None;
            }
            if selected != Mode::Analyze {
                job = None;
            }
            if !matches!(selected, Mode::Before | Mode::After) {
                player = None;
                playing = Mode::Idle;
            }
            if selected == Mode::Idle && prepared.peek().is_some() {
                prepared.set(None);
            }
            let update: Result<(), String> = async {
                if selected == Mode::Monitor {
                    if monitor.as_ref().is_some_and(|m| m.devices != devices(&state.peek())) { monitor = None; }
                    if monitor.is_none() { monitor = Some(MicrophoneTest::start(state)?); }
                    if let Some(test) = &mut monitor {
                        let next = test.update(&state.peek())?;
                        if *level.peek() != next { level.set(next); }
                    }
                } else if selected == Mode::Record {
                    if recording.is_none() { prepared.set(None); recording = Some(Recording::start(state)?); elapsed.set(0); }
                    if let Some(capture) = &mut recording {
                        capture.collect(&state.peek())?;
                        let seconds = capture.start.elapsed().as_secs();
                        if *elapsed.peek() != seconds { elapsed.set(seconds); }
                        if seconds >= calibration::SECONDS {
                            let context = capture.context.clone();
                            let config = capture.config;
                            let frames = std::mem::take(&mut capture.frames);
                            recording = None;
                            let cancel = Arc::new(AtomicBool::new(false));
                            let worker_cancel = cancel.clone();
                            let handle = tokio::task::spawn_blocking(move || {
                                let analysis = calibration::analyze(frames, config, worker_cancel)?;
                                Ok(Prepared { analysis, context, original: config })
                            });
                            job = Some(Job { handle, cancel });
                            mode.set(Mode::Analyze);
                        }
                    }
                } else if selected == Mode::Analyze {
                    if job.as_ref().is_some_and(|job| job.handle.is_finished()) {
                        if let Some(job) = &mut job {
                            let result = (&mut job.handle).await.map_err(|e| format!("Microphone analysis failed: {e}"))??;
                            if calibration::context(&state.peek())? != result.context { return Err("The audio device changed. Repeat the microphone adjustment.".into()); }
                            prepared.set(Some(Arc::new(result)));
                            mode.set(Mode::Ready);
                        }
                        job = None;
                    }
                } else if matches!(selected, Mode::Ready | Mode::Before | Mode::After) {
                    let data = prepared.peek().clone();
                    if let Some(data) = data {
                        if Config::from_state(&state.peek()) != data.original {
                            return Err("The audio device or settings changed. Repeat the microphone adjustment.".into());
                        }
                        if context_checked.elapsed() > std::time::Duration::from_millis(500) {
                            if calibration::context(&state.peek())? != data.context { return Err("The audio device changed. Repeat the microphone adjustment.".into()); }
                            context_checked = Instant::now();
                        }
                        if matches!(selected, Mode::Before | Mode::After) {
                            if playing != selected { player = None; player = Some(Player::start(state, data, selected == Mode::After)?); playing = selected; }
                            if player.as_mut().is_some_and(|p| p.faults.try_recv().is_ok()) { return Err("The audio device changed. Repeat the microphone adjustment.".into()); }
                            if player.as_ref().is_some_and(|p| p.task.is_finished()) { mode.set(Mode::Ready); }
                        }
                    }
                }
                Ok(())
            }.await;
            if let Err(message) = update {
                error.set(Some(message));
                mode.set(Mode::Idle);
            }
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        }
    });
    let busy = in_call(&state.read());
    let selected = mode();
    let adjusting = !matches!(selected, Mode::Idle | Mode::Monitor);
    let progress = elapsed().min(calibration::SECONDS);
    let instruction = if progress < 3 {
        "Stay quiet while we measure the room."
    } else if progress < 10 {
        "Speak normally: tell us what you did today."
    } else if progress < 13 {
        "Now speak softly, without whispering."
    } else {
        "Stay quiet again."
    };
    rsx! {
        section { class: "mb-4 rounded-lg border border-[var(--border)] p-3",
            div { class: "flex items-center justify-between gap-2 flex-wrap",
                span { class: "text-[13px] font-semibold text-[var(--text)]", {crate::i18n::text("Test and adjust", &[])} }
                div { class: "flex gap-2",
                    button { class: "rounded px-3 py-1 text-[12px] border border-[var(--border)] text-[var(--text)] disabled:opacity-60", disabled: busy || adjusting,
                        onclick: move |_| { error.set(None); mode.set(if mode() == Mode::Monitor { Mode::Idle } else { Mode::Monitor }); },
                        if selected == Mode::Monitor { {crate::i18n::text("Stop test", &[])} } else { {crate::i18n::text("Test microphone", &[])} }
                    }
                    button { class: "rounded px-3 py-1 text-[12px] border border-[var(--border)] text-[var(--text)] disabled:opacity-60", disabled: busy || adjusting,
                        onclick: move |_| { error.set(None); mode.set(Mode::Record); },
                        {crate::i18n::text("Adjust my microphone", &[])}
                    }
                }
            }
            if busy { p { class: "mt-2 text-[12px] text-[var(--text-dim)]", {crate::i18n::text("Leave your voice call to test the microphone.", &[])} } }
            else if selected == Mode::Record {
                p { class: "mt-2 text-[13px] text-[var(--text)]", role: "status", {crate::i18n::label(instruction)} }
                progress { class: "w-full mt-2", max: "15", value: "{progress}", aria_label: crate::i18n::text("Microphone adjustment progress", &[]) }
                p { class: "text-[11px] text-[var(--text-dim)]", "{progress} / 15 s" }
            } else if selected == Mode::Analyze {
                p { class: "mt-2 text-[12px] text-[var(--text-muted)]", role: "status", {crate::i18n::text("Comparing voice profiles on this device…", &[])} }
            } else if let Some(data) = prepared.read().clone() {
                p { class: "mt-2 text-[12px] text-[var(--text)]",
                    {crate::i18n::text("Recommended: {0} · volume {1}% · sensitivity {2}", &[crate::i18n::label(data.analysis.config.processing.profile.label()).to_string(), data.analysis.config.volume.to_string(), peak_to_db_label(data.analysis.config.threshold)])}
                }
                if data.analysis.limited_gain { p { class: "mt-1 text-[11px] text-[var(--text-dim)]", {crate::i18n::text("Your input is quiet. Moving the microphone closer can improve it further.", &[])} } }
                div { class: "flex gap-2 mt-2 flex-wrap",
                    button { class: "rounded px-3 py-1 text-[12px] border border-[var(--border)] text-[var(--text)]", "aria-pressed": selected == Mode::Before, onclick: move |_| mode.set(if mode() == Mode::Before { Mode::Ready } else { Mode::Before }), {crate::i18n::text("Listen to current", &[])} }
                    button { class: "rounded px-3 py-1 text-[12px] border border-[var(--border)] text-[var(--text)]", "aria-pressed": selected == Mode::After, onclick: move |_| mode.set(if mode() == Mode::After { Mode::Ready } else { Mode::After }), {crate::i18n::text("Listen to recommended", &[])} }
                    button { class: "rounded px-3 py-1 text-[12px] border border-[var(--border)] text-[var(--text)]", onclick: move |_| { error.set(None); mode.set(Mode::Record); }, {crate::i18n::text("Repeat adjustment", &[])} }
                    button { class: "rounded px-3 py-1 text-[12px] border border-[var(--border)] text-[var(--text)]",
                        onclick: move |_| {
                            let data = prepared.peek().clone();
                            if let Some(data) = data {
                                if calibration::context(&state.peek()).as_ref() != Ok(&data.context) || Config::from_state(&state.peek()) != data.original { error.set(Some("The audio device or settings changed. Repeat the microphone adjustment.".into())); mode.set(Mode::Idle); return; }
                                let mut next = settings.peek().clone();
                                data.analysis.config.apply_settings(&mut next);
                                crate::mic_profile::remember(&mut next.mic_calibrations, Calibration { version: 1, context: data.context.clone(), config: data.analysis.config });
                                settings.set(next.clone()); crate::settings::save(&next);
                                data.analysis.config.apply_state(&mut state.write());
                                voice.send(VoiceCmd::ApplyMicrophone { config: data.analysis.config });
                                mode.set(Mode::Idle);
                            }
                        }, {crate::i18n::text("Apply recommendation", &[])}
                    }
                }
                p { class: "mt-2 text-[11px] text-[var(--text-dim)]", role: "status",
                    if matches!(selected, Mode::Before | Mode::After) { {crate::i18n::text("Playing your sample. Tap the same button to stop.", &[])} }
                    else { {crate::i18n::text("Compare both samples before applying. Listen for clear, steady speech.", &[])} }
                }
            } else if selected == Mode::Monitor {
                div { class: "w-full h-2 mt-2 rounded-full overflow-hidden", style: "background-color: var(--bg2);",
                    div { class: "h-full rounded-full", style: "width: {peak_to_meter_pct(level().0)}%; background-color: var(--accent);" }
                }
                p { class: "mt-1 text-[11px] text-[var(--text-dim)]", role: "status",
                    if level().1 { {crate::i18n::text("Playing your voice back", &[])} } else { {crate::i18n::text("Below threshold — not transmitting", &[])} }
                }
            } else {
                p { class: "mt-2 text-[12px] text-[var(--text-dim)]", {crate::i18n::text("Use headphones for playback. Adjustment takes 15 seconds, stays on this PC and changes nothing until you apply it.", &[])} }
            }
            if adjusting { button { class: "mt-2 rounded px-3 py-1 text-[12px] border border-[var(--border)] text-[var(--text-muted)]", onclick: move |_| mode.set(Mode::Idle), {crate::i18n::text("Keep my settings", &[])} } }
            if let Some(message) = error() { p { class: "mt-2 text-[12px]", style: "color: var(--danger);", role: "alert", {crate::i18n::message(&message)} } }
        }
    }
}
