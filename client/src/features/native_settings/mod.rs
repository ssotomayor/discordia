mod ui;

use super::{
    settings_dialog::WebSettingsDialog,
    voice::{VoiceCmd, VoiceTx, use_voice_tx},
};
use crate::{
    settings::ClientSettings,
    state::{AppState, ScreenShareStats, TrackStats, VoicePhase},
};
use dioxus::prelude::*;
use parking_lot::Mutex;
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{
        Arc, LazyLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::mpsc;

#[derive(Clone, Debug)]
pub(super) enum Edit {
    Output(Option<String>),
    Input(Option<String>),
    AppVolume(u8),
    Soundboard(u8),
    MicVolume(u16),
    Sensitivity(u32),
    Bypass(bool),
    Agc(bool),
    Noise(bool),
    Suppression(u32),
    Bitrate(u32),
    Camera(Option<String>),
    ScreenQuality(String),
    ScreenAudio(bool),
    AdaptiveQuality(bool),
    GlobalLevel(bool),
    ShareActivity(bool),
    DetectGames(bool),
    DiscordPresence(bool),
    AddGame(String, String),
    RemoveGame(String),
}

impl Edit {
    fn apply(&self, prefs: &mut ClientSettings) -> Result<(), &'static str> {
        match self {
            Self::Output(v) => prefs.selected_output_device = v.clone(),
            Self::Input(v) => prefs.selected_input_device = v.clone(),
            Self::AppVolume(v) => prefs.sfx_volume = (*v).min(100),
            Self::Soundboard(v) => prefs.soundboard_volume = (*v).min(100),
            Self::MicVolume(v) => prefs.mic_volume = (*v).min(200),
            Self::Sensitivity(v) => prefs.mic_sensitivity = (*v).min(i16::MAX as u32),
            Self::Bypass(v) => prefs.bypass_system_audio_processing = *v,
            Self::Agc(v) => prefs.auto_gain_control = *v,
            Self::Noise(v) => prefs.noise_cancellation = *v,
            Self::Suppression(v) => {
                prefs.denoise_atten_lim_db = (*v).clamp(
                    super::voice::DENOISE_ATTEN_LIM_DB_MIN,
                    super::voice::DENOISE_ATTEN_LIM_DB_MAX,
                )
            }
            Self::Bitrate(v) => prefs.voice_bitrate_kbps = if *v == 24 { 24 } else { 48 },
            Self::Camera(v) => prefs.camera_device_id = v.clone(),
            Self::ScreenQuality(v) => {
                if !super::screenshare::QUALITY_PRESETS
                    .iter()
                    .any(|(id, _, _)| *id == v)
                {
                    return Err("Unknown screen-share quality.");
                }
                prefs.screenshare_quality = v.clone();
                prefs.screenshare_fps = None;
            }
            Self::ScreenAudio(v) => prefs.screenshare_audio = *v,
            Self::AdaptiveQuality(v) => prefs.screenshare_adaptive_quality = *v,
            Self::GlobalLevel(v) => prefs.publish_global_level = *v,
            Self::ShareActivity(v) => prefs.share_activity = *v,
            Self::DetectGames(v) => prefs.detect_games = *v,
            Self::DiscordPresence(v) => prefs.discord_rpc_socket = *v,
            Self::AddGame(exe, name) => {
                crate::presence::detect::set_override(&mut prefs.detect_extra, exe, name)?
            }
            Self::RemoveGame(exe) => prefs.detect_extra.retain(|(key, _)| key != exe),
        }
        Ok(())
    }
}

pub(super) struct Snapshot {
    prefs: ClientSettings,
    accent: Option<String>,
    ack: u64,
    inputs: Vec<String>,
    outputs: Vec<String>,
    cameras: Vec<(String, String)>,
    reconnecting: bool,
    microphone_active: bool,
    microphone_error: bool,
    level: u32,
    pre_level: u32,
    muted: bool,
    speaking: bool,
    bypass_error: Option<String>,
    voice_connected: bool,
    activity: Option<String>,
    voice_stats: Vec<(String, TrackStats)>,
    outbound: Option<ScreenShareStats>,
    inbound: Option<ScreenShareStats>,
}

fn snapshot(state: &AppState, prefs: &ClientSettings, ack: u64) -> Snapshot {
    let dm = state.dm_call.as_ref();
    let microphone_error = dm.is_some_and(|call| call.microphone_error.is_some());
    let reconnecting = state.voice.phase == VoicePhase::Connecting
        || dm.is_some_and(|call| {
            matches!(
                call.phase,
                super::dm_call::Phase::Connecting | super::dm_call::Phase::Reconnecting
            )
        });
    let voice_connected = state.voice.phase == VoicePhase::Connected;
    let microphone_active = !microphone_error
        && (voice_connected
            || dm.is_some_and(|call| call.phase == super::dm_call::Phase::Connected));
    let mut voice_stats: Vec<_> = state
        .voice_stats
        .iter()
        .map(|(pk, stats)| (state.display_name(pk), *stats))
        .collect();
    voice_stats.sort_by(|a, b| a.0.cmp(&b.0));
    Snapshot {
        prefs: prefs.clone(),
        accent: super::workspace::guild_accent_to_apply(
            state.dm_mode,
            prefs.keep_my_accent,
            state
                .selected_guild
                .and_then(|id| state.guilds.iter().find(|guild| guild.id == id))
                .and_then(|guild| guild.accent.clone()),
        )
        .or_else(|| prefs.accent.clone()),
        ack,
        inputs: state.available_input_devices.clone(),
        outputs: state.available_output_devices.clone(),
        cameras: state
            .available_cameras
            .iter()
            .enumerate()
            .map(|(i, c)| {
                (
                    c.id.clone(),
                    if c.label.is_empty() {
                        format!("Camera {}", i + 1)
                    } else {
                        c.label.clone()
                    },
                )
            })
            .collect(),
        reconnecting,
        microphone_active,
        microphone_error,
        level: state.mic_gate_level,
        pre_level: state.mic_level_pre,
        muted: state.voice.muted,
        speaking: state.voice.speaking,
        bypass_error: state.mic_bypass_error.clone(),
        voice_connected,
        activity: state
            .self_user
            .as_ref()
            .and_then(|user| state.activity_of(&user.pubkey))
            .map(|activity| activity.name.clone()),
        voice_stats,
        outbound: state.screen_share_stats.clone(),
        inbound: state.screen_share_in_stats.clone(),
    }
}

pub(super) enum Event {
    Edit(u64, Edit, bool),
    TestSound,
    Stats(bool),
    Closed,
    Failed(String),
}

pub(super) struct Session {
    snapshot: Mutex<Option<Snapshot>>,
    context: Mutex<Option<eframe::egui::Context>>,
    close: AtomicBool,
    events: mpsc::UnboundedSender<Event>,
}

impl Session {
    fn refresh(&self, next: Snapshot) {
        *self.snapshot.lock() = Some(next);
        if let Some(ctx) = self.context.lock().as_ref() {
            ctx.request_repaint();
        }
    }
    fn close(&self) {
        self.close.store(true, Ordering::Release);
        if let Some(ctx) = self.context.lock().as_ref() {
            ctx.request_repaint();
        }
    }
    pub(super) fn send(&self, event: Event) {
        if self.events.send(event).is_err() {
            self.close();
        }
    }

    fn wants_reopen(&self, desired_open: bool) -> bool {
        desired_open && self.close.load(Ordering::Acquire)
    }
}

static RUNNER: LazyLock<Result<std::sync::mpsc::Sender<Arc<Session>>, String>> =
    LazyLock::new(|| {
        let (tx, rx) = std::sync::mpsc::channel::<Arc<Session>>();
        std::thread::Builder::new()
            .name("discordia-settings".into())
            .spawn(move || {
                while let Ok(session) = rx.recv() {
                    if session.close.load(Ordering::Acquire) {
                        session.send(Event::Closed);
                        continue;
                    }
                    match ui::run(session.clone()) {
                        Ok(()) => session.send(Event::Closed),
                        Err(error) => session.send(Event::Failed(error)),
                    }
                }
            })
            .map_err(|e| format!("Couldn't start native Settings: {e}"))?;
        Ok(tx)
    });

fn stats_polling(voice: &VoiceTx, enabled: bool) {
    voice.send(VoiceCmd::SetStatsPolling { enabled });
    let _eval = document::eval(&super::screenshare::screen_stats_js(enabled));
}

fn complete_window_close(
    session: Option<&Session>,
    mut state: Signal<AppState>,
    mut revision: Signal<u64>,
) {
    let reopen = session.is_some_and(|session| session.wants_reopen(state.peek().audio_settings));
    if !reopen {
        state.write().audio_settings = false;
    }
    // Taking the nonreactive session must wake the effect for queued reopen requests.
    revision += 1;
}

fn apply_edit(
    edit: &Edit,
    mut state: Signal<AppState>,
    mut settings: Signal<ClientSettings>,
    voice: &VoiceTx,
    gateway: &crate::state::GatewayTx,
    persist: bool,
) {
    let mut next = settings.read().clone();
    if let Err(error) = edit.apply(&mut next) {
        state.write().error_toast = Some(error.into());
        return;
    }
    match edit {
        Edit::Output(_) => {
            state.write().selected_output_device = next.selected_output_device.clone();
            voice.send(VoiceCmd::SetDevices {
                input: None,
                output: next.selected_output_device.clone(),
            });
        }
        Edit::Input(_) => {
            state.write().selected_input_device = next.selected_input_device.clone();
            voice.send(VoiceCmd::SetDevices {
                input: next.selected_input_device.clone(),
                output: None,
            });
        }
        Edit::Soundboard(_) => {
            state.write().soundboard_volume = next.soundboard_volume as u32;
            voice.send(VoiceCmd::SetSoundboardVolume {
                percent: next.soundboard_volume as u32,
            });
        }
        Edit::MicVolume(_) => {
            state.write().mic_volume = next.mic_volume;
            voice.send(VoiceCmd::SetMicVolume {
                percent: next.mic_volume,
            });
        }
        Edit::Sensitivity(_) => {
            state.write().mic_sensitivity = next.mic_sensitivity;
            voice.send(VoiceCmd::SetSensitivity {
                threshold: next.mic_sensitivity,
            });
        }
        Edit::Bypass(_) => {
            state.write().bypass_system_audio_processing = next.bypass_system_audio_processing;
            voice.send(VoiceCmd::SetBypassSystemProcessing {
                enabled: next.bypass_system_audio_processing,
            });
        }
        Edit::Agc(_) => {
            state.write().auto_gain_control = next.auto_gain_control;
            voice.send(VoiceCmd::SetAutoGainControl {
                enabled: next.auto_gain_control,
            });
        }
        Edit::Noise(_) => {
            state.write().noise_cancellation = next.noise_cancellation;
            voice.send(VoiceCmd::SetNoiseCancellation {
                enabled: next.noise_cancellation,
            });
        }
        Edit::Suppression(_) => {
            state.write().denoise_atten_lim_db = next.denoise_atten_lim_db;
            voice.send(VoiceCmd::SetDenoiseAttenLim {
                db: next.denoise_atten_lim_db,
            });
        }
        Edit::Bitrate(_) => {
            state.write().voice_bitrate_kbps = next.voice_bitrate_kbps;
            voice.send(VoiceCmd::SetVoiceBitrate {
                kbps: next.voice_bitrate_kbps,
            });
        }
        Edit::Camera(id) => {
            next.camera_device_label = state
                .read()
                .available_cameras
                .iter()
                .find(|c| Some(&c.id) == id.as_ref())
                .map(|c| c.label.clone())
                .filter(|s| !s.is_empty())
        }
        _ => {}
    }
    let restart_camera = matches!(edit, Edit::Camera(_)) && state.read().camera_on;
    settings.set(next.clone());
    if persist {
        crate::settings::save(&next);
    }
    if restart_camera {
        super::camera::toggle_camera(state, settings, gateway, false);
        super::camera::toggle_camera(state, settings, gateway, true);
    }
}

#[component]
pub(super) fn NativeSettingsDialog() -> Element {
    let mut state = crate::state::use_app_state();
    let settings = use_context::<Signal<ClientSettings>>();
    let voice = use_voice_tx();
    let gateway = crate::state::use_gateway();
    let active = use_hook(|| Rc::new(RefCell::new(None::<Arc<Session>>)));
    let window_revision = use_signal(|| 0_u64);
    let mut fallback = use_signal(|| false);
    let (tx, rx) = use_hook(|| {
        let (tx, rx) = mpsc::unbounded_channel();
        (tx, Rc::new(RefCell::new(Some(rx))))
    });
    {
        let active = active.clone();
        let voice = voice.clone();
        use_effect(move || {
            let _revision = window_revision();
            if state.read().audio_settings && !fallback() && active.borrow().is_none() {
                let session = Arc::new(Session {
                    snapshot: Mutex::new(Some(snapshot(&state.read(), &settings.read(), 0))),
                    context: Mutex::new(None),
                    close: AtomicBool::new(false),
                    events: tx.clone(),
                });
                match RUNNER
                    .as_ref()
                    .map_err(Clone::clone)
                    .and_then(|runner| runner.send(session.clone()).map_err(|e| e.to_string()))
                {
                    Ok(()) => {
                        *active.borrow_mut() = Some(session);
                        voice.send(VoiceCmd::ListDevices);
                    }
                    Err(error) => {
                        state.write().error_toast = Some(error);
                        fallback.set(true);
                    }
                }
            } else if !state.read().audio_settings
                && let Some(session) = active.borrow().as_ref()
            {
                session.close();
            }
        });
    }
    {
        let active = active.clone();
        let voice = voice.clone();
        use_future(move || {
            let mut receiver = rx.borrow_mut().take();
            let active = active.clone();
            let voice = voice.clone();
            let gateway = gateway.clone();
            async move {
                let Some(receiver) = receiver.as_mut() else {
                    return;
                };
                let mut ticker = tokio::time::interval(Duration::from_millis(200));
                let mut ack = 0;
                loop {
                    tokio::select! {
                        event = receiver.recv() => match event {
                            Some(Event::Edit(id, edit, persist)) => { apply_edit(&edit, state, settings, &voice, &gateway, persist); ack = id; }
                            Some(Event::TestSound) => { let prefs = settings.read(); crate::native_sounds::configure(prefs.sfx_volume, prefs.selected_output_device.clone()); crate::native_sounds::play("call-connected"); }
                            Some(Event::Stats(enabled)) => stats_polling(&voice, enabled),
                            Some(Event::Closed) => {
                                let closed = active.borrow_mut().take();
                                complete_window_close(closed.as_deref(), state, window_revision);
                                stats_polling(&voice, false);
                                crate::settings::save(&settings.read());
                                ack = 0;
                            }
                            Some(Event::Failed(error)) => { active.borrow_mut().take(); state.write().error_toast = Some(format!("{error}. Using the existing Settings dialog.")); fallback.set(true); stats_polling(&voice, false); }
                            None => break,
                        },
                        _ = ticker.tick() => {
                            if state.peek().audio_settings && let Some(session) = active.borrow().as_ref() {
                                session.refresh(snapshot(&state.peek(), &settings.peek(), ack));
                            }
                        }
                    }
                }
            }
        });
    }
    use_drop(move || {
        if let Some(session) = active.borrow_mut().take() {
            session.close();
        }
        stats_polling(&voice, false);
    });
    if fallback() {
        rsx! { WebSettingsDialog {} }
    } else {
        rsx! {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn delayed_window_close_preserves_only_an_explicit_reopen_request() {
        let (events, _receiver) = mpsc::unbounded_channel();
        let session = Session {
            snapshot: Mutex::new(None),
            context: Mutex::new(None),
            close: AtomicBool::new(false),
            events,
        };
        let mut dom = VirtualDom::new_with_props(
            |session: Arc<Session>| {
                let mut state = use_signal(AppState::empty);
                let revision = use_signal(|| 0_u64);
                use_hook(|| {
                    state.write().audio_settings = true;
                    complete_window_close(Some(&session), state, revision);
                    assert!(!state.read().audio_settings, "window X closes Settings");
                    assert_eq!(revision(), 1);

                    session.close();
                    complete_window_close(Some(&session), state, revision);
                    assert!(!state.read().audio_settings, "a closed dialog stays closed");
                    assert_eq!(revision(), 2);

                    state.write().audio_settings = true;
                    complete_window_close(Some(&session), state, revision);
                    assert!(
                        state.read().audio_settings,
                        "a reopen request survives teardown"
                    );
                    assert_eq!(revision(), 3, "the opening effect is notified");
                });
                rsx! {}
            },
            Arc::new(session),
        );
        dom.rebuild_in_place();
    }

    #[test]
    fn native_microphone_and_playback_edits_reach_the_existing_voice_service() {
        let (voice_tx, mut commands) = mpsc::unbounded_channel();
        let (gateway, _outbound) = mpsc::unbounded_channel();
        let mut dom = VirtualDom::new_with_props(
            |(voice, gateway): (VoiceTx, crate::state::GatewayTx)| {
                let state = use_signal(AppState::empty);
                let prefs = use_signal(ClientSettings::default);
                use_hook(|| {
                    for edit in [
                        Edit::MicVolume(150),
                        Edit::Soundboard(35),
                        Edit::Input(Some("Mic".into())),
                        Edit::Output(Some("Headphones".into())),
                        Edit::Agc(false),
                        Edit::Noise(false),
                        Edit::Bypass(true),
                    ] {
                        apply_edit(&edit, state, prefs, &voice, &gateway, false);
                    }
                    assert_eq!(prefs.read().mic_volume, 150);
                    assert_eq!(state.read().mic_volume, 150);
                    assert_eq!(state.read().soundboard_volume, 35);
                    assert_eq!(state.read().selected_input_device.as_deref(), Some("Mic"));
                });
                rsx! {}
            },
            (VoiceTx(voice_tx), crate::state::GatewayTx(gateway)),
        );
        dom.rebuild_in_place();
        assert!(matches!(
            commands.try_recv().unwrap(),
            VoiceCmd::SetMicVolume { percent: 150 }
        ));
        assert!(matches!(
            commands.try_recv().unwrap(),
            VoiceCmd::SetSoundboardVolume { percent: 35 }
        ));
        assert!(
            matches!(commands.try_recv().unwrap(),VoiceCmd::SetDevices {input:Some(name),output:None} if name=="Mic")
        );
        assert!(
            matches!(commands.try_recv().unwrap(),VoiceCmd::SetDevices {input:None,output:Some(name)} if name=="Headphones")
        );
        assert!(matches!(
            commands.try_recv().unwrap(),
            VoiceCmd::SetAutoGainControl { enabled: false }
        ));
        assert!(matches!(
            commands.try_recv().unwrap(),
            VoiceCmd::SetNoiseCancellation { enabled: false }
        ));
        assert!(matches!(
            commands.try_recv().unwrap(),
            VoiceCmd::SetBypassSystemProcessing { enabled: true }
        ));
    }

    #[test]
    #[ignore = "opens a real native Settings window twice; DISCORDIA_NATIVE_SETTINGS_PROBE exports tab screenshots"]
    fn native_settings_window_closes_and_reopens_without_a_webview() {
        use dioxus::desktop::tao::{
            event_loop::EventLoopBuilder, platform::windows::EventLoopBuilderExtWindows,
        };
        let mut event_builder = EventLoopBuilder::<()>::new();
        event_builder.with_any_thread(true);
        let event_loop = event_builder.build();
        let app_window = crate::desktop_window_builder()
            .with_title("Discordia — window coexistence test")
            .with_visible(false)
            .build(&event_loop)
            .expect("Tao app window");
        let directory =
            std::env::var_os("DISCORDIA_NATIVE_SETTINGS_PROBE").expect("set screenshot directory");
        for _ in 0..2 {
            let (events, mut receiver) = mpsc::unbounded_channel();
            let mut state = AppState::empty();
            state.available_input_devices = vec!["Microphone (Realtek Audio)".into()];
            state.available_output_devices = vec!["Headphones (Realtek Audio)".into()];
            state.voice.phase = VoicePhase::Connected;
            state.mic_gate_level = 900;
            state.mic_level_pre = 2200;
            let session = Arc::new(Session {
                snapshot: Mutex::new(Some(snapshot(&state, &ClientSettings::default(), 0))),
                context: Mutex::new(None),
                close: AtomicBool::new(false),
                events,
            });
            RUNNER.as_ref().unwrap().send(session.clone()).unwrap();
            loop {
                match receiver.blocking_recv().expect("native window result") {
                    Event::Closed => break,
                    Event::Failed(error) => panic!("{error}"),
                    _ => {}
                }
            }
            assert!(session.context.lock().is_some());
            app_window.set_title("Discordia — Tao window still responds");
        }
        for name in ["audio", "microphone", "video", "activity", "diagnostics"] {
            assert!(
                std::path::Path::new(&directory)
                    .join(format!("{name}.png"))
                    .is_file()
            );
        }
    }
    #[test]
    fn native_edits_preserve_unrelated_preferences_and_validate_audio_ranges() {
        let mut prefs = ClientSettings {
            theme: "violet".into(),
            mic_volume: 120,
            ..Default::default()
        };
        Edit::AppVolume(255).apply(&mut prefs).unwrap();
        Edit::MicVolume(500).apply(&mut prefs).unwrap();
        Edit::Bitrate(1000).apply(&mut prefs).unwrap();
        Edit::Suppression(u32::MAX).apply(&mut prefs).unwrap();
        Edit::AdaptiveQuality(true).apply(&mut prefs).unwrap();
        assert!(prefs.screenshare_adaptive_quality);
        assert_eq!(prefs.sfx_volume, 100);
        assert_eq!(prefs.mic_volume, 200);
        assert_eq!(prefs.voice_bitrate_kbps, 48);
        assert_eq!(
            prefs.denoise_atten_lim_db,
            super::super::voice::DENOISE_ATTEN_LIM_DB_MAX
        );
        assert_eq!(prefs.theme, "violet");
        let before = prefs.clone();
        assert!(
            Edit::ScreenQuality("invalid".into())
                .apply(&mut prefs)
                .is_err()
        );
        assert_eq!(prefs, before);
    }
    #[test]
    fn native_game_edits_use_the_same_validation_and_replace_existing_entries() {
        let mut prefs = ClientSettings::default();
        Edit::AddGame(" Game.EXE ".into(), "Game".into())
            .apply(&mut prefs)
            .unwrap();
        Edit::AddGame("game.exe".into(), "Renamed".into())
            .apply(&mut prefs)
            .unwrap();
        assert_eq!(
            prefs.detect_extra,
            vec![("game.exe".into(), "Renamed".into())]
        );
        assert!(
            Edit::AddGame("../bad.exe".into(), "Bad".into())
                .apply(&mut prefs)
                .is_err()
        );
        Edit::RemoveGame("game.exe".into())
            .apply(&mut prefs)
            .unwrap();
        assert!(prefs.detect_extra.is_empty());
    }
}
