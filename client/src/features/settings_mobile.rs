use dioxus::prelude::*;

use super::settings_dialog::SettingsTab;

pub(super) fn group(tab: SettingsTab) -> SettingsTab {
    match tab {
        SettingsTab::General | SettingsTab::Notifications | SettingsTab::Activity => {
            SettingsTab::General
        }
        SettingsTab::Audio => SettingsTab::Audio,
        SettingsTab::Video => SettingsTab::Video,
        SettingsTab::Diagnostics | SettingsTab::Updates => SettingsTab::Updates,
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
                (SettingsTab::Updates, "App", super::icons::DOWNLOAD),
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
    Sound,
}

#[component]
pub(super) fn AudioTabs(mut selected: Signal<AudioSection>) -> Element {
    rsx! {
        nav { class: "dxf-settings-audio-tabs", aria_label: crate::i18n::text("Voice and audio", &[]),
            for (section, label) in [
                (AudioSection::Microphone, "Microphone"),
                (AudioSection::Sound, "Sound"),
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
pub(super) fn AppTabs(mut selected: Signal<SettingsTab>) -> Element {
    rsx! {
        nav { class: "dxf-settings-audio-tabs", aria_label: crate::i18n::text("App", &[]),
            for (tab, label) in [(SettingsTab::Updates, "Updates"), (SettingsTab::Diagnostics, "Diagnostics")] {
                button { key: "{label}", r#type: "button", aria_pressed: selected() == tab,
                    onclick: move |_| selected.set(tab),
                    {crate::i18n::label(label)}
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
    fn updates_are_the_default_app_page() {
        assert!(group(SettingsTab::Updates) == SettingsTab::Updates);
        assert!(group(SettingsTab::Diagnostics) == SettingsTab::Updates);
        assert!(group(SettingsTab::Audio) == SettingsTab::Audio);
        assert!(group(SettingsTab::Video) == SettingsTab::Video);
    }
}
