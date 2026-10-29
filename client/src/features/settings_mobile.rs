use dioxus::prelude::*;

use super::settings_dialog::SettingsTab;

pub(super) fn group(tab: SettingsTab) -> SettingsTab {
    match tab {
        SettingsTab::General | SettingsTab::Notifications | SettingsTab::Activity => {
            SettingsTab::General
        }
        SettingsTab::Audio => SettingsTab::Audio,
        SettingsTab::Video => SettingsTab::Video,
        SettingsTab::Diagnostics | SettingsTab::Updates => SettingsTab::Diagnostics,
    }
}

#[component]
pub(super) fn Navigation(mut selected: Signal<SettingsTab>) -> Element {
    let current = group(selected());
    rsx! {
        nav { class: "dxf-mobile-settings-nav", aria_label: crate::i18n::text("Settings", &[]),
            for (tab, label, icon) in [
                (SettingsTab::General, "General", super::icons::GEAR),
                (SettingsTab::Audio, "Audio", super::icons::SPEAKER),
                (SettingsTab::Video, "Video", super::icons::CAMERA),
                (SettingsTab::Diagnostics, "Support", super::icons::ACTIVITY),
            ] {
                button { key: "{label}", r#type: "button",
                    aria_current: if current == tab { Some("page") } else { None },
                    onclick: move |_| { if current != tab { selected.set(tab); } },
                    span { class: "dxf-mobile-settings-icon", dangerous_inner_html: icon }
                    span { {crate::i18n::label(label)} }
                }
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum AudioSection {
    Microphone,
    Output,
    Sounds,
}

#[component]
pub(super) fn AudioTabs(mut selected: Signal<AudioSection>) -> Element {
    rsx! {
        nav { class: "dxf-settings-audio-tabs", aria_label: crate::i18n::text("Voice and audio", &[]),
            for (section, label) in [
                (AudioSection::Microphone, "Microphone"),
                (AudioSection::Output, "Output"),
                (AudioSection::Sounds, "App sounds"),
            ] {
                button { key: "{label}", r#type: "button", aria_pressed: selected() == section,
                    onclick: move |_| selected.set(section),
                    {crate::i18n::label(label)}
                }
            }
        }
    }
}

#[component]
pub(super) fn SupportTabs(mut selected: Signal<SettingsTab>) -> Element {
    rsx! {
        nav { class: "dxf-settings-audio-tabs", aria_label: crate::i18n::text("Support", &[]),
            for (tab, label) in [(SettingsTab::Diagnostics, "Diagnostics"), (SettingsTab::Updates, "Updates")] {
                button { key: "{label}", r#type: "button", aria_pressed: selected() == tab,
                    onclick: move |_| selected.set(tab),
                    {crate::i18n::label(label)}
                }
            }
        }
    }
}

#[component]
pub(super) fn Appearance() -> Element {
    let mut settings = use_context::<Signal<crate::settings::ClientSettings>>();
    rsx! {
        section { class: "dxf-mobile-settings-section",
            h3 { {crate::i18n::text("Appearance", &[])} }
            label { class: "dxf-mobile-settings-field",
                span { {crate::i18n::text("Theme", &[])} }
                select { value: settings.read().theme.clone(), aria_label: crate::i18n::text("Theme", &[]),
                    onchange: move |event: FormEvent| {
                        let mut next = settings.peek().clone();
                        next.theme = event.value();
                        crate::settings::save(&next);
                        settings.set(next);
                    },
                    for theme in crate::app::THEMES {
                        option { value: theme.id, {crate::i18n::label(theme.label)} }
                    }
                }
            }
            label { class: "dxf-mobile-settings-field",
                span { {crate::i18n::text("Text size", &[])} output { "{settings.read().text_size_percent}%" } }
                input { r#type: "range", min: "80", max: "140", step: "10",
                    aria_label: crate::i18n::text("Text size", &[]), value: "{settings.read().text_size_percent}",
                    oninput: move |event: FormEvent| {
                        if let Ok(value) = event.value().parse::<u16>() {
                            let mut next = settings.peek().clone();
                            next.text_size_percent = value.clamp(80, 140);
                            settings.set(next);
                        }
                    },
                    onchange: move |_| crate::settings::save(&settings.peek()),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn general_contains_language_notifications_and_activity() {
        for tab in [
            SettingsTab::General,
            SettingsTab::Notifications,
            SettingsTab::Activity,
        ] {
            assert!(group(tab) == SettingsTab::General);
        }
    }

    #[test]
    fn update_requests_still_select_support() {
        assert!(group(SettingsTab::Updates) == SettingsTab::Diagnostics);
        assert!(group(SettingsTab::Diagnostics) == SettingsTab::Diagnostics);
        assert!(group(SettingsTab::Audio) == SettingsTab::Audio);
        assert!(group(SettingsTab::Video) == SettingsTab::Video);
    }
}
