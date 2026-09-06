//! The app's own settings, in a dialog rather than a floating panel.
//!
//! It lives here, mounted at the workspace root, and not inside `UserPanel`
//! where it was written. A raised grid panel carries a `z-index`, that is a
//! stacking context, and a `position: fixed` child of one is trapped inside it
//! — so the panel opened *underneath* its neighbours until it was dragged,
//! which raised it. Nothing about the fix is cosmetic; see `grid-layout`'s own
//! note on the same trap in `item.rs`.

use dioxus::prelude::*;

use crate::features::voice::use_voice_tx;
use crate::state::{VoicePhase, use_app_state};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SettingsTab {
    Audio,
    Mic,
    Video,
    Activity,
    Diagnostics,
}

/// The empty group label means "same group as the row above" — the heading is
/// drawn by the first tab that opens one.
const SETTINGS_TABS: &[(&str, SettingsTab, &str, &str)] = &[
    (
        "Voice",
        SettingsTab::Audio,
        "Audio",
        crate::features::icons::SPEAKER,
    ),
    (
        "",
        SettingsTab::Mic,
        "Microphone",
        crate::features::icons::MIC,
    ),
    (
        "Picture",
        SettingsTab::Video,
        "Video and screen",
        crate::features::icons::CAMERA,
    ),
    (
        "Status",
        SettingsTab::Activity,
        "Level and activity",
        crate::features::icons::GAMEPAD,
    ),
    (
        "",
        SettingsTab::Diagnostics,
        "Diagnostics",
        crate::features::icons::ACTIVITY,
    ),
];

#[component]
pub fn SettingsDialog() -> Element {
    let mut state = use_app_state();
    let voice = use_voice_tx();
    let mut settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let persist_settings = move |_: FormEvent| crate::settings::save(&settings.read());

    let mut show_stats = use_signal(|| false);
    let mut settings_tab = use_signal(|| SettingsTab::Audio);

    if !state.read().audio_settings {
        return rsx! { Fragment {} };
    }

    let selected_camera_id = settings.read().camera_device_id.clone();
    let mic_sensitivity = state.read().mic_sensitivity;
    let mic_level = state.read().mic_level;
    let noise_cancellation = state.read().noise_cancellation;
    let atten_lim_db = state.read().denoise_atten_lim_db;
    let mic_volume = state.read().mic_volume;
    let auto_gain_control = state.read().auto_gain_control;
    let bypass_supported = crate::rawmic::supported();
    let bypass_system_audio = state.read().bypass_system_audio_processing;
    let bypass_error = state.read().mic_bypass_error.clone();
    let voice_bitrate_kbps = state.read().voice_bitrate_kbps;

    let v_for_input_change = voice.clone();
    let v_for_output_change = voice.clone();
    let v_for_sensitivity = voice.clone();
    let v_for_denoise = voice.clone();
    let v_for_atten = voice.clone();
    let v_for_mic_volume = voice.clone();
    let v_for_agc = voice.clone();
    let v_for_bypass = voice.clone();
    let v_for_bitrate = voice.clone();

    let voice_phase = state.read().voice.phase;
    let reconnecting = matches!(voice_phase, VoicePhase::Connecting);
    let mic_level_pct = crate::features::voice::peak_to_meter_pct(mic_level);
    let mic_level_pre = state.read().mic_level_pre;
    let mic_level_pre_pct = crate::features::voice::peak_to_meter_pct(mic_level_pre);
    // Either one moves the level the threshold is then judged against, and the
    // AGC moves it *up*, so the old `pre > post` test could never fire for it.
    let show_pre = (noise_cancellation || auto_gain_control) && mic_level_pre != mic_level;
    let mic_level_display = crate::features::voice::peak_to_db_label(mic_level);
    let threshold_pct = crate::features::voice::peak_to_meter_pct(mic_sensitivity);
    let sensitivity_display = crate::features::voice::peak_to_db_label(mic_sensitivity);
    let pre_caption = match (noise_cancellation, auto_gain_control) {
        (true, false) => {
            "Grey tick: your raw microphone. The gap is what noise cancellation removed."
        }
        (false, true) => {
            "Grey tick: your raw microphone. Auto gain moves the level the white threshold is \
             judged against, so the threshold is not a microphone level."
        }
        (true, true) => {
            "Grey tick: your raw microphone. Noise cancellation and auto gain both move the level \
             before the white threshold sees it."
        }
        (false, false) => "",
    };

    let available_input_devices = state.read().available_input_devices.clone();
    let available_output_devices = state.read().available_output_devices.clone();
    let selected_input_device = state.read().selected_input_device.clone();
    let selected_output_device = state.read().selected_output_device.clone();
    let available_cameras = state.read().available_cameras.clone();

    let screenshare_quality = settings.read().screenshare_quality.clone();
    let screenshare_audio = settings.read().screenshare_audio;
    let screenshare_hint = crate::features::screenshare::QUALITY_PRESETS
        .iter()
        .find(|(id, _, _)| *id == screenshare_quality)
        .map(|(_, _, hint)| *hint)
        .unwrap_or("");

    let self_voice = state.read().voice.clone();
    let muted = self_voice.muted;
    let gate_open = self_voice.speaking;

    rsx! {
        div {
            class: "dxf-backdrop-in fixed inset-0 z-[70] flex items-center justify-center bg-black/50 p-6",
            onclick: move |_| state.write().audio_settings = false,
            div {
                // A fixed height, not a shrink-wrap: the tallest tab decides it once,
                // so switching tabs moves nothing. `max-h-full` still gives way on
                // a short window, and the right pane scrolls inside either way.
                class: "dxf-modal-in w-[780px] h-[560px] max-w-full max-h-full flex flex-col bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg shadow-xl overflow-hidden",
                onclick: move |e: MouseEvent| e.stop_propagation(),

                div { class: "h-8 px-2 flex items-center gap-2 border-b border-[var(--border)] shrink-0 select-none",
                    span { class: "text-[11px] font-medium text-[var(--text)] flex-1", "Settings" }
                    button {
                        class: "text-[var(--text-dim)] hover:text-[var(--text)] text-base leading-none",
                        onclick: move |_| state.write().audio_settings = false,
                        "✕"
                    }
                }

                    div { class: "flex-1 flex min-h-0",
                        div { class: "w-48 shrink-0 border-r border-[var(--border)] bg-[var(--panel2)] p-2 flex flex-col gap-0.5 overflow-y-auto",
                            for (group, t, label, icon) in SETTINGS_TABS.iter().copied() {
                                {
                                    let selected = settings_tab() == t;
                                    let cls = if selected {
                                        "relative w-full h-9 px-2.5 flex items-center gap-2 rounded-lg text-left text-[13px] font-medium bg-[var(--panel-solid)] text-[var(--text)]"
                                    } else {
                                        "relative w-full h-9 px-2.5 flex items-center gap-2 rounded-lg text-left text-[13px] text-[var(--text-dim)] hover:text-[var(--text-muted)] hover:bg-white/[0.03] transition-colors"
                                    };
                                    rsx! {
                                        Fragment { key: "{label}",
                                            if !group.is_empty() {
                                                div { class: "px-2 pt-3 pb-1.5 font-mono text-[9px] uppercase tracking-[0.18em] text-[var(--text-dim)]",
                                                    "{group}"
                                                }
                                            }
                                            button {
                                                class: "{cls}",
                                                onclick: move |_| settings_tab.set(t),
                                                if selected {
                                                    span {
                                                        class: "absolute",
                                                        style: "left:-8px; top:50%; transform:translateY(-50%); width:3px; height:18px; border-radius:0 3px 3px 0; background: var(--accent);",
                                                    }
                                                }
                                                span { class: "block w-4 h-4 shrink-0", dangerous_inner_html: icon }
                                                "{label}"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        div { class: "flex-1 min-w-0 overflow-y-auto p-4",
                        if settings_tab() == SettingsTab::Audio {
                        h3 { class: "dxf-display text-[17px] font-bold tracking-tight text-[var(--text)]", "Audio" }
                        p { class: "mt-0.5 mb-4 text-[12.5px] text-[var(--text-dim)]",
                            "Where sound comes out, and how loud the cues are."
                        }
                        div { class: "mb-2",
                            span { class: "text-[11px] text-[var(--text-muted)]", "Output" }
                            select {
                                class: "w-full mt-1 bg-[var(--panel-solid)] text-[var(--text)] border border-[var(--border)] rounded px-2 py-1 text-sm disabled:opacity-60",
                                style: "color: var(--text); background: var(--panel-solid);",
                                disabled: "{reconnecting}",
                                onchange: move |e| {
                                    let val = e.value();
                                    let val_cloned = val.clone();
                                    let mut next = settings.read().clone();
                                    if val_cloned.is_empty() { next.selected_output_device = None; } else { next.selected_output_device = Some(val_cloned.clone()); }
                                    settings.set(next.clone());
                                    crate::settings::save(&next);

                                    let mut s = state.write();
                                    s.selected_output_device = if val_cloned.is_empty() { None } else { Some(val_cloned.clone()) };
                                    let v = v_for_output_change.clone();
                                    v.send(crate::features::voice::VoiceCmd::SetDevices { input: None, output: s.selected_output_device.clone() });
                                },
                                option { value: "", "System default" }
                                for dev in available_output_devices.iter() {
                                    option { selected: selected_output_device.as_ref().map(|n| n == dev).unwrap_or(false), value: "{dev}", "{dev}" }
                                }
                            }
                        }
                        div { class: "mb-2",
                            span { class: "text-[11px] text-[var(--text-muted)]", "Sound effects" }
                            div { class: "flex items-center gap-2 mt-1",
                                input {
                                    r#type: "range",
                                    min: "0",
                                    max: "100",
                                    value: "{settings.read().sfx_volume}",
                                    class: "flex-1 accent-[var(--accent)]",
                                    title: "UI sound effects volume",
                                    oninput: move |e| {
                                        let val: u8 = e.value().parse().unwrap_or(70).min(100);
                                        let mut next = settings.read().clone();
                                        next.sfx_volume = val;
                                        settings.set(next);
                                        let v = val as f32 / 100.0;
                                        let _ = document::eval(&format!(
                                            "window.dxSfx && window.dxSfx.setVolume({v});"
                                        ));
                                    },
                                    onchange: persist_settings,
                                }
                                span { class: "text-[10px] text-[var(--text-dim)] w-8 text-right", "{settings.read().sfx_volume}%" }
                            }
                        }
                        }
                        if settings_tab() == SettingsTab::Mic {
                        h3 { class: "dxf-display text-[17px] font-bold tracking-tight text-[var(--text)]", "Microphone" }
                        p { class: "mt-0.5 mb-4 text-[12.5px] text-[var(--text-dim)]",
                            "What others hear, when the gate opens, and what is processed before it leaves."
                        }
                        div { class: "mb-2",
                            span { class: "text-[11px] text-[var(--text-muted)]", "Input" }
                            select {
                                class: "w-full mt-1 bg-[var(--panel-solid)] text-[var(--text)] border border-[var(--border)] rounded px-2 py-1 text-sm disabled:opacity-60",
                                style: "color: var(--text); background: var(--panel-solid);",
                                disabled: "{reconnecting}",
                                onchange: move |e| {
                                    let val = e.value();
                                    let val_cloned = val.clone();
                                    let mut next = settings.read().clone();
                                    if val_cloned.is_empty() { next.selected_input_device = None; } else { next.selected_input_device = Some(val_cloned.clone()); }
                                    settings.set(next.clone());
                                    crate::settings::save(&next);

                                    let mut s = state.write();
                                    s.selected_input_device = if val_cloned.is_empty() { None } else { Some(val_cloned.clone()) };
                                    let v = v_for_input_change.clone();
                                    v.send(crate::features::voice::VoiceCmd::SetDevices { input: s.selected_input_device.clone(), output: None });
                                },
                                option { value: "", "System default" }
                                for dev in available_input_devices.iter() {
                                    option { selected: selected_input_device.as_ref().map(|n| n == dev).unwrap_or(false), value: "{dev}", "{dev}" }
                                }
                            }
                        }
                        div { class: "mb-2",
                            div { class: "flex items-center justify-between",
                                span { class: "text-[11px] text-[var(--text-muted)]", "Level" }
                                if reconnecting {
                                    span { class: "text-[10px] text-[var(--text-dim)]", "reconnecting" }
                                } else if voice_phase == VoicePhase::Connected {
                                    span { class: "text-[10px] text-[var(--text-dim)]", "{mic_level_display}" }
                                } else {
                                    span { class: "text-[10px] text-[var(--text-dim)]", "join voice" }
                                }
                            }
                            if voice_phase == VoicePhase::Connected && !reconnecting {
                                div {
                                    class: "relative w-full h-2 mt-1 rounded-full overflow-hidden",
                                    style: "background: var(--bg2);",
                                    div {
                                        class: "absolute inset-y-0 left-0 rounded-full transition-all duration-75",
                                        style: "width: {mic_level_pct}%; background: linear-gradient(90deg, var(--up), var(--accent), var(--danger));",
                                    }
                                    // A tick, not the faint bar this replaced: the AGC pushes
                                    // the raw level *below* the drawn one, where a bar hides.
                                    if show_pre {
                                        div {
                                            class: "absolute top-0 bottom-0 w-0.5 bg-[var(--text-dim)] pointer-events-none",
                                            style: "left: {mic_level_pre_pct}%;",
                                        }
                                    }
                                    div {
                                        class: "absolute top-0 bottom-0 w-0.5 bg-white/70 pointer-events-none",
                                        style: "left: {threshold_pct}%;",
                                    }
                                }
                                if show_pre {
                                    span { class: "text-[10px] text-[var(--text-dim)] mt-0.5 block",
                                        "{pre_caption}"
                                    }
                                }
                            } else {
                                div {
                                    class: "w-full h-2 mt-1 rounded-full",
                                    style: "background: var(--bg2);",
                                }
                            }
                        }
                        div { class: "mb-2",
                            div { class: "flex items-center justify-between",
                                span { class: "text-[11px] text-[var(--text-muted)]", "Microphone Input" }
                                span { class: "text-[10px] text-[var(--text-dim)]", "{mic_volume}%" }
                            }
                            input {
                                r#type: "range",
                                min: "0",
                                max: "200",
                                value: "{mic_volume}",
                                disabled: auto_gain_control,
                                class: if auto_gain_control { "w-full mt-1 accent-[var(--accent)] opacity-40 cursor-not-allowed" } else { "w-full mt-1 accent-[var(--accent)]" },
                                oninput: move |e| {
                                    let pct: u16 = e.value().parse().unwrap_or(100).min(200);
                                    let mut next = settings.read().clone();
                                    next.mic_volume = pct;
                                    settings.set(next);
                                    state.write().mic_volume = pct;
                                    let v = v_for_mic_volume.clone();
                                    v.send(crate::features::voice::VoiceCmd::SetMicVolume { percent: pct });
                                },
                                onchange: persist_settings,
                            }
                            if auto_gain_control {
                                span { class: "text-[10px] text-[var(--text-dim)] mt-0.5 block",
                                    "Auto gain is setting your level, so this does nothing. Turn it off below to set it by hand."
                                }
                            }
                        }
                        div { class: "mb-2",
                            div { class: "flex items-center justify-between",
                                span { class: "text-[11px] text-[var(--text-muted)]", "Sensitivity" }
                                span { class: "text-[10px] text-[var(--text-dim)]", "{sensitivity_display}" }
                            }
                            input {
                                r#type: "range",
                                min: "0",
                                max: "100",
                                value: "{threshold_pct}",
                                class: "w-full mt-1 accent-[var(--accent)]",
                                oninput: move |e| {
                                    let pct: u32 = e.value().parse().unwrap_or(40).clamp(0, 100);
                                    let val = crate::features::voice::meter_pct_to_peak(pct);
                                    let mut next = settings.read().clone();
                                    next.mic_sensitivity = val;
                                    settings.set(next);
                                    state.write().mic_sensitivity = val;
                                    let v = v_for_sensitivity.clone();
                                    v.send(crate::features::voice::VoiceCmd::SetSensitivity { threshold: val });
                                },
                                onchange: persist_settings,
                            }
                            if voice_phase == VoicePhase::Connected && !reconnecting {
                                if muted {
                                    span { class: "text-[10px] text-[var(--text-dim)] mt-0.5 block", "Muted" }
                                } else if gate_open {
                                    span { class: "text-[10px] mt-0.5 block", style: "color: var(--up);", "Transmitting" }
                                } else {
                                    span { class: "text-[10px] text-[var(--text-dim)] mt-0.5 block", "Below threshold — not transmitting" }
                                }
                            }
                        }
                        if bypass_supported {
                            div { class: "mb-2",
                                label { class: "flex items-center gap-2 cursor-pointer select-none",
                                    input {
                                        r#type: "checkbox",
                                        class: "accent-[var(--accent)]",
                                        checked: bypass_system_audio,
                                        onchange: move |e| {
                                            let on = e.checked();
                                            let mut next = settings.read().clone();
                                            next.bypass_system_audio_processing = on;
                                            settings.set(next.clone());
                                            crate::settings::save(&next);
                                            state.write().bypass_system_audio_processing = on;
                                            v_for_bypass.send(crate::features::voice::VoiceCmd::SetBypassSystemProcessing { enabled: on });
                                        },
                                    }
                                    span { class: "text-[11px] text-[var(--text-muted)] flex-1", "Bypass system audio processing" }
                                }
                                span { class: "text-[10px] text-[var(--text-dim)] mt-0.5 block",
                                    "Skips the suppression and gain your audio driver applies before we hear anything, so only this app's processing touches your voice. Reopens the microphone."
                                }
                                if let Some(err) = bypass_error {
                                    span { class: "text-[10px] mt-0.5 block", style: "color: var(--danger);",
                                        "Couldn't bypass it: {err}. The microphone is open the usual way."
                                    }
                                }
                            }
                        }
                        div { class: "mb-2",
                            label { class: "flex items-center gap-2 cursor-pointer select-none",
                                input {
                                    r#type: "checkbox",
                                    class: "accent-[var(--accent)]",
                                    checked: auto_gain_control,
                                    onchange: move |e| {
                                        let on = e.checked();
                                        let mut next = settings.read().clone();
                                        next.auto_gain_control = on;
                                        settings.set(next.clone());
                                        crate::settings::save(&next);
                                        state.write().auto_gain_control = on;
                                        let v = v_for_agc.clone();
                                        v.send(crate::features::voice::VoiceCmd::SetAutoGainControl { enabled: on });
                                    },
                                }
                                span { class: "text-[11px] text-[var(--text-muted)]", "Automatic gain control" }
                            }
                        }
                        div { class: "mb-2",
                            label { class: "flex items-center gap-2 cursor-pointer select-none",
                                input {
                                    r#type: "checkbox",
                                    class: "accent-[var(--accent)]",
                                    checked: noise_cancellation,
                                    onchange: move |e| {
                                        let on = e.checked();
                                        let mut next = settings.read().clone();
                                        next.noise_cancellation = on;
                                        settings.set(next.clone());
                                        crate::settings::save(&next);
                                        state.write().noise_cancellation = on;
                                        v_for_denoise.send(crate::features::voice::VoiceCmd::SetNoiseCancellation { enabled: on });
                                    },
                                }
                                span { class: "text-[11px] text-[var(--text-muted)] flex-1", "Noise cancellation" }
                            }
                            span { class: "text-[10px] text-[var(--text-dim)] mt-0.5 block",
                                "Removes fans, keyboards and room noise (DeepFilterNet, ~1.5% CPU)."
                            }
                        }
                        if noise_cancellation {
                            div { class: "mb-2",
                                div { class: "flex items-center justify-between",
                                    span { class: "text-[11px] text-[var(--text-muted)]", "Suppression strength" }
                                    span { class: "text-[10px] text-[var(--text-dim)]", "{atten_lim_db} dB max" }
                                }
                                input {
                                    r#type: "range",
                                    min: "{crate::features::voice::DENOISE_ATTEN_LIM_DB_MIN}",
                                    max: "{crate::features::voice::DENOISE_ATTEN_LIM_DB_MAX}",
                                    step: "1",
                                    value: "{atten_lim_db}",
                                    class: "w-full mt-1 accent-[var(--accent)]",
                                    oninput: move |e| {
                                        let db: u32 = e.value().parse().unwrap_or(30).clamp(
                                            crate::features::voice::DENOISE_ATTEN_LIM_DB_MIN,
                                            crate::features::voice::DENOISE_ATTEN_LIM_DB_MAX,
                                        );
                                        let mut next = settings.read().clone();
                                        next.denoise_atten_lim_db = db;
                                        settings.set(next);
                                        state.write().denoise_atten_lim_db = db;
                                        v_for_atten.send(crate::features::voice::VoiceCmd::SetDenoiseAttenLim { db });
                                    },
                                    onchange: persist_settings,
                                }
                                span { class: "text-[10px] text-[var(--text-dim)] mt-0.5 block",
                                    "Lower keeps more of your voice, and more of the room with it."
                                }
                            }
                        }
                        div { class: "h-px bg-[var(--border)] my-4" }
                        p { class: "mb-3 font-mono text-[9px] uppercase tracking-[0.18em] text-[var(--text-dim)]",
                            "Transmission"
                        }
                        if reconnecting {
                            div { class: "mb-3 flex items-center gap-2 px-2.5 py-2 rounded-lg border text-[12px] text-[var(--warn)]",
                                style: "background: color-mix(in srgb, var(--warn) 8%, transparent); border-color: color-mix(in srgb, var(--warn) 35%, transparent);",
                                span { class: "dx-spinner" }
                                span { "Reconnecting audio…" }
                            }
                        }
                        div { class: "mb-2",
                            span { class: "text-[11px] text-[var(--text-muted)]", "Voice quality" }
                            select {
                                class: "w-full mt-1 bg-[var(--panel-solid)] text-[var(--text)] border border-[var(--border)] rounded px-2 py-1 text-sm",
                                value: "{voice_bitrate_kbps}",
                                onchange: move |e| {
                                    let kbps = if e.value() == "24" { 24 } else { 48 };
                                    let mut next = settings.read().clone();
                                    next.voice_bitrate_kbps = kbps;
                                    settings.set(next.clone());
                                    crate::settings::save(&next);
                                    state.write().voice_bitrate_kbps = kbps;
                                    v_for_bitrate.send(crate::features::voice::VoiceCmd::SetVoiceBitrate { kbps });
                                },
                                option { value: "24", "Standard — 24 kbit/s" }
                                option { value: "48", "High — 48 kbit/s" }
                            }
                            span { class: "text-[10px] text-[var(--text-dim)] mt-0.5 block",
                                if voice_phase == VoicePhase::Connected {
                                    "Applies the next time you join a voice channel."
                                } else {
                                    "Higher sounds better on low voices and background music, and costs more upload."
                                }
                            }
                        }
                        }
                        if settings_tab() == SettingsTab::Video {
                        h3 { class: "dxf-display text-[17px] font-bold tracking-tight text-[var(--text)]", "Video and screen" }
                        p { class: "mt-0.5 mb-4 text-[12.5px] text-[var(--text-dim)]",
                            "Your camera, and the shape of what you share."
                        }
                        div { class: "mb-2",
                            span { class: "text-[11px] text-[var(--text-muted)]", "Camera" }
                            select {
                                class: "w-full mt-1 bg-[var(--panel-solid)] text-[var(--text)] border border-[var(--border)] rounded px-2 py-1 text-sm",
                                style: "color: var(--text); background: var(--panel-solid);",
                                onchange: move |e| {
                                    let val = e.value();
                                    let mut next = settings.read().clone();
                                    next.camera_device_id = (!val.is_empty()).then(|| val.clone());
                                    next.camera_device_label = state
                                        .read()
                                        .available_cameras
                                        .iter()
                                        .find(|d| d.id == val)
                                        .map(|d| d.label.clone())
                                        .filter(|l| !l.is_empty());
                                    settings.set(next.clone());
                                    crate::settings::save(&next);
                                    if state.read().camera_on {
                                        crate::features::camera::toggle_camera(state, settings, false);
                                        crate::features::camera::toggle_camera(state, settings, true);
                                    }
                                },
                                option { value: "", "System default" }
                                for (i, cam) in available_cameras.iter().enumerate() {
                                    option {
                                        selected: selected_camera_id.as_ref().map(|id| id == &cam.id).unwrap_or(false),
                                        value: "{cam.id}",
                                        if cam.label.is_empty() { "Camera {i + 1}" } else { "{cam.label}" }
                                    }
                                }
                            }
                            if !available_cameras.is_empty() && available_cameras.iter().all(|c| c.label.is_empty()) {
                                div {
                                    class: "text-[10px] text-[var(--text-dim)] mt-1",
                                    "Turn your camera on once to see device names."
                                }
                            }
                        }
                        div { class: "mb-2",
                            span { class: "text-[11px] text-[var(--text-muted)]", "Screen share" }
                            select {
                                class: "w-full mt-1 bg-[var(--panel-solid)] text-[var(--text)] border border-[var(--border)] rounded px-2 py-1 text-sm",
                                style: "color: var(--text); background: var(--panel-solid);",
                                onchange: move |e| {
                                    let mut next = settings.read().clone();
                                    next.screenshare_quality = e.value();
                                    settings.set(next.clone());
                                    crate::settings::save(&next);
                                },
                                for (id, label, _) in crate::features::screenshare::QUALITY_PRESETS.iter() {
                                    option {
                                        selected: screenshare_quality == *id,
                                        value: "{id}",
                                        "{label}"
                                    }
                                }
                            }
                            span { class: "text-[10px] text-[var(--text-dim)] mt-0.5 block",
                                "{screenshare_hint}"
                            }
                            label { class: "flex items-center gap-2 cursor-pointer select-none mt-1.5",
                                input {
                                    r#type: "checkbox",
                                    class: "accent-[var(--accent)]",
                                    checked: screenshare_audio,
                                    onchange: move |e| {
                                        let mut next = settings.read().clone();
                                        next.screenshare_audio = e.checked();
                                        settings.set(next.clone());
                                        crate::settings::save(&next);
                                    },
                                }
                                span { class: "text-[11px] text-[var(--text-muted)] flex-1", "Share computer sound" }
                            }
                        }
                        }
                        if settings_tab() == SettingsTab::Activity {
                        h3 { class: "dxf-display text-[17px] font-bold tracking-tight text-[var(--text)]", "Level" }
                        p { class: "mt-0.5 mb-4 text-[12.5px] text-[var(--text-dim)]",
                            "Each server counts what you earned on it. Only this app can add those up across servers, so publishing the total is the only way anyone else sees one."
                        }
                        div { class: "mb-4",
                            label { class: "flex items-center gap-2 cursor-pointer select-none",
                                input {
                                    r#type: "checkbox",
                                    class: "accent-[var(--accent)]",
                                    checked: settings.read().publish_global_level,
                                    onchange: move |e| {
                                        let mut next = settings.read().clone();
                                        next.publish_global_level = e.checked();
                                        settings.set(next.clone());
                                        crate::settings::save(&next);
                                    },
                                }
                                span { class: "text-[13px] text-[var(--text)] flex-1", "Publish my overall level to Nostr" }
                            }
                            p { class: "mt-1 ml-5 text-[11px] text-[var(--text-dim)]",
                                "A number and a count of servers — never which ones. Anyone reading it sees a claim your key signed, marked as self-reported, and it gates nothing anywhere."
                            }
                        }
                        h3 { class: "dxf-display text-[17px] font-bold tracking-tight text-[var(--text)]", "Game activity" }
                        p { class: "mt-0.5 mb-4 text-[12.5px] text-[var(--text-dim)]",
                            "Shows the people you share a guild with what you are playing. Everything here is off until you turn it on, and nothing about this machine leaves it while it is off."
                        }
                        div { class: "mb-3",
                            label { class: "flex items-center gap-2 cursor-pointer select-none",
                                input {
                                    r#type: "checkbox",
                                    class: "accent-[var(--accent)]",
                                    checked: settings.read().share_activity,
                                    onchange: move |e| {
                                        let mut next = settings.read().clone();
                                        next.share_activity = e.checked();
                                        settings.set(next.clone());
                                        crate::settings::save(&next);
                                    },
                                }
                                span { class: "text-[13px] text-[var(--text)] flex-1", "Share what I am playing" }
                            }
                            p { class: "mt-1 ml-5 text-[11px] text-[var(--text-dim)]",
                                "The master switch. Turning it off clears what your guilds can see straight away."
                            }
                        }
                        if settings.read().share_activity {
                            div { class: "mb-3 pl-3 border-l border-[var(--edge)]",
                                label { class: "flex items-center gap-2 cursor-pointer select-none",
                                    input {
                                        r#type: "checkbox",
                                        class: "accent-[var(--accent)]",
                                        checked: settings.read().detect_games,
                                        onchange: move |e| {
                                            let mut next = settings.read().clone();
                                            next.detect_games = e.checked();
                                            settings.set(next.clone());
                                            crate::settings::save(&next);
                                        },
                                    }
                                    span { class: "text-[13px] text-[var(--text)] flex-1", "Detect running games" }
                                }
                                p { class: "mt-1 ml-5 mb-3 text-[11px] text-[var(--text-dim)]",
                                    "Walks the process list every 15 seconds and matches it against a short built-in list. Only a match is ever sent — never the list of what is running."
                                }
                                label { class: "flex items-center gap-2 cursor-pointer select-none",
                                    input {
                                        r#type: "checkbox",
                                        class: "accent-[var(--accent)]",
                                        checked: settings.read().discord_rpc_socket,
                                        onchange: move |e| {
                                            let mut next = settings.read().clone();
                                            next.discord_rpc_socket = e.checked();
                                            settings.set(next.clone());
                                            crate::settings::save(&next);
                                        },
                                    }
                                    span { class: "text-[13px] text-[var(--text)] flex-1", "Accept Discord Rich Presence" }
                                }
                                p { class: "mt-1 ml-5 text-[11px] text-[var(--text-dim)]",
                                    "Listens on the sockets a game already looks for, so anything shipping Rich Presence reports here with no extra work. Whichever of us starts first takes the socket, so a running Discord will stop seeing your games — or we will see none. Restart the app after changing this."
                                }
                            }
                        }
                        }
                        if settings_tab() == SettingsTab::Diagnostics {
                        h3 { class: "dxf-display text-[17px] font-bold tracking-tight text-[var(--text)]", "Diagnostics" }
                        p { class: "mt-0.5 mb-4 text-[12.5px] text-[var(--text-dim)]",
                            "The only thing here that configures nothing: it measures."
                        }
                        div { class: "mb-1",
                            label { class: "flex items-center gap-2 cursor-pointer select-none",
                                input {
                                    r#type: "checkbox",
                                    class: "accent-[var(--accent)]",
                                    checked: show_stats(),
                                    onchange: move |e| show_stats.set(e.checked()),
                                }
                                span { class: "text-[11px] text-[var(--text-muted)] flex-1", "Connection stats" }
                            }
                            if show_stats() {
                                crate::features::channels::ConnectionStats {}
                            }
                        }
                        }
                        }
                    }
            }
        }
    }
}
