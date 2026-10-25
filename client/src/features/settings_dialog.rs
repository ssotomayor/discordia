//! The app's own settings, in a dialog rather than a floating panel.
//!
//! It lives here, mounted at the workspace root, and not inside `UserPanel`
//! where it was written. A raised grid panel carries a `z-index`, that is a
//! stacking context, and a `position: fixed` child of one is trapped inside it
//! — so the panel opened *underneath* its neighbours until it was dragged,
//! which raised it. Nothing about the fix is cosmetic; see `grid-layout`'s own
//! note on the same trap in `item.rs`.

use dioxus::prelude::*;

use crate::state::use_app_state;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SettingsTab {
    General,
    Notifications,
    Audio,
    Video,
    Activity,
    Diagnostics,
    Updates,
}

/// The empty group label means "same group as the row above" — the heading is
/// drawn by the first tab that opens one.
const SETTINGS_TABS: &[(&str, SettingsTab, &str, &str)] = &[
    (
        "",
        SettingsTab::Notifications,
        "DM notifications",
        crate::features::icons::CHAT,
    ),
    (
        "",
        SettingsTab::General,
        "Language",
        crate::features::icons::PALETTE,
    ),
    (
        "Voice",
        SettingsTab::Audio,
        "Voice and audio",
        crate::features::icons::SPEAKER,
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
    (
        "",
        SettingsTab::Updates,
        "Updates",
        crate::features::icons::DOWNLOAD,
    ),
];

fn settings_tab_visible(tab: SettingsTab, android: bool, ios: bool) -> bool {
    match tab {
        SettingsTab::Notifications => android,
        SettingsTab::Video | SettingsTab::Updates => !ios,
        _ => true,
    }
}

#[cfg(test)]
mod navigation_tests {
    use super::*;

    fn menu(android: bool, ios: bool) -> Vec<SettingsTab> {
        SETTINGS_TABS
            .iter()
            .map(|(_, tab, _, _)| *tab)
            .filter(|tab| settings_tab_visible(*tab, android, ios))
            .collect()
    }

    #[test]
    fn android_can_reach_video_screen_and_update_settings() {
        let tabs = menu(true, false);
        assert!(tabs.contains(&SettingsTab::Video));
        assert!(tabs.contains(&SettingsTab::Notifications));
        assert!(tabs.contains(&SettingsTab::Updates));
    }

    #[test]
    fn desktop_and_ios_keep_their_existing_navigation() {
        let desktop = menu(false, false);
        assert!(desktop.contains(&SettingsTab::Video));
        assert!(desktop.contains(&SettingsTab::Updates));
        assert!(!desktop.contains(&SettingsTab::Notifications));
        let ios = menu(false, true);
        assert!(!ios.contains(&SettingsTab::Video));
        assert!(!ios.contains(&SettingsTab::Updates));
        assert!(!ios.contains(&SettingsTab::Notifications));
    }
}

#[component]
pub fn SettingsDialog() -> Element {
    let mut state = use_app_state();
    crate::mobile_back::use_handler(
        crate::mobile_back::SETTINGS,
        state.read().audio_settings,
        move |_| state.write().audio_settings = false,
    );
    let gw_camera = crate::state::use_gateway();
    let mut settings = use_context::<Signal<crate::settings::ClientSettings>>();

    let mut show_stats = use_signal(|| false);
    let mut settings_tab = use_signal(|| SettingsTab::Audio);
    let updates = try_consume_context::<crate::update::Updates>();
    let mut update_request = use_signal(|| {
        updates
            .map(|updates| *updates.open_request.peek())
            .unwrap_or(0)
    });
    use_effect(move || {
        if let Some(updates) = updates {
            let request = *updates.open_request.read();
            if request != *update_request.peek() {
                update_request.set(request);
                settings_tab.set(SettingsTab::Updates);
            }
        }
    });

    if !state.read().audio_settings {
        return rsx! { Fragment {} };
    }

    let selected_camera_id = settings.read().camera_device_id.clone();
    let available_cameras = state.read().available_cameras.clone();

    let screenshare_quality =
        crate::features::screenshare::quality_for_platform(&settings.read().screenshare_quality)
            .to_owned();
    let screenshare_audio = settings.read().screenshare_audio;
    let screenshare_hint = crate::features::screenshare::QUALITY_PRESETS
        .iter()
        .find(|(id, _, _)| *id == screenshare_quality)
        .map(|(_, _, hint)| *hint)
        .unwrap_or("");

    rsx! {
        style { {include_str!("../../assets/settings.css")} }
        div {
            class: "dxf-settings-backdrop dxf-backdrop-in fixed inset-0 z-[70] flex items-center justify-center bg-black/50 p-6",
            onclick: move |_| state.write().audio_settings = false,
            div {
                class: "dxf-settings dxf-modal-in max-w-full max-h-full flex flex-col bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg shadow-xl overflow-hidden",
                role: "dialog",
                aria_modal: "true",
                aria_label: crate::i18n::text("Settings", &[]),
                onclick: move |e: MouseEvent| e.stop_propagation(),

                div { class: "dxf-settings-heading h-8 px-2 flex items-center gap-2 border-b border-[var(--border)] shrink-0 select-none",
                    span { class: "text-[11px] font-medium text-[var(--text)] flex-1", {crate::i18n::text("Settings", &[])} }
                    button {
                        class: "text-[var(--text-dim)] hover:text-[var(--text)] text-base leading-none",
                        aria_label: crate::i18n::text("Close", &[]),
                        onclick: move |_| state.write().audio_settings = false,
                        "✕"
                    }
                }

                    div { class: "dxf-settings-body flex-1 flex min-h-0",
                        div { class: "dxf-settings-nav shrink-0 border-r border-[var(--border)] bg-[var(--panel2)] p-2 flex flex-col gap-0.5 overflow-y-auto",
                            for (group, t, label, icon) in SETTINGS_TABS.iter().copied().filter(|(_, tab, _, _)| {
                                settings_tab_visible(*tab, cfg!(target_os = "android"), cfg!(target_os = "ios"))
                            }) {
                                {
                                    let group = crate::i18n::label(group);
                                    let label = crate::i18n::label(label);
                                    let selected = settings_tab() == t;
                                    let cls = if selected {
                                        "relative w-full h-9 px-2.5 flex items-center gap-2 rounded-lg text-left text-[13px] font-medium bg-[var(--panel-solid)] text-[var(--text)]"
                                    } else {
                                        "relative w-full h-9 px-2.5 flex items-center gap-2 rounded-lg text-left text-[13px] text-[var(--text-dim)] hover:text-[var(--text-muted)] hover:bg-white/[0.03] transition-colors"
                                    };
                                    rsx! {
                                        Fragment { key: "{label}",
                                            if !group.is_empty() {
                                                div { class: "dxf-settings-group px-2 pt-3 pb-1.5 font-mono text-[9px] uppercase tracking-[0.18em] text-[var(--text-dim)]",
                                                    "{group}"
                                                }
                                            }
                                            button {
                                                class: "{cls}",
                                                aria_label: "{label}",
                                                aria_current: if selected { Some("page") } else { None },
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
                        div { class: "dxf-settings-content flex-1 min-w-0 overflow-y-auto",
                        if settings_tab() == SettingsTab::General {
                            h3 { class: "dxf-display text-[17px] font-bold text-[var(--text)]", {crate::i18n::text("Language", &[])} }
                            div { class: "mt-4", crate::i18n::LanguageSelector {} }
                        }
                        if settings_tab() == SettingsTab::Notifications { { android_notification_settings() } }
                        if settings_tab() == SettingsTab::Updates { crate::update::UpdatePanel {} }
                        if settings_tab() == SettingsTab::Audio {
                            super::settings_microphone::MicrophoneSettings {}
                        }
                        if settings_tab() == SettingsTab::Video {
                        h3 { class: "dxf-display text-[17px] font-bold tracking-tight text-[var(--text)]", {crate::i18n::text("Video and screen", &[])} }
                        p { class: "mt-0.5 mb-4 text-[12.5px] text-[var(--text-dim)]",
                            {crate::i18n::text("Your camera, and the shape of what you share.", &[])}
                        }
                        div { class: "mb-2",
                            span { class: "text-[11px] text-[var(--text-muted)]", {crate::i18n::text("Camera", &[])} }
                            select {
                                class: "w-full mt-1 bg-[var(--panel-solid)] text-[var(--text)] border border-[var(--border)] rounded px-2 py-1 text-sm",
                                style: "color: var(--text); background: var(--panel-solid);",
                                aria_label: crate::i18n::text("Camera", &[]),
                                value: selected_camera_id.clone().unwrap_or_default(),
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
                                        crate::features::camera::toggle_camera(state, settings, &gw_camera, false);
                                        crate::features::camera::toggle_camera(state, settings, &gw_camera, true);
                                    }
                                },
                                option { value: "", {crate::i18n::text("System default", &[])} }
                                for (i, cam) in available_cameras.iter().enumerate() {
                                    option {
                                        selected: selected_camera_id.as_ref().map(|id| id == &cam.id).unwrap_or(false),
                                        value: "{cam.id}",
                                        if cam.label.is_empty() { {crate::i18n::text("Camera {0}", &[(i + 1).to_string()])} } else { "{cam.label}" }
                                    }
                                }
                            }
                            if !available_cameras.is_empty() && available_cameras.iter().all(|c| c.label.is_empty()) {
                                div {
                                    class: "text-[10px] text-[var(--text-dim)] mt-1",
                                    {crate::i18n::text("Turn your camera on once to see device names.", &[])}
                                }
                            }
                        }
                        div { class: "mb-2",
                            span { class: "text-[11px] text-[var(--text-muted)]", {crate::i18n::text("Screen share", &[])} }
                            select {
                                class: "w-full mt-1 bg-[var(--panel-solid)] text-[var(--text)] border border-[var(--border)] rounded px-2 py-1 text-sm",
                                style: "color: var(--text); background: var(--panel-solid);",
                                aria_label: crate::i18n::text("Screen share", &[]),
                                value: "{screenshare_quality}",
                                onchange: move |e| {
                                    let mut next = settings.read().clone();
                                    next.screenshare_quality = e.value();
                                    next.screenshare_fps = None;
                                    settings.set(next.clone());
                                    crate::settings::save(&next);
                                },
                                for (id, label, _) in crate::features::screenshare::QUALITY_PRESETS.iter().filter(|(id, _, _)| crate::features::screenshare::screen_quality_supported(id)) {
                                    option {
                                        selected: screenshare_quality == *id,
                                        value: "{id}",
                                        {crate::i18n::label(label)}
                                    }
                                }
                            }
                            span { class: "text-[10px] text-[var(--text-dim)] mt-0.5 block",
                                {crate::i18n::label(screenshare_hint)}
                            }
                            label { class: "flex items-center gap-2 cursor-pointer select-none mt-1.5",
                                input {
                                    r#type: "checkbox",
                                    class: "accent-[var(--accent)]",
                                    checked: screenshare_audio,
                                    aria_label: crate::i18n::text("Share computer sound", &[]),
                                    onchange: move |e| {
                                        let mut next = settings.read().clone();
                                        next.screenshare_audio = e.checked();
                                        settings.set(next.clone());
                                        crate::settings::save(&next);
                                    },
                                }
                                span { class: "text-[11px] text-[var(--text-muted)] flex-1", {crate::i18n::text("Share computer sound", &[])} }
                            }
                        }
                        }
                        if settings_tab() == SettingsTab::Video {
                            label { class: "flex items-center gap-2 cursor-pointer mt-2",
                                input { r#type: "checkbox", class: "accent-[var(--accent)]",
                                    checked: settings.read().screenshare_adaptive_quality,
                                    aria_label: crate::i18n::text("Adaptive quality for viewers", &[]),
                                    onchange: move |e| {
                                        let mut next = settings.read().clone();
                                        next.screenshare_adaptive_quality = e.checked();
                                        settings.set(next.clone());
                                        crate::settings::save(&next);
                                    },
                                }
                                span { class: "text-[12px] text-[var(--text)]", {crate::i18n::text("Adaptive quality for viewers", &[])} }
                            }
                            p { class: "text-[10px] text-[var(--text-dim)] mt-1",
                                {crate::i18n::text("Send an additional quality for slower connections. Uses more upload bandwidth and encoding resources. Applies to the next screen share.", &[])}
                            }
                        }
                        if settings_tab() == SettingsTab::Activity {
                        h3 { class: "dxf-display text-[17px] font-bold tracking-tight text-[var(--text)]", {crate::i18n::text("Level", &[])} }
                        p { class: "mt-0.5 mb-4 text-[12.5px] text-[var(--text-dim)]",
                            {crate::i18n::text("Each server counts what you earned on it. Only this app can add those up across servers, so publishing the total is the only way anyone else sees one.", &[])}
                        }
                        div { class: "mb-4",
                            label { class: "flex items-center gap-2 cursor-pointer select-none",
                                input {
                                    r#type: "checkbox",
                                    class: "accent-[var(--accent)]",
                                    checked: settings.read().publish_global_level,
                                    aria_label: crate::i18n::text("Publish my overall level to Nostr", &[]),
                                    onchange: move |e| {
                                        let mut next = settings.read().clone();
                                        next.publish_global_level = e.checked();
                                        settings.set(next.clone());
                                        crate::settings::save(&next);
                                    },
                                }
                                span { class: "text-[13px] text-[var(--text)] flex-1", {crate::i18n::text("Publish my overall level to Nostr", &[])} }
                            }
                            p { class: "mt-1 ml-5 text-[11px] text-[var(--text-dim)]",
                                {crate::i18n::text("A number and a count of servers — never which ones. Anyone reading it sees a claim your key signed, marked as self-reported, and it gates nothing anywhere.", &[])}
                            }
                        }
                        if !cfg!(any(target_os = "android", target_os = "ios")) {
                        h3 { class: "dxf-display text-[17px] font-bold tracking-tight text-[var(--text)]", {crate::i18n::text("Game activity", &[])} }
                        p { class: "mt-0.5 mb-4 text-[12.5px] text-[var(--text-dim)]",
                            {crate::i18n::text("Shows the people you share a guild with what you are playing. Enabled by default. Turn off sharing to stop publishing your game activity.", &[])}
                        }
                        div { class: "mb-3",
                            label { class: "flex items-center gap-2 cursor-pointer select-none",
                                input {
                                    r#type: "checkbox",
                                    class: "accent-[var(--accent)]",
                                    checked: settings.read().share_activity,
                                    aria_label: crate::i18n::text("Share what I am playing", &[]),
                                    onchange: move |e| {
                                        let mut next = settings.read().clone();
                                        next.share_activity = e.checked();
                                        settings.set(next.clone());
                                        crate::settings::save(&next);
                                    },
                                }
                                span { class: "text-[13px] text-[var(--text)] flex-1", {crate::i18n::text("Share what I am playing", &[])} }
                            }
                            p { class: "mt-1 ml-5 text-[11px] text-[var(--text-dim)]",
                                {crate::i18n::text("The master switch. Turning it off clears what your guilds can see straight away.", &[])}
                            }
                        }
                        if settings.read().share_activity {
                            div { class: "mb-3 pl-3 border-l border-[var(--edge)]",
                                label { class: "flex items-center gap-2 cursor-pointer select-none",
                                    input {
                                        r#type: "checkbox",
                                        class: "accent-[var(--accent)]",
                                        checked: settings.read().detect_games,
                                            aria_label: crate::i18n::text("Detect running games", &[]),
                                        onchange: move |e| {
                                            let mut next = settings.read().clone();
                                            next.detect_games = e.checked();
                                            settings.set(next.clone());
                                            crate::settings::save(&next);
                                        },
                                    }
                                    span { class: "text-[13px] text-[var(--text)] flex-1", {crate::i18n::text("Detect running games", &[])} }
                                }
                                p { class: "mt-1 ml-5 mb-3 text-[11px] text-[var(--text-dim)]",
                                    if cfg!(windows) {
                                        {crate::i18n::text("Detects running games every 15 seconds using local Steam, Epic and Ubisoft installations and a built-in list. Only the detected game's name is shared.", &[])}
                                    } else {
                                        {crate::i18n::text("Detects running games every 15 seconds using local Steam installations and a built-in list. Only the detected game's name is shared.", &[])}
                                    }
                                }
                                if settings.read().detect_games {
                                    p { class: "mt-1 mb-3 text-xs text-[var(--text-muted)]",
                                        {
                                            let s = state.read();
                                            s.self_user.as_ref().and_then(|user| s.activity_of(&user.pubkey))
                                                .map(crate::features::profiles::activity_line)
                                                .unwrap_or_else(|| crate::i18n::text("No game detected yet. Open a game and wait up to 15 seconds.", &[]))
                                        }
                                    }
                                }
                                label { class: "flex items-center gap-2 cursor-pointer select-none",
                                    input {
                                        r#type: "checkbox",
                                        class: "accent-[var(--accent)]",
                                        disabled: !crate::presence::ipc::discord_compatible_supported(),
                                        checked: settings.read().discord_rpc_socket && crate::presence::ipc::discord_compatible_supported(),
                                        aria_label: crate::i18n::text("Accept Discord Rich Presence", &[]),
                                        onchange: move |e| {
                                            let mut next = settings.read().clone();
                                            next.discord_rpc_socket = e.checked();
                                            settings.set(next.clone());
                                            crate::settings::save(&next);
                                        },
                                    }
                                    span { class: "text-[13px] text-[var(--text)] flex-1", {crate::i18n::text("Accept Discord Rich Presence", &[])} }
                                }
                                p { class: "mt-1 ml-5 text-[11px] text-[var(--text-dim)]",
                                    if crate::presence::ipc::discord_compatible_supported() {
                                        {crate::i18n::text("Listens on the sockets a game already looks for. Whichever app starts first takes the socket. Restart the app after changing this.", &[])}
                                    } else {
                                        {crate::i18n::text("Unavailable on Windows: this integration can crash Call of Duty's Discord SDK. Running-game detection remains available.", &[])}
                                    }
                                }
                                GameDetectionOverrides {}
                            }
                        }
                        }
                        }
                        if settings_tab() == SettingsTab::Diagnostics {
                        h3 { class: "dxf-display text-[17px] font-bold tracking-tight text-[var(--text)]", {crate::i18n::text("Diagnostics", &[])} }
                        p { class: "mt-0.5 mb-4 text-[12.5px] text-[var(--text-dim)]",
                            {crate::i18n::text("The only thing here that configures nothing: it measures.", &[])}
                        }
                        div { class: "mb-1",
                            label { class: "flex items-center gap-2 cursor-pointer select-none",
                                input {
                                    r#type: "checkbox",
                                    class: "accent-[var(--accent)]",
                                    checked: show_stats(),
                                    aria_label: crate::i18n::text("Connection stats", &[]),
                                    onchange: move |e| show_stats.set(e.checked()),
                                }
                                span { class: "text-[11px] text-[var(--text-muted)] flex-1", {crate::i18n::text("Connection stats", &[])} }
                            }
                            if show_stats() {
                                crate::features::channels::ConnectionStats {}
                            }
                        }
                        CallRecording {}
                        }
                        }
                    }
            }
        }
    }
}

fn android_notification_settings() -> Element {
    #[cfg(target_os = "android")]
    {
        rsx! { crate::dm_notifications::NotificationSettings {} }
    }
    #[cfg(not(target_os = "android"))]
    {
        rsx! {}
    }
}

/// Not saved: a recorder left on would keep recording calls after a restart.
#[component]
fn CallRecording() -> Element {
    let mut folder = use_signal(crate::voice_record::folder);
    let mut error = use_signal(|| None::<String>);
    rsx! {
        div { class: "mt-4",
            label { class: "flex items-center gap-2 cursor-pointer select-none",
                input {
                    r#type: "checkbox",
                    class: "accent-[var(--accent)]",
                    checked: folder.read().is_some(),
                    aria_label: crate::i18n::text("Record calls", &[]),
                    onchange: move |e| {
                        if e.checked() {
                            match crate::voice_record::start(None) {
                                Ok(dir) => {
                                    folder.set(Some(dir));
                                    error.set(None);
                                }
                                Err(message) => error.set(Some(message)),
                            }
                        } else {
                            crate::voice_record::stop();
                            folder.set(None);
                        }
                    },
                }
                span { class: "text-[11px] text-[var(--text-muted)] flex-1", {crate::i18n::text("Record calls", &[])} }
            }
            p { class: "mt-1 ml-5 text-[11px] text-[var(--text-dim)]",
                {crate::i18n::text("Saves your microphone before and after processing, every voice you hear, and network figures to a folder on this computer, until you turn it off or quit. Tell the people in the call.", &[])}
            }
            if let Some(dir) = folder() {
                div { class: "mt-1 ml-5 flex items-center gap-2 text-[11px]",
                    span { class: "min-w-0 flex-1 truncate text-[var(--text-muted)]", title: "{dir.display()}", "{dir.display()}" }
                    button {
                        class: "rounded border border-[var(--border)] px-2 py-0.5 hover:text-[var(--accent)]",
                        onclick: move |_| {
                            if let Err(message) = super::chat_tools::open_folder(&dir) {
                                error.set(Some(message));
                            }
                        },
                        {crate::i18n::text("Open folder", &[])}
                    }
                }
            }
            if let Some(message) = error() {
                p { class: "mt-1 ml-5 text-[11px] text-[var(--danger)]", "{message}" }
            }
        }
    }
}

#[component]
fn GameDetectionOverrides() -> Element {
    let mut settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let mut executable = use_signal(String::new);
    let mut title = use_signal(String::new);
    let mut error = use_signal(|| None::<String>);
    let games = settings.read().detect_extra.clone();
    rsx! {
        div { class: "mt-4",
            h4 { class: "text-[13px] text-[var(--text)]", {crate::i18n::text("Additional games", &[])} }
            p { class: "mt-1 mb-2 text-[11px] text-[var(--text-dim)]",
                {crate::i18n::text("For games the automatic detector misses, add the executable name shown in Task Manager → Details and the name to display.", &[])}
            }
            div { class: "flex flex-wrap gap-2",
                input {
                    class: "min-w-0 flex-1 rounded border border-[var(--border)] bg-[var(--bg)] px-2 py-1 text-xs",
                    placeholder: crate::i18n::text("Executable, e.g. game.exe", &[]), value: "{executable}", maxlength: 240,
                    aria_label: crate::i18n::text("Executable, e.g. game.exe", &[]),
                    oninput: move |e| executable.set(e.value()),
                }
                input {
                    class: "min-w-0 flex-1 rounded border border-[var(--border)] bg-[var(--bg)] px-2 py-1 text-xs",
                    placeholder: crate::i18n::text("Game name", &[]), value: "{title}", maxlength: 80,
                    aria_label: crate::i18n::text("Game name", &[]),
                    oninput: move |e| title.set(e.value()),
                }
                button {
                    class: "rounded border border-[var(--border)] px-2 py-1 text-xs hover:text-[var(--accent)] disabled:opacity-40",
                    disabled: executable.read().trim().is_empty() || title.read().trim().is_empty(),
                    aria_label: crate::i18n::text("Add game", &[]),
                    onclick: move |_| {
                        let mut next = settings.read().clone();
                        let result = crate::presence::detect::set_override(&mut next.detect_extra, &executable.read(), &title.read());
                        match result {
                            Ok(()) => {
                                crate::settings::save(&next);
                                settings.set(next);
                                executable.set(String::new());
                                title.set(String::new());
                                error.set(None);
                            }
                            Err(message) => error.set(Some(message.into())),
                        }
                    },
                    {crate::i18n::text("Add game", &[])}
                }
            }
            if let Some(message) = error() {
                p { class: "mt-1 text-xs text-[var(--danger)]", {crate::i18n::message(&(message).to_string())} }
            }
            for (exe, name) in games {
                div { key: "{exe}", class: "mt-2 flex items-center gap-2 text-xs",
                    span { class: "min-w-0 flex-1 truncate", "{name} · {exe}" }
                    button {
                        class: "text-[var(--text-muted)] hover:text-[var(--danger)]",
                        aria_label: crate::i18n::text("Remove", &[]),
                        onclick: move |_| {
                            let mut next = settings.read().clone();
                            next.detect_extra.retain(|(key, _)| key != &exe);
                            crate::settings::save(&next);
                            settings.set(next);
                        },
                        {crate::i18n::text("Remove", &[])}
                    }
                }
            }
        }
    }
}
