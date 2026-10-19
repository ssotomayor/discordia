use crate::features::voice::use_voice_tx;
use crate::state::use_app_state;
use dioxus::prelude::*;

#[component]
pub(super) fn OutputDevice(disabled: bool) -> Element {
    let mut state = use_app_state();
    let mut settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let v_for_output_change = use_voice_tx();
    let available_output_devices = state.read().available_output_devices.clone();
    let selected_output_device = state.read().selected_output_device.clone();
    rsx! {
                        div { class: "mb-2",
                            span { class: "dxf-device-label", {crate::i18n::text("Audio output", &[])} }
                            select {
                                class: "w-full mt-1 bg-[var(--panel-solid)] text-[var(--text)] border border-[var(--border)] rounded px-2 py-1 text-sm disabled:opacity-60",
                                style: "color: var(--text); background: var(--panel-solid);",
                                disabled,
                                id: "audio-output", aria_label: crate::i18n::text("Audio output", &[]),
                                value: selected_output_device.clone().unwrap_or_default(),
                                onchange: move |e| {
                                    let val = e.value();
                                    let val_cloned = val.clone();
                                    let mut next = settings.read().clone();
                                    if val_cloned.is_empty() { next.selected_output_device = None; } else { next.selected_output_device = Some(val_cloned.clone()); }
                                    let mic = (next.selected_output_device != settings.peek().selected_output_device)
                                        .then(|| crate::mic_profile::Config::from_state(&state.peek()).for_new_output());
                                    if let Some(config) = mic { config.apply_settings(&mut next); }
                                    settings.set(next.clone());
                                    crate::settings::save(&next);

                                    let mut s = state.write();
                                    s.selected_output_device = if val_cloned.is_empty() { None } else { Some(val_cloned.clone()) };
                                    let v = v_for_output_change.clone();
                                    if let Some(config) = mic { config.apply_state(&mut s); v.send(crate::features::voice::VoiceCmd::ApplyMicrophone { config }); }
                                    v.send(crate::features::voice::VoiceCmd::SetDevices { input: None, output: s.selected_output_device.clone() });
                                },
                                option { value: "", {crate::i18n::text("System default", &[])} }
                                if let Some(name) = selected_output_device.as_ref().filter(|name| !available_output_devices.contains(name)) {
                                    option { value: "{name}", {crate::i18n::text("{0} (unavailable)", std::slice::from_ref(name))} }
                                }
                                for dev in available_output_devices.iter() {
                                    option { selected: selected_output_device.as_ref().map(|n| n == dev).unwrap_or(false), value: "{dev}", "{dev}" }
                                }
                            }
                        }

    }
}

#[component]
pub(super) fn AppSounds() -> Element {
    let mut settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let persist_settings = move |_: FormEvent| crate::settings::save(&settings.read());
    rsx! {
        details { class: "dxf-audio-sounds", open: true,
            summary { {crate::i18n::text("App sounds and soundboard", &[])} }
            p { class: "dxf-audio-sounds-note", {crate::i18n::text("Adjust the volume of what you hear in the app.", &[])} }
            div { class: "dxf-audio-sound-row",
                label { r#for: "app-sounds-volume", {crate::i18n::text("App sounds", &[])} }
                div { class: "flex items-center gap-2",
                    input { id: "app-sounds-volume", r#type: "range", min: "0", max: "100", value: "{settings.read().sfx_volume}", class: "flex-1",
                        aria_label: crate::i18n::text("App sounds volume", &[]),
                        title: crate::i18n::text("Messages, incoming and outgoing calls, and interface notifications.", &[]),
                        oninput: move |e| {
                            let val: u8 = e.value().parse().unwrap_or(70).min(100);
                            let mut next = settings.read().clone(); next.sfx_volume = val; settings.set(next);
                        }, onchange: persist_settings,
                    }
                    output { "for": "app-sounds-volume", "{settings.read().sfx_volume}%" }
                }
            }
            div { class: "dxf-audio-sound-row",
                span { {crate::i18n::text("Soundboard volume", &[])} }
                crate::features::soundboard::SoundboardVolume {}
            }
            button { class: "dxf-audio-preview-sound", onclick: move |_| {
                let saved = settings.read();
                crate::native_sounds::configure(saved.sfx_volume, saved.selected_output_device.clone());
                crate::native_sounds::play("call-connected");
            }, {crate::i18n::text("Test app sound", &[])} }
        }
    }
}
