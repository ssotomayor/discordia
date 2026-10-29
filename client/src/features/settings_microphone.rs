use super::settings_mobile::AudioSection;
use crate::features::voice::{VoiceCmd, use_voice_tx};
use crate::mic_profile::{Config, Profile};
use crate::state::use_app_state;
use dioxus::prelude::*;

#[component]
pub(super) fn MicrophoneSettings() -> Element {
    let section = use_signal(|| AudioSection::Microphone);
    let mut state = use_app_state();
    let voice = use_voice_tx();
    let mut settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let exclusive = use_signal(|| false);
    let config = Config::from_state(&state.read());
    let apply = use_callback(move |(config, persist): (Config, bool)| {
        let mut next = settings.peek().clone();
        config.apply_settings(&mut next);
        settings.set(next.clone());
        if persist {
            crate::settings::save(&next);
        }
        config.apply_state(&mut state.write());
        voice.send(VoiceCmd::ApplyMicrophone { config });
    });
    let commit = move |config: Config| apply.call((config, true));
    let slide = move |mut config: Config| {
        config.processing.profile = Profile::Custom;
        apply.call((config, false));
    };
    let persist = move |_: FormEvent| crate::settings::save(&settings.peek());
    let manual = move |mut config: Config| {
        config.processing.profile = Profile::Custom;
        commit(config);
    };
    let v_devices = use_voice_tx();
    let v_bypass = use_voice_tx();
    let v_retry = use_voice_tx();
    let inputs = state.read().available_input_devices.clone();
    let selected_input = state
        .read()
        .selected_input_device
        .clone()
        .unwrap_or_default();
    let reconnecting = state.read().voice.phase == crate::state::VoicePhase::Connecting
        || state.read().dm_call.as_ref().is_some_and(|c| {
            matches!(
                c.phase,
                crate::features::dm_call::Phase::Connecting
                    | crate::features::dm_call::Phase::Reconnecting
            )
        });
    let disabled = exclusive() || reconnecting;
    let (active, input_used, output_used, fallback) = {
        let current = state.read();
        let active = current.mic_test_active
            || current.voice.phase == crate::state::VoicePhase::Connected
            || current
                .dm_call
                .as_ref()
                .is_some_and(|c| c.phase == crate::features::dm_call::Phase::Connected);
        let input = current.active_input_device.clone().filter(|_| active);
        let output = current.active_output_device.clone().filter(|_| active);
        let fallback = input
            .as_ref()
            .filter(|actual| {
                current
                    .selected_input_device
                    .as_ref()
                    .is_some_and(|wanted| wanted != *actual)
            })
            .cloned();
        (
            active,
            input,
            output
                .or_else(|| current.selected_output_device.clone())
                .unwrap_or_else(|| crate::i18n::text("System default", &[])),
            fallback,
        )
    };

    rsx! {
        h3 { {crate::i18n::text("Voice and audio", &[])} }
        p { class: "mt-1 mb-4 text-[13px] text-[var(--text-dim)]", {crate::i18n::text("Choose your devices, check your voice and make yourself heard.", &[])} }
        if cfg!(target_os = "android") { super::settings_mobile::AudioTabs { selected: section } }
        if !cfg!(target_os = "android") || section() == AudioSection::Microphone {
        fieldset { disabled, class: "min-w-0 mb-4",
            div { class: "dxf-mic-device-grid",
                div {
                    label { class: "dxf-device-label",
                        {crate::i18n::text("Microphone", &[])}
                        select { class: "w-full mt-1 bg-[var(--panel-solid)] text-[var(--text)] border border-[var(--border)] rounded px-2 py-1 text-sm", value: "{selected_input}", id: "mic-input", aria_label: crate::i18n::text("Microphone", &[]),
                            onchange: move |e| {
                                let value = e.value(); let input = (!value.is_empty()).then_some(value);
                                let mut next = settings.peek().clone(); next.selected_input_device = input.clone();
                                let mut mic = Config::from_state(&state.peek()); mic.processing.profile = Profile::Custom;
                                mic.apply_settings(&mut next);
                                settings.set(next.clone()); crate::settings::save(&next);
                                { let mut current = state.write(); current.selected_input_device = input.clone(); mic.apply_state(&mut current); }
                                v_devices.send(VoiceCmd::ApplyMicrophone { config: mic });
                                v_devices.send(VoiceCmd::SetDevices { input, output: None });
                            },
                            option { value: "", {crate::i18n::text("System default", &[])} }
                            if !selected_input.is_empty() && !inputs.contains(&selected_input) {
                                option { value: "{selected_input}", {crate::i18n::text("{0} (unavailable)", std::slice::from_ref(&selected_input))} }
                            }
                            for device in inputs { option { value: "{device}", "{device}" } }
                        }
                    }
                    p { class: "dxf-mic-device-note",
                        if active { {crate::i18n::text("In use: {0}", &[input_used.unwrap_or_else(|| crate::i18n::text("Unavailable", &[]))])} }
                        else { {crate::i18n::text("Start a test to see which microphone is in use.", &[])} }
                    }
                }
                if !cfg!(target_os = "android") { div {
                    super::settings_audio::OutputDevice { disabled }
                    p { class: "dxf-mic-device-note", if active { {crate::i18n::text("In use: {0}", &[output_used])} } else { {crate::i18n::text("What you hear", &[])} } }

                } }
            }
            if let Some(device) = fallback {
                p { class: "dxf-mic-device-note", style: "color: var(--accent);", {crate::i18n::text("Selected microphone unavailable. Using {0}.", &[device])} }
            }
            div { class: "mt-4 flex items-center justify-between gap-2 flex-wrap",
                span { class: "text-[13px] font-semibold text-[var(--text)]", {crate::i18n::text("Voice profile", &[])} }

            }
            div { class: "dxf-mic-profiles", role: "group", aria_label: crate::i18n::text("Voice profile", &[]),
                for profile in Profile::ALL {
                    button { class: "dxf-mic-profile", r#type: "button", aria_label: crate::i18n::label(profile.label()), "aria-pressed": config.processing.profile == profile,
                        onclick: move |_| {
                            let mut next = Config::from_state(&state.peek()).select_profile(profile, &settings.peek());
                            if profile != Profile::Custom
                                && let Ok(context) = crate::features::voice::microphone_context(&state.peek())
                                && let Some(saved) = settings.peek().mic_calibrations.iter().rev().find(|saved| saved.version == 1 && saved.context == context && saved.config.processing.profile == profile)
                            { next = saved.config; }
                            commit(next);
                        },
                        {crate::i18n::label(profile.label())}
                    }
                }
            }
            p { class: "dxf-mic-profile-description", {crate::i18n::label(config.processing.profile.description())} }
            if !config.processing.headphones && config.processing.echo { p { class: "mt-1 text-[11px] text-[var(--text-dim)]", {crate::i18n::text("Echo cancellation is enabled for speakers. Use headphones for the most natural voice.", &[])} } }
        }
        crate::features::voice::mic_test::MicrophoneTestPanel { exclusive }
        fieldset { disabled, class: "min-w-0",
            div { class: "dxf-mic-main-controls",
                div { class: "dxf-mic-volume-row",
                    div {
                        label { r#for: "mic-volume", {crate::i18n::text("Microphone Volume", &[])} }
                        p { {crate::i18n::text("Changes how loud your voice sounds to others.", &[])} }
                    }
                    output { "for": "mic-volume", "{config.volume}%" }
                    input { id: "mic-volume", aria_label: crate::i18n::text("Microphone Volume", &[]), r#type: "range", min: "0", max: "200", value: "{config.volume}", oninput: move |e| { let mut next = Config::from_state(&state.peek()); next.volume = e.value().parse().unwrap_or(config.volume); slide(next); }, onchange: persist }
                }

            VoiceQualitySettings {}
            }
            details { class: "dxf-mic-advanced",
                summary { {crate::i18n::text("Advanced voice settings", &[])} }
                label { class: "flex items-center gap-2 text-[12px] text-[var(--text-muted)]",
                    {crate::i18n::text("Listening with", &[])}
                    select { class: "bg-[var(--panel-solid)] text-[var(--text)] border border-[var(--border)] rounded px-2 py-1", aria_label: crate::i18n::text("Listening with", &[]), value: if config.processing.headphones { "headphones" } else { "speakers" },
                        onchange: move |e| { let mut next = Config::from_state(&state.peek()); next.processing.headphones = e.value() == "headphones"; next.processing.echo = !next.processing.headphones; commit(next); },
                        option { value: "headphones", {crate::i18n::text("Headphones", &[])} }
                        option { value: "speakers", {crate::i18n::text("Speakers", &[])} }
                    }
                }
                div { class: "dxf-settings-control",
                    label { r#for: "mic-sensitivity", {crate::i18n::text("Sensitivity", &[])} }
                    output { "for": "mic-sensitivity", "{crate::features::voice::peak_to_db_label(config.threshold)}" }
                    input { id: "mic-sensitivity", aria_label: crate::i18n::text("Sensitivity", &[]), r#type: "range", min: "0", max: "100", value: "{crate::features::voice::peak_to_meter_pct(config.threshold)}", disabled: !config.processing.gate,
                        oninput: move |e| { let mut next = Config::from_state(&state.peek()); next.threshold = crate::features::voice::meter_pct_to_peak(e.value().parse().unwrap_or(40)); slide(next); }, onchange: persist }
                }
                label { class: "dxf-settings-check", input { r#type: "checkbox", checked: config.processing.gate, aria_label: crate::i18n::text("Transmit only when I speak", &[]), onchange: move |e| { let mut next = Config::from_state(&state.peek()); next.processing.gate = e.checked(); manual(next); } } {crate::i18n::text("Transmit only when I speak", &[])} }
                label { class: "dxf-settings-check", input { r#type: "checkbox", checked: config.processing.gate && config.processing.voice_detection, aria_label: crate::i18n::text("Use voice detection (experimental)", &[]), onchange: move |e| { let mut next = Config::from_state(&state.peek()); next.processing.voice_detection = e.checked(); if e.checked() { next.processing.gate = true; } manual(next); } } {crate::i18n::text("Use voice detection (experimental)", &[])} }
                if config.processing.gate && config.processing.voice_detection { p { class: "text-[11px] text-[var(--text-dim)]", {crate::i18n::text("Detects speech and keeps a short lead-in to preserve word beginnings. Check the result with the microphone test.", &[])} } }
                label { class: "dxf-settings-check", input { r#type: "checkbox", checked: config.agc, aria_label: crate::i18n::text("Automatic gain control", &[]), onchange: move |e| { let mut next = Config::from_state(&state.peek()); next.agc = e.checked(); manual(next); } } {crate::i18n::text("Automatic gain control", &[])} }
                label { class: "dxf-settings-check", input { r#type: "checkbox", checked: config.denoise, aria_label: crate::i18n::text("Noise cancellation", &[]), onchange: move |e| { let mut next = Config::from_state(&state.peek()); next.denoise = e.checked(); manual(next); } } {crate::i18n::text("Noise cancellation", &[])} }
                if config.denoise {
                    p { class: "text-[12px] text-[var(--text-dim)]", {crate::i18n::text("Noise filter: RNNoise", &[])} }
                    div { class: "dxf-settings-control",
                        label { r#for: "mic-suppression", {crate::i18n::text("Suppression strength", &[])} }
                        output { "for": "mic-suppression", {crate::i18n::text("{0} dB max", &[config.suppression.to_string()])} }
                        input { id: "mic-suppression", aria_label: crate::i18n::text("Suppression strength", &[]), r#type: "range", min: "6", max: "60", value: "{config.suppression}", oninput: move |e| { let mut next = Config::from_state(&state.peek()); next.suppression = e.value().parse().unwrap_or(config.suppression); slide(next); }, onchange: persist }
                    }
                    p { class: "text-[12px] text-[var(--text-dim)]", {crate::i18n::text("Lower keeps more of your voice, and more of the room with it.", &[])} }
                    p { class: "text-[11px] text-[var(--text-dim)]", {crate::i18n::text("Strength blends filtered audio with the time-aligned original; it does not change microphone volume.", &[])} }
                    if let Some(fallback) = state.read().denoise_fallback {
                        div { class: "flex items-center justify-between gap-3 text-[12px]", role: "status",
                            span { style: "color: var(--warn);",
                                match fallback {
                                    crate::denoise::Fallback::Off => crate::i18n::text("Noise cancellation paused because no filter could run smoothly.", &[]),
                                }
                            }
                            button { class: "shrink-0 px-2 py-1 rounded border border-[var(--border)] text-[var(--text)]", r#type: "button",
                                onclick: move |_| {
                                    { let mut s = state.write(); s.denoise_fallback = None; s.denoise_retry = s.denoise_retry.wrapping_add(1); }
                                    v_retry.send(VoiceCmd::RetryNoiseCancellation);
                                },
                                {crate::i18n::text("Retry", &[])}
                            }
                        }
                    }
                }
                label { class: "dxf-settings-check", input { r#type: "checkbox", checked: config.processing.echo, aria_label: crate::i18n::text("Echo cancellation", &[]), onchange: move |e| { let mut next = Config::from_state(&state.peek()); next.processing.echo = e.checked(); manual(next); } } {crate::i18n::text("Echo cancellation", &[])} }
                label { class: "dxf-settings-check", input { r#type: "checkbox", checked: config.processing.high_pass, aria_label: crate::i18n::text("Reduce low-frequency rumble", &[]), onchange: move |e| { let mut next = Config::from_state(&state.peek()); next.processing.high_pass = e.checked(); manual(next); } } {crate::i18n::text("Reduce low-frequency rumble", &[])} }
                if crate::rawmic::supported() {
                    label { class: "dxf-settings-check", input { r#type: "checkbox", checked: state.read().bypass_system_audio_processing, aria_label: crate::i18n::text("Bypass system audio processing", &[]),
                        onchange: move |e| {
                            let enabled = e.checked(); let mut next = settings.peek().clone(); next.bypass_system_audio_processing = enabled;
                            settings.set(next.clone()); crate::settings::save(&next); state.write().bypass_system_audio_processing = enabled;
                            v_bypass.send(VoiceCmd::SetBypassSystemProcessing { enabled });
                        } } {crate::i18n::text("Bypass system audio processing", &[])} }
                    if let Some(error) = state.read().mic_bypass_error.clone() { p { class: "text-[12px]", style: "color: var(--danger);", {crate::i18n::text("Couldn't bypass it: {0}. The microphone is open the usual way.", &[error])} } }
                }
            }
        }
        }
        if !cfg!(target_os = "android") || section() == AudioSection::Sound {
            fieldset { disabled, class: "min-w-0",
                if cfg!(target_os = "android") { super::settings_audio::OutputDevice { disabled } }
                super::settings_audio::AppSounds {}
            }
        }
    }
}

#[component]
fn VoiceQualitySettings() -> Element {
    let mut state = use_app_state();
    let mut settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let voice = use_voice_tx();
    let bitrate = state.read().voice_bitrate_kbps;
    rsx! {
        div { class: "dxf-voice-quality",
            label { class: "dxf-voice-quality-row", {crate::i18n::text("Voice quality", &[])}
                select { class: "w-full mt-2 bg-[var(--panel-solid)] text-[var(--text)] border border-[var(--border)] rounded px-2 py-1 text-sm", value: "{bitrate}", aria_label: crate::i18n::text("Voice quality", &[]), title: crate::i18n::text("Higher sounds better on low voices and background music, and costs more upload.", &[]),
                    onchange: move |e| {
                        let kbps = crate::settings::normalize_voice_bitrate(e.value().parse().unwrap_or(48));
                        let mut next = settings.peek().clone(); next.voice_bitrate_kbps = kbps; settings.set(next.clone()); crate::settings::save(&next);
                        state.write().voice_bitrate_kbps = kbps; voice.send(VoiceCmd::SetVoiceBitrate { kbps });
                    },
                    option { value: "24", {crate::i18n::text("Standard — 24 kbit/s", &[])} }
                    option { value: "48", {crate::i18n::text("High — 48 kbit/s", &[])} }
                    option { value: "64", {crate::i18n::text("Maximum — 64 kbit/s", &[])} }
                }
            }

        }
    }
}
