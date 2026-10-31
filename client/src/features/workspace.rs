use dioxus::prelude::*;
use dioxus_grid_layout::{
    FloatRect, GridItem, GridLayout, GridPosition, LayoutMode, use_layout_store,
};

use crate::features::sounds::sfx;
use crate::features::{
    channels::ChannelsColumn, chat::ChatView, guilds::GuildsSidebar, members::MembersPanel,
    voice::spawn_voice_service,
};
use crate::net::spawn_gateway;
use crate::protocol::{ClientMessage, Id};
use crate::state::{ConnectionStatus, SessionParams, VoicePhase, use_app_state, use_gateway};

const GRID_ROWS: u32 = 30;
const GRID_GAP: f64 = 8.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MobileTab {
    Channels,
    Chat,
    Members,
}

fn persist_layout(
    mut settings: Signal<crate::settings::ClientSettings>,
    layout: dioxus_grid_layout::LayoutStore,
) {
    let mut next = settings.read().clone();
    let streams = layout.get("streams").is_some();
    let cells = layout
        .snapshot()
        .into_iter()
        .map(|(id, p)| (id, [p.x, p.y, p.w, p.h]))
        .collect();
    let free = layout
        .free_snapshot()
        .into_iter()
        .map(|(id, r)| (id, [r.x, r.y, r.w, r.h]))
        .collect();
    if streams {
        next.stream_layout_cells = cells;
        next.stream_layout_free = free;
    } else {
        next.layout_cells = cells;
        next.layout_free = free;
    }
    settings.set(next.clone());
    crate::settings::save(&next);
}

fn default_layout() -> Vec<(String, GridPosition)> {
    LAYOUT_TEMPLATES[0].1()
}

fn default_free_layout() -> Vec<(String, FloatRect)> {
    let transferred_width = 7.0 / 12.0 * 0.10;
    vec![
        (
            "channels".into(),
            FloatRect::new(1.0 / 12.0, 0.0, 2.0 / 12.0 + transferred_width, 1.0),
        ),
        (
            "chat".into(),
            FloatRect::new(3.0 / 12.0 + transferred_width, 0.0, 7.0 / 12.0 * 0.90, 1.0),
        ),
    ]
}

/// Whole-window arrangements, each covering all four panels and all twelve
/// columns. A template that left a gap would read as a bug in the drag handles.
#[allow(clippy::type_complexity)]
const LAYOUT_TEMPLATES: &[(&str, fn() -> Vec<(String, GridPosition)>, &str)] = &[
    ("Default", tpl_default, "Rail, channels, chat, members."),
    (
        "Wide chat",
        tpl_wide_chat,
        "Members under the channel list; the log gets the room.",
    ),
    (
        "Stacked",
        tpl_stacked,
        "Members along the bottom, under the log.",
    ),
    (
        "Even",
        tpl_even,
        "Channels and members matched, chat in the middle.",
    ),
];

pub(super) fn tpl_default() -> Vec<(String, GridPosition)> {
    vec![
        ("guilds".into(), GridPosition::new(0, 0, 1, GRID_ROWS)),
        ("channels".into(), GridPosition::new(1, 0, 2, GRID_ROWS)),
        ("chat".into(), GridPosition::new(3, 0, 7, GRID_ROWS)),
        ("members".into(), GridPosition::new(10, 0, 2, GRID_ROWS)),
    ]
}

/// No panel is ever dropped, only moved: every item has a `min_h` of 10 rows,
/// so a template that split 30 rows more finely than 20/10 would be silently
/// clamped and land somewhere nobody asked for.
fn tpl_wide_chat() -> Vec<(String, GridPosition)> {
    let half = GRID_ROWS / 2;
    vec![
        ("guilds".into(), GridPosition::new(0, 0, 1, GRID_ROWS)),
        ("channels".into(), GridPosition::new(1, 0, 2, half)),
        (
            "members".into(),
            GridPosition::new(1, half, 2, GRID_ROWS - half),
        ),
        ("chat".into(), GridPosition::new(3, 0, 9, GRID_ROWS)),
    ]
}

fn tpl_stacked() -> Vec<(String, GridPosition)> {
    let top = 20;
    vec![
        ("guilds".into(), GridPosition::new(0, 0, 1, GRID_ROWS)),
        ("channels".into(), GridPosition::new(1, 0, 2, GRID_ROWS)),
        ("chat".into(), GridPosition::new(3, 0, 9, top)),
        (
            "members".into(),
            GridPosition::new(3, top, 9, GRID_ROWS - top),
        ),
    ]
}

fn tpl_even() -> Vec<(String, GridPosition)> {
    vec![
        ("guilds".into(), GridPosition::new(0, 0, 1, GRID_ROWS)),
        ("channels".into(), GridPosition::new(1, 0, 3, GRID_ROWS)),
        ("chat".into(), GridPosition::new(4, 0, 5, GRID_ROWS)),
        ("members".into(), GridPosition::new(9, 0, 3, GRID_ROWS)),
    ]
}

const UNPLUG_ICON_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M18.36 6.64a9 9 0 1 1-12.73 0"/><line x1="12" y1="2" x2="12" y2="12"/></svg>"##;

/// Leaving a session in two beats: ask first when this machine is the host,
/// then tear the media down before the views that own it are unmounted.
#[derive(Clone, PartialEq)]
enum Leaving {
    /// Waiting on the host's answer. Carries the reason it would leave with.
    Confirm(String),
    /// Shutting the media down; `on_disconnect` fires when it is finished.
    Running(String),
}

/// A cap on the teardown, so a wedged webview or SFU cannot trap the session.
const LEAVE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Closes the webview's LiveKit room — camera, screen share and the E2EE
/// worker with it — and answers when it is really gone rather than when the
/// call was made.
pub(crate) const STOP_WEBVIEW_MEDIA_JS: &str = r#"
(async () => {
  try { await window.dxScreen.disconnect(); } catch (e) {}
  dioxus.send(true);
})();
"#;

// The nearer workspace override must yield to the app's personal theme outside guilds.
fn guild_accent_to_apply(
    dm_mode: bool,
    keep_my_accent: bool,
    guild_accent: Option<String>,
) -> Option<String> {
    if dm_mode || keep_my_accent {
        return None;
    }
    guild_accent
}

fn workspace_accent_style(accent: Option<String>) -> String {
    accent
        .map(|a| crate::app::accent_vars(&a))
        .unwrap_or_else(|| {
            // Dioxus restores omitted style properties, so explicitly reset every guild override.
            "--accent: inherit; --accent-strong: inherit; --accent-soft: inherit;".into()
        })
}

/// The layout control: templates, hand-rearranging, and the way back. Its own
/// popover because the button that used to do this floated over the bottom
/// right corner of every screen, which is where a chat window's newest message
/// is.
#[component]
fn LayoutButton(
    layout: dioxus_grid_layout::LayoutStore,
    settings: Signal<crate::settings::ClientSettings>,
    edit_mode: Signal<bool>,
) -> Element {
    let mut open = use_signal(|| false);
    let mut edit_mode = edit_mode;

    let apply = move |make: fn() -> Vec<(String, GridPosition)>, is_default: bool| {
        let mut layout = layout;
        let free = if is_default {
            default_free_layout()
        } else {
            Vec::new()
        };
        let mut arrangement = super::stream_layout::Arrangement {
            cells: make(),
            free,
        };
        if layout.get("streams").is_some() {
            arrangement = super::stream_layout::above_chat(arrangement, 12, GRID_ROWS);
        }
        layout.restore(arrangement.cells, arrangement.free);
        persist_layout(settings, layout);
    };

    rsx! {
        div { class: "relative",
            button {
                class: if edit_mode() {
                    "w-8 h-8 flex items-center justify-center rounded-lg border border-[var(--accent)] bg-[var(--panel2)] text-[var(--accent)] transition-colors"
                } else {
                    "w-8 h-8 flex items-center justify-center rounded-lg border border-[var(--border)] bg-[var(--panel2)] text-[var(--text-muted)] hover:text-[var(--accent)] hover:border-[var(--accent)] transition-colors"
                },
                title: crate::i18n::text("Panel layout", &[]),
                onclick: move |_| open.toggle(),
                span { class: "block w-4 h-4", dangerous_inner_html: crate::features::icons::LAYOUT }
            }
            if open() {
                div {
                    class: "fixed inset-0 z-40",
                    onclick: move |_| open.set(false),
                }
                div {
                    class: "dxf-pop-in absolute right-0 top-10 z-50 w-60 rounded-lg border border-[var(--border)] bg-[var(--panel-solid)] shadow-xl p-2",
                    onclick: move |e: MouseEvent| e.stop_propagation(),
                    div { class: "text-[10px] uppercase tracking-wider text-[var(--text-muted)] px-1 mb-1.5",
                        {crate::i18n::text("Arrangements", &[])}
                    }
                    for (name, make, hint) in LAYOUT_TEMPLATES.iter().copied() {
                        button {
                            key: "{name}",
                            class: "w-full text-left px-2 py-1.5 rounded-md hover:bg-white/[0.04] transition-colors",
                            onclick: move |_| {
                                apply(make, name == "Default");
                                open.set(false);
                            },
                            div { class: "text-xs text-[var(--text)]", "{name}" }
                            div { class: "text-[10px] text-[var(--text-dim)] leading-snug", {crate::i18n::label(hint)} }
                        }
                    }
                    div { class: "border-t border-[var(--border)] mt-2 pt-2",
                        button {
                            class: "w-full text-left px-2 py-1.5 rounded-md hover:bg-white/[0.04] transition-colors text-xs text-[var(--text)]",
                            onclick: move |_| {
                                let now = !edit_mode();
                                edit_mode.set(now);
                                open.set(false);
                            },
                            if edit_mode() { {crate::i18n::text("Stop rearranging", &[])} } else { {crate::i18n::text("Rearrange by hand…", &[])} }
                        }
                        div { class: "text-[10px] text-[var(--text-dim)] px-2 pb-1 leading-snug",
                            {crate::i18n::text("Drag a panel by its top edge, resize from the corner.", &[])}
                        }
                    }
                }
            }
        }
    }
}

/// What to call the place we are dialling, from what the session asked for.
/// The gateway has not answered yet, so its own name is not available — this is
/// the only description that exists before `Ready`.
fn connecting_target(mode: &crate::state::SessionMode) -> String {
    use crate::state::SessionMode;
    match mode {
        SessionMode::Remote { server_url } => crate::protocol::dial_origin(server_url)
            .unwrap_or_else(|| server_url.trim().to_string()),
        SessionMode::SelfHost { .. } => "this machine".to_string(),
        SessionMode::ByCode { code, .. } => code.trim().to_uppercase(),
    }
}

/// Covers the whole app, not the chat pane: until `Ready` lands there is no
/// guild, no channel and no member list, so the panels behind this are empty
/// furniture and framing them as a loaded app was the lie.
#[component]
fn ConnectingOverlay(target: String) -> Element {
    rsx! {
        div {
            class: "dxf-backdrop-in fixed inset-0 z-[80] flex flex-col items-center justify-center gap-6 overflow-hidden",
            style: "background: color-mix(in srgb, var(--bg) 92%, black);",
            div { class: "dxf-crt absolute inset-0 pointer-events-none" }
            div { class: "dxf-crt-roll absolute inset-x-0 top-0 pointer-events-none" }

            div { class: "dxf-connect-bob",
                crate::app::DiscordiaLogo { class: "w-20 h-20" }
            }
            div { class: "flex flex-col items-center gap-1.5 z-10",
                div { class: "dxf-display text-lg font-bold tracking-tight text-[var(--text)]",
                    {crate::i18n::text("Connecting to {0}", &[(target).to_string()])}
                }
                div { class: "dxf-ellipsis flex items-center gap-1 text-[var(--accent)] text-xl leading-none",
                    span { "\u{00b7}" }
                    span { "\u{00b7}" }
                    span { "\u{00b7}" }
                }
            }
        }
    }
}

#[component]
pub fn WorkspaceView(
    params: SessionParams,
    display_name: String,
    on_disconnect: EventHandler<String>,
    on_switch: EventHandler<super::server_rail::Switch>,
) -> Element {
    super::bookmarks::use_panel();
    let mut state = use_app_state();
    // A selected DM here is in the chat panel, always on screen; left false by
    // the last session, every message read here counted as unread.
    use_effect(move || state.write().dm_pane_open = true);
    let settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let leaving = use_signal(|| None::<Leaving>);
    // Set only once the switch is confirmed; the teardown below then ends in
    // `on_switch` instead of `on_disconnect`.
    let mut switch_to = use_signal(|| None::<super::server_rail::Switch>);
    let quitting = use_context::<crate::app::QuitRequest>().0;
    use_effect(move || {
        if quitting() && !matches!(*leaving.peek(), Some(Leaving::Running(_))) {
            let mut leaving = leaving;
            leaving.set(Some(Leaving::Running(String::new())));
        }
    });

    // Media outlives this component. A lost connection bypasses the host's
    // confirmation because the session has already ended.
    let leave = move |reason: String| {
        let mut leaving = leaving;
        let asked = matches!(*leaving.peek(), Some(Leaving::Confirm(_)));
        let idle = leaving.peek().is_none();
        if !(idle || (asked && !reason.is_empty())) {
            return;
        }
        let hosting = state.peek().host_info.is_some();
        leaving.set(Some(if hosting && reason.is_empty() {
            Leaving::Confirm(reason)
        } else {
            Leaving::Running(reason)
        }));
    };

    crate::mobile_back::use_handler(crate::mobile_back::SCREEN, true, move |_| {
        leave(String::new())
    });

    let (gateway_tx, gateway_shutdown, voice_tx) = use_hook(|| {
        state.write().status = ConnectionStatus::Connecting;
        let voice_tx = spawn_voice_service(state);
        let (gateway_tx, gateway_shutdown) =
            spawn_gateway(params.clone(), state, voice_tx.clone(), move |reason| {
                leave(reason);
            });
        (gateway_tx, gateway_shutdown, voice_tx)
    });
    provide_context(gateway_tx.clone());
    let name_gateway = gateway_tx.clone();
    let mut sent_name = use_signal(|| None::<String>);
    use_effect(use_reactive((&display_name,), move |(name,)| {
        if state.read().status != ConnectionStatus::Ready {
            sent_name.set(None);
        } else if sent_name.peek().as_ref() != Some(&name) {
            name_gateway.send(ClientMessage::UpdateUsername {
                username: name.clone(),
            });
            sent_name.set(Some(name));
        }
    }));
    crate::route_reporting::use_route_reports(state, gateway_tx.clone());
    provide_context(crate::features::voice::VoiceTx(voice_tx.clone()));
    crate::voice_handoff::use_route(
        gateway_tx.clone(),
        crate::features::voice::VoiceTx(voice_tx.clone()),
    );
    #[cfg(target_os = "android")]
    crate::call_notifications::use_guild_route(
        gateway_tx.clone(),
        crate::features::voice::VoiceTx(voice_tx.clone()),
    );
    provide_context(state);
    provide_context(params.identity.clone());
    provide_context(params.mode.clone());
    let rail = super::server_rail::use_server_rail(&params, state);
    provide_context(rail);
    let popouts = super::stream_viewer::use_popouts(state);
    provide_context(popouts);
    let profile_owner = use_signal(|| params.identity.pubkey.clone());
    let profile_synced_at = use_memo(move || state.read().profile_synced_at);
    let mut profile_sent_at = use_signal(|| *profile_synced_at.peek());
    let profile_gateway = gateway_tx.clone();
    use_effect(move || {
        let at = profile_synced_at();
        if at == *profile_sent_at.peek() {
            return;
        }
        profile_sent_at.set(at);
        if let Some(local) = crate::profile::load(&profile_owner.peek()) {
            let snapshot = state.peek();
            profile_gateway
                .send(local.set_profile_message(snapshot.profile_of(&profile_owner.peek())));
        }
    });
    let streams_visible = use_memo(move || {
        let s = state.read();
        let detached = popouts.detached.read();
        s.screen_viewing.iter().any(|pk| !detached.contains(pk))
    });

    let layout = use_layout_store(|| {
        let saved = settings.read();
        if saved.layout_cells.is_empty() {
            return default_layout();
        }
        saved
            .layout_cells
            .iter()
            .map(|(id, [x, y, w, h])| (id.clone(), GridPosition::new(*x, *y, *w, *h)))
            .collect()
    });
    use_hook(|| {
        let saved = settings.read();
        if saved.layout_free.is_empty() {
            if saved.layout_cells.is_empty() {
                let mut store = layout;
                for (id, rect) in default_free_layout() {
                    store.set_free(id, rect);
                }
            }
            return;
        }
        let mut store = layout;
        for (id, [x, y, w, h]) in &saved.layout_free {
            store.set_free(id.clone(), FloatRect::new(*x, *y, *w, *h));
        }
    });
    let mut edit_mode = use_signal(|| false);
    let mut workspace_size = use_signal(|| None::<super::workspace_layout::Size>);
    super::workspace_layout::use_arrangement(
        layout,
        settings,
        streams_visible,
        workspace_size,
        edit_mode,
        GRID_GAP,
    );

    // Media rooms survive an unmount; the gateway must finish its close
    // handshake before quitting can destroy the transport.
    let leave_voice = voice_tx.clone();
    let leave_gateway = gateway_tx.clone();
    use_effect(move || {
        let Some(Leaving::Running(reason)) = leaving() else {
            return;
        };
        let voice_tx = leave_voice.clone();
        let gateway_shutdown = gateway_shutdown.clone();
        leave_gateway.send(ClientMessage::LeaveVoice);
        spawn(async move {
            let stop_webview = document::eval(STOP_WEBVIEW_MEDIA_JS);
            let (done_tx, done_rx) = tokio::sync::oneshot::channel();
            let _ = voice_tx.send(crate::features::voice::VoiceCmd::Disconnect {
                done: Some(done_tx),
            });
            let teardown = async move {
                let mut stop_webview = stop_webview;
                let _ = stop_webview.recv::<bool>().await;
                let _ = done_rx.await;
            };
            if tokio::time::timeout(LEAVE_TIMEOUT, teardown).await.is_err() {
                eprintln!("[discordia] media teardown timed out; leaving anyway");
            }
            if tokio::time::timeout(LEAVE_TIMEOUT, gateway_shutdown.close())
                .await
                .is_err()
            {
                tracing::warn!("gateway teardown timed out; leaving anyway");
            }
            match switch_to.peek().clone() {
                Some(target) => on_switch.call(target),
                None => on_disconnect.call(reason),
            }
        });
    });

    let status = state.read().status;
    let activities_open = use_signal(|| false);
    let mut is_mobile = use_signal(|| cfg!(any(target_os = "android", target_os = "ios")));

    use_effect(move || {
        let mut eval = document::eval(
            r#"
            const mql = window.matchMedia('(max-width: 768px)');
            dioxus.send(mql.matches);
            mql.addEventListener('change', (e) => dioxus.send(e.matches));
        "#,
        );
        spawn(async move {
            while let Ok(matches) = eval.recv::<bool>().await {
                is_mobile.set(cfg!(any(target_os = "android", target_os = "ios")) || matches);
            }
        });
    });

    let guild_accent_style = {
        let keep_mine = use_context::<Signal<crate::settings::ClientSettings>>()
            .read()
            .keep_my_accent;
        let s = state.read();
        let guild_accent = s.selected_guild.and_then(|gid| {
            s.guilds
                .iter()
                .find(|g| g.id == gid)
                .and_then(|g| g.accent.clone())
        });
        workspace_accent_style(guild_accent_to_apply(s.dm_mode, keep_mine, guild_accent))
    };

    use_future(move || async move {
        let mut state = state;
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
            let now = std::time::Instant::now();
            let stale = {
                let s = state.read();
                s.typing.values().any(|set| {
                    set.values()
                        .any(|(_, t)| now.duration_since(*t).as_secs() >= 5)
                })
            };
            if stale {
                let mut s = state.write();
                for set in s.typing.values_mut() {
                    set.retain(|_, (_, t)| now.duration_since(*t).as_secs() < 5);
                }
                s.typing.retain(|_, set| !set.is_empty());
            }
        }
    });

    let mac_top_pad = if cfg!(target_os = "macos") {
        "pt-5"
    } else {
        ""
    };
    let mac_titlebar_clear = if cfg!(target_os = "macos") {
        "pt-2 pl-20"
    } else {
        ""
    };
    let mobile_class = if is_mobile() {
        "dxf-workspace-mobile"
    } else {
        ""
    };

    rsx! {
        div { class: "h-full w-full min-w-0 flex flex-col bg-[var(--bg)] p-2 gap-2 {mac_top_pad} {mobile_class}",
            style: "{guild_accent_style}; padding-top: max(var(--safe-top), 0.5rem); padding-left: max(var(--safe-left), 0.5rem); padding-right: max(var(--safe-right), 0.5rem);",
            onpointerdown: move |_| {
                if state.peek().soundboard_open { state.write().soundboard_open = false; }
            },
            onkeydown: move |e| {
                if e.key() == Key::Escape {
                    if state.peek().soundboard_open {
                        state.write().soundboard_open = false;
                    }
                    if state.peek().voice_menu.is_some() {
                        state.write().voice_menu = None;
                    }
                }
            },
            VoiceSounds {}
            VoiceSpeakingBridge {}
            ErrorToast {}
            if status == ConnectionStatus::Reconnecting {
                if let Some(report) = state.read().gateway_failure.clone() {
                    crate::features::connect::ConnectionErrorPanel { message: report }
                }
            }
            match leaving() {
                Some(Leaving::Confirm(_)) => rsx! { StopHostingDialog { leaving } },
                Some(Leaving::Running(_)) => rsx! {
                    div {
                        class: "dxf-backdrop-in fixed inset-0 z-50 flex items-center justify-center bg-black/50",
                        div { class: "text-sm text-[var(--text-muted)]",
                            match switch_to() {
                                Some(target) => rsx! { {crate::i18n::text("Switching to {0}…", &[(target . server . label ()).to_string()])} },
                                None => rsx! { {crate::i18n::text("Disconnecting…", &[])} },
                            }
                        }
                    }
                },
                None => rsx! {
                    // The host's own confirmation is the rail's dialog, not StopHostingDialog.
                    super::server_rail::SwitchHost {
                        on_go: move |target| {
                            let mut leaving = leaving;
                            if leaving.peek().is_none() {
                                switch_to.set(Some(target));
                                leaving.set(Some(Leaving::Running(String::new())));
                            }
                        },
                    }
                    super::server_rail::AddServerDialog {}
                },
            }
            crate::features::activities::ActivityHost { picker: activities_open, mobile: is_mobile() }
            crate::presence::PresenceService {}
            crate::features::leveling::XpLedgerService {}
            crate::features::screenshare::ScreenShareBridge {}
            crate::features::screenshare::ScreenSourcePicker {}
            crate::features::camera::CameraBridge {}
            crate::features::camera::CameraSourcePicker {}
            crate::mediakey::MediaKeyBridge {}
            if !cfg!(target_os = "android") {
                crate::features::screenshare::ScreenSelfPreview {}
                crate::features::camera::CameraSelfPreview {}
                crate::features::camera::CameraGridWindow {}
            }
            crate::features::profiles::ProfileCard {}
            crate::features::bot_commands::CommandDialogHost {}
            crate::features::chat::ImageViewer {}
            super::bookmarks::BookmarksDialog {}
            if status == ConnectionStatus::Connecting {
                ConnectingOverlay { target: connecting_target(&params.mode) }
            }
            if state.read().topology_open {
                crate::features::topology::TopologyDialog {
                    on_close: move |_| state.clone().write().topology_open = false,
                }
            }
            crate::features::settings_dialog::SettingsDialog {}
            crate::features::soundboard::SoundboardPanel {}
            crate::features::voice_menu::VoiceMenuHost {}
            crate::features::channel_access::ChannelAccessHost {}
            GuildDialogHost {}
            crate::features::guilds::RulesPromptDialog {}

            div { class: "dxf-workspace-header dxf-drag-region flex items-center gap-2 {mac_titlebar_clear}",
                onmousedown: move |_| crate::app::start_window_drag(),
                if !cfg!(target_os = "windows") {
                    div { class: "dxf-workspace-brand shrink-0 flex items-center gap-2 px-1",
                        crate::app::DiscordiaLogo { class: "dxf-workspace-logo w-6 h-6", compact: cfg!(target_os = "android") }
                        span { class: "dxf-display dxf-wordmark text-lg font-bold tracking-tight", {crate::i18n::text("Discordia", &[])} }
                    }
                }
                HostBanner {}
                // One chip: what the connection is, and the button that ends it.
                // Named, so it is never read as the voice hang-up downstairs.
                div {
                    class: "dxf-connection-controls dxf-no-drag shrink-0 flex items-center rounded-md border border-[var(--border)] bg-[var(--panel2)] overflow-hidden",
                    onmousedown: move |e| e.stop_propagation(),
                    TransportBadge {}
                    VoiceRouteBadge {}
                    if status == ConnectionStatus::Reconnecting {
                        span { class: "px-2 text-[10px] text-[var(--warn)]", {crate::i18n::text("Reconnecting…", &[])} }
                    }
                    EncryptionBadge {}
                    if !cfg!(target_os = "android") {
                    button {
                        class: "dxf-server-disconnect h-8 px-2 flex items-center gap-1.5 border-l border-[var(--border)] text-[10px] uppercase tracking-wider text-[var(--text-muted)] hover:text-[var(--danger)] transition-colors",
                        title: crate::i18n::text("Disconnect from this server", &[]),
                        aria_label: crate::i18n::text("Disconnect from this server", &[]),
                        onclick: move |_| leave(String::new()),
                        span { class: "block w-4 h-4", dangerous_inner_html: UNPLUG_ICON_SVG }
                        span { class: "dxf-disconnect-label", {crate::i18n::text("Disconnect", &[])} }
                    }
                    }
                }
                // Settings and appearance sit in the corner of the window, not
                // beside your name: they are about the app, not about you.
                div { class: "dxf-workspace-tools dxf-no-drag ml-auto shrink-0 flex items-center gap-1.5",
                    onmousedown: move |e| e.stop_propagation(),
                    if !is_mobile() {
                        LayoutButton { layout, settings, edit_mode }
                    }
                    crate::features::appearance::AppearanceButton {}
                    button {
                        class: "w-8 h-8 flex items-center justify-center rounded-lg border border-[var(--border)] bg-[var(--panel2)] text-[var(--text-muted)] hover:text-[var(--accent)] hover:border-[var(--accent)] transition-colors",
                        title: crate::i18n::text("Settings", &[]),
                        onclick: move |_| {
                            let open = !state.read().audio_settings;
                            state.clone().write().audio_settings = open;
                        },
                        span { class: "block w-4 h-4", dangerous_inner_html: crate::features::icons::GEAR }
                    }
                    if cfg!(target_os = "android") {
                        button {
                            class: "dxf-server-disconnect flex items-center justify-center rounded-lg text-[var(--text-muted)] hover:text-[var(--danger)]",
                            title: crate::i18n::text("Disconnect from this server", &[]),
                            aria_label: crate::i18n::text("Disconnect from this server", &[]),
                            onclick: move |_| leave(String::new()),
                            span { class: "block w-4 h-4", dangerous_inner_html: UNPLUG_ICON_SVG }
                        }
                    }
                }
            }

            if is_mobile() {
                super::mobile_workspace::MobileWorkspace { activities_open }
            } else {
                div { class: "flex-1 overflow-auto min-h-0",
                    onmounted: move |event| {
                        let data = event.data();
                        spawn(async move {
                            if let Ok(rect) = data.get_client_rect().await
                                && workspace_size.peek().is_none()
                            {
                                workspace_size.set(Some((rect.size.width, rect.size.height)));
                            }
                        });
                    },
                    onresize: move |event| {
                        if let Ok(size) = event.get_content_box_size() {
                            workspace_size.set(Some((size.width, size.height)));
                        }
                    },
                        GridLayout {
                            cols: 12, rows: GRID_ROWS, gap: GRID_GAP,
                            store: layout, editable: edit_mode(),
                            mode: LayoutMode::Free,
                            on_change: move |_: Vec<(String, GridPosition)>| persist_layout(settings, layout),
                            GridItem { id: "guilds", x: 0, y: 0, w: 1, h: GRID_ROWS, min_w: 1, min_h: 10,
                                min_w_px: f64::from(super::workspace_layout::RAIL_MIN),
                                min_h_px: f64::from(super::workspace_layout::RAIL_MIN),
                                // A size container, so the rail lays itself out as a row
                                // whenever it is wider than tall.
                                div { class: "dxf-guilds-frame relative w-full h-full",
                                    GuildsSidebar {}
                                    if edit_mode() {
                                        RailDock { layout, settings, size: workspace_size }
                                    }
                                }
                            }
                            GridItem { id: "channels", x: 1, y: 0, w: 2, h: GRID_ROWS, min_w: 2, min_h: 10,
                                ChannelsColumn {}
                            }
                            crate::features::screenshare::ScreenWatchWindow {}
                            GridItem { id: "chat", x: 3, y: 0, w: 7, h: GRID_ROWS, min_w: 3, min_h: 4,
                                div { class: "panel-hover w-full h-full flex flex-col bg-[var(--bg)] border border-[var(--border)] rounded-xl overflow-hidden",
                                    ChatView {}
                                }
                            }
                            GridItem { id: "members", x: 10, y: 0, w: 2, h: GRID_ROWS, min_w: 2, min_h: 10,
                                MembersPanel {}
                            }
                        }
                    }
            }

            // While rearranging, one reminder of how to stop — the button that
            // started it is up in the corner and easy to lose track of.
            if edit_mode() {
                div { class: "fixed bottom-3 right-3 z-40",
                    button {
                        class: "dxf-cta rounded-lg px-3 py-1.5 text-[10px] uppercase tracking-wider",
                        onclick: move |_| edit_mode.set(false),
                        {crate::i18n::text("Done rearranging", &[])}
                    }
                }
            }
        }
    }
}

/// While rearranging: put the rail along an edge, the other panels keeping
/// their arrangement in what is left. Dragging it anywhere else still works.
#[component]
fn RailDock(
    layout: dioxus_grid_layout::LayoutStore,
    settings: Signal<crate::settings::ClientSettings>,
    size: Signal<Option<super::workspace_layout::Size>>,
) -> Element {
    use super::workspace_layout::{Dock, dock_rail, rail_px};
    let dock = move |side: Dock| {
        let Some(size) = size() else { return };
        let base = super::stream_layout::Arrangement {
            cells: layout.snapshot(),
            free: layout.free_snapshot(),
        };
        let thickness = rail_px(&settings.peek());
        let Some(rects) = dock_rail(&base, side, size, GRID_GAP, thickness) else {
            return;
        };
        let mut store = layout;
        for (id, rect) in rects {
            store.set_free(id, rect);
        }
        persist_layout(settings, layout);
    };
    rsx! {
        div { class: "dxf-rail-dock absolute inset-0 z-10 flex items-center justify-center",
            div { class: "dxf-rail-dock-buttons flex flex-col gap-1 p-1 rounded-lg border border-[var(--border-strong)] bg-[var(--panel-solid)] shadow-lg",
                for (side, label, icon) in [
                    (Dock::Left, "Dock to the left", DOCK_LEFT_SVG),
                    (Dock::Top, "Dock to the top", DOCK_TOP_SVG),
                    (Dock::Bottom, "Dock to the bottom", DOCK_BOTTOM_SVG),
                ] {
                    button {
                        r#type: "button",
                        class: "w-7 h-7 flex items-center justify-center rounded text-[var(--text-muted)] hover:text-[var(--accent)] hover:bg-white/[0.06] transition-colors",
                        title: crate::i18n::text(label, &[]),
                        aria_label: crate::i18n::text(label, &[]),
                        onclick: move |_| dock(side),
                        span { class: "block w-4 h-4", dangerous_inner_html: icon }
                    }
                }
            }
        }
    }
}

const DOCK_LEFT_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5"><rect x="1.5" y="2.5" width="13" height="11" rx="1.5"/><rect x="1.5" y="2.5" width="3.5" height="11" rx="1" fill="currentColor"/></svg>"##;
const DOCK_TOP_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5"><rect x="1.5" y="2.5" width="13" height="11" rx="1.5"/><rect x="1.5" y="2.5" width="13" height="3.5" rx="1" fill="currentColor"/></svg>"##;
const DOCK_BOTTOM_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5"><rect x="1.5" y="2.5" width="13" height="11" rx="1.5"/><rect x="1.5" y="10" width="13" height="3.5" rx="1" fill="currentColor"/></svg>"##;

#[component]
pub(crate) fn ErrorToast() -> Element {
    let mut state = use_app_state();
    let message = use_memo(move || state.read().error_toast.clone());

    use_effect(move || {
        let Some(current) = message() else { return };
        spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(6)).await;
            let mut s = state.write();
            if s.error_toast.as_deref() == Some(current.as_str()) {
                s.error_toast = None;
            }
        });
    });

    let Some(msg) = message() else {
        return rsx! { Fragment {} };
    };

    rsx! {
        div { class: "dxf-pop-in fixed bottom-12 left-1/2 -translate-x-1/2 z-50 max-w-md flex items-center gap-2 px-3 py-2 rounded-lg border border-[var(--danger)]/50 bg-[var(--panel-solid)] shadow-xl",
            span { class: "w-2 h-2 rounded-full shrink-0", style: "background: var(--danger);" }
            span { class: "text-xs text-[var(--text)] flex-1", {crate::i18n::message(&msg)} }
            button {
                class: "text-[var(--text-dim)] hover:text-[var(--text)] text-sm leading-none",
                onclick: move |_| state.write().error_toast = None,
                "✕"
            }
        }
    }
}

#[component]
fn VoiceSounds() -> Element {
    let state = use_app_state();
    let phase = use_memo(move || state.read().voice.phase);
    let mut last_phase = use_signal(|| VoicePhase::Idle);

    use_effect(move || {
        let now = phase();
        let prev = *last_phase.peek();
        if now == prev {
            return;
        }
        last_phase.set(now);
        match now {
            VoicePhase::Connected => sfx("connect"),
            VoicePhase::Idle | VoicePhase::Error
                if matches!(prev, VoicePhase::Connected | VoicePhase::Connecting) =>
            {
                sfx("disconnect")
            }
            _ => {}
        }
    });

    let viewing = use_memo(move || state.read().screen_viewing.clone());
    let mut last_viewing = use_signal(|| state.peek().screen_viewing.clone());
    use_effect(move || {
        let now = viewing();
        let prev = last_viewing.peek().clone();
        if now == prev {
            return;
        }
        let started = now.iter().any(|pk| !prev.contains(pk));
        let stopped = prev.iter().any(|pk| !now.contains(pk));
        last_viewing.set(now.clone());
        if started {
            sfx("watch-start");
        } else if stopped {
            sfx("watch-stop");
        }
    });

    let status = use_memo(move || state.read().status);
    let mut last_status = use_signal(|| ConnectionStatus::Connecting);
    use_effect(move || {
        let now = status();
        let prev = *last_status.peek();
        if now == prev {
            return;
        }
        last_status.set(now);
        match (prev, now) {
            (ConnectionStatus::Connecting, ConnectionStatus::Ready) => sfx("connect"),
            (_, ConnectionStatus::Disconnected) => sfx("server-disconnect"),
            _ => {}
        }
    });

    let sharing = use_memo(move || state.read().screen_sharing);
    let mut last_sharing = use_signal(|| false);
    use_effect(move || {
        let now = sharing();
        if now == *last_sharing.peek() {
            return;
        }
        last_sharing.set(now);
        if now {
            sfx("stream-start");
        } else {
            sfx("stream-stop");
        }
    });

    let voice_channel = use_memo(move || state.read().voice.channel_id);
    let voice_states = use_memo(move || state.read().voice_states.clone());
    let self_pk = use_memo(move || state.read().self_user.as_ref().map(|u| u.pubkey.clone()));
    let mut last_peers_channel = use_signal(|| None::<Id>);
    let mut last_peers = use_signal(Vec::<String>::new);
    use_effect(move || {
        let ch = voice_channel();
        let states = voice_states();
        let me = self_pk();
        let peers: Vec<String> = match (&ch, &me) {
            (Some(cid), Some(me_pk)) => states
                .iter()
                .filter(|v| v.channel_id == Some(*cid) && &v.user_pubkey != me_pk)
                .map(|v| v.user_pubkey.clone())
                .collect(),
            _ => Vec::new(),
        };
        if ch != *last_peers_channel.peek() {
            last_peers_channel.set(ch);
            last_peers.set(peers);
            return;
        }
        let prev = last_peers.peek().clone();
        let joined = peers.iter().any(|p| !prev.contains(p));
        let left = prev.iter().any(|p| !peers.contains(p));
        if joined {
            sfx("peer-join");
        }
        if left {
            sfx("peer-leave");
        }
        last_peers.set(peers);
    });

    let screen_shares = use_memo(move || state.read().screen_shares.clone());
    let mut last_shares_channel = use_signal(|| None::<Id>);
    let mut last_sharers = use_signal(Vec::<String>::new);
    use_effect(move || {
        let ch = voice_channel();
        let shares = screen_shares();
        let me = self_pk();
        let sharers: Vec<String> = match (&ch, &me) {
            (Some(cid), Some(me_pk)) => shares
                .get(cid)
                .map(|v| v.iter().filter(|p| *p != me_pk).cloned().collect())
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        if ch != *last_shares_channel.peek() {
            last_shares_channel.set(ch);
            last_sharers.set(sharers);
            return;
        }
        let prev = last_sharers.peek().clone();
        let started = sharers.iter().any(|p| !prev.contains(p));
        let stopped = prev.iter().any(|p| !sharers.contains(p));
        if started {
            sfx("peer-stream-start");
        }
        if stopped {
            sfx("peer-stream-stop");
        }
        last_sharers.set(sharers);
    });

    rsx! { Fragment {} }
}

#[component]
fn VoiceSpeakingBridge() -> Element {
    let state = use_app_state();
    let gateway = use_gateway();
    let speaking = use_memo(move || state.read().voice.speaking);
    let mut last = use_signal(|| false);

    use_effect(move || {
        let now = speaking();
        if now != *last.peek() {
            last.set(now);
            gateway.send(ClientMessage::SetSpeaking { speaking: now });
        }
    });

    rsx! { Fragment {} }
}

pub(crate) fn others_connected(s: &crate::state::AppState) -> usize {
    let me = s.self_user.as_ref().map(|u| u.pubkey.as_str());
    let mut online: Vec<&str> = s
        .members
        .iter()
        .filter(|m| m.online && Some(m.user.pubkey.as_str()) != me)
        .map(|m| m.user.pubkey.as_str())
        .collect();
    online.sort_unstable();
    online.dedup();
    online.len()
}

/// What stopping the host actually costs, said before it happens.
///
/// The count is of people the gateway still holds a connection for, which is
/// what "and everyone in them" means here — the member list also carries people
/// who are merely members and already offline.
#[component]
fn StopHostingDialog(leaving: Signal<Option<Leaving>>) -> Element {
    let state = use_app_state();
    let mut leaving = leaving;

    let (guilds, others) = {
        let s = state.read();
        (s.guilds.len(), others_connected(&s))
    };
    let guilds_label = if guilds == 1 { "guild" } else { "guilds" };
    let guilds_label = crate::i18n::label(guilds_label);
    let people = match others {
        0 => crate::i18n::text("Nobody else is connected right now.", &[]),
        1 => crate::i18n::text(
            "One other person is connected right now, and will be dropped.",
            &[],
        ),
        n => crate::i18n::text(
            "{0} other people are connected right now, and will be dropped.",
            &[n.to_string()],
        ),
    };

    let cancel = move |_| leaving.set(None);
    let confirm = move |_| leaving.set(Some(Leaving::Running(String::new())));

    rsx! {
        div {
            class: "dxf-backdrop-in fixed inset-0 z-50 flex items-center justify-center bg-black/50",
            onclick: cancel,
            div {
                class: "dxf-modal-in w-[26rem] flex flex-col bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg shadow-xl overflow-hidden",
                onclick: move |e| e.stop_propagation(),
                div { class: "px-4 py-3 border-b border-[var(--border)]",
                    h3 { class: "text-sm font-medium text-[var(--warn)]", {crate::i18n::text("This machine is the server", &[])} }
                }
                div { class: "px-4 py-3 flex flex-col gap-2 text-sm text-[var(--text-muted)]",
                    p {
                        {crate::i18n::text("Disconnecting shuts the server down. Its {0} {1}, ", &[(guilds).to_string(), (guilds_label).to_string()])}
                        {crate::i18n::text("their channels and any voice or screen share go with it, and nobody ", &[])}
                        {crate::i18n::text("can reach them until you host again from here.", &[])}
                    }
                    p { "{people}" }
                    p { class: "text-[var(--text-dim)]",
                        {crate::i18n::text("Nothing is deleted — messages and settings stay on this machine.", &[])}
                    }
                }
                div { class: "px-4 py-3 border-t border-[var(--border)] flex items-center justify-end gap-2",
                    button {
                        class: "px-3 py-1 rounded text-[11px] text-[var(--text-muted)] hover:text-[var(--text)] transition-colors",
                        onclick: cancel,
                        {crate::i18n::text("Keep hosting", &[])}
                    }
                    button {
                        class: "px-3 py-1 rounded text-[11px] uppercase tracking-wider text-[var(--danger)] border border-[var(--border)] hover:border-[var(--danger)] transition-colors",
                        onclick: confirm,
                        {crate::i18n::text("Stop the server", &[])}
                    }
                }
            }
        }
    }
}

#[component]
fn HostBanner() -> Element {
    let mut state = use_app_state();
    let snapshot = state.read();
    let Some(info) = snapshot.host_info.clone() else {
        return rsx! { Fragment {} };
    };
    drop(snapshot);

    let mut copied = use_signal(|| false);
    let mut copied_share = use_signal(|| false);
    let share = info.share.clone();
    let shortcode = info.shortcode.clone();
    let publish_error = info.publish_error.clone();
    let listed_public = info.listed_public;
    let has_shortcode = info.shortcode.is_some();
    let reachability = info.reachability.clone();
    let voice_why = info.voice_reason.clone();
    let (voice_label, voice_color, voice_title) = if info.livekit_url.is_empty() {
        (
            "voice unavailable",
            "text-[var(--warn)]",
            "No SFU: the bundled one did not start and the rendezvous offers none.".to_string(),
        )
    } else if info.voice_bundled {
        (
            "voice on this machine",
            "text-[var(--success)]",
            voice_why.clone(),
        )
    } else {
        (
            "voice via rendezvous",
            "text-[var(--success)]",
            format!(
                "{voice_why}. Calls run through the rendezvous's SFU — let the router map the \
                 voice ports or enable IPv6 to carry them yourself."
            ),
        )
    };

    rsx! {
        div { class: "dxf-no-drag shrink-0 h-8 pl-2.5 pr-1 bg-[var(--panel2)] border border-[var(--border)] rounded-lg flex items-center gap-2 text-xs",
            onmousedown: move |e| e.stop_propagation(),
            span {
                class: "w-2 h-2 rounded-full shrink-0",
                style: "background: var(--success);",
                title: crate::i18n::text("Hosting from this machine", &[]),
            }
            button {
                class: "text-[11.5px] text-[var(--text-dim)] hover:text-[var(--accent)] transition-colors",
                title: crate::i18n::text("Where your data goes", &[]),
                onclick: move |_| state.write().topology_open = true,
                {crate::i18n::text("self-hosted", &[])}
            }
            // The code is what you hand a friend, so it is the one thing here
            // sized to be read and copied rather than skimmed.
            if let Some(code) = shortcode {
                code { class: "font-mono text-[12px] text-[var(--text)] select-all", "{code}" }
                button {
                    class: "w-6 h-6 shrink-0 flex items-center justify-center rounded text-[var(--text-dim)] hover:text-[var(--accent)] transition-colors",
                    title: if copied() { crate::i18n::text("Copied", &[]) } else { crate::i18n::text("Copy the invite code", &[]) },
                    onclick: move |_| {
                        copied.set(crate::clipboard::copy_text(&code));
                    },
                    span {
                        class: "block w-3.5 h-3.5",
                        dangerous_inner_html: crate::features::icons::COPY,
                    }
                }
            }
            // The share string is long and opaque; it is copied, never read.
            if let Some(share) = share {
                button {
                    class: "pl-2 inline-flex items-center gap-1 text-[10.5px] text-[var(--text-muted)] hover:text-[var(--accent)] transition-colors border-l border-[var(--border)]",
                    title: crate::i18n::text("Copy the quic:// address friends type to reach this machine directly. It carries the key their connection is checked against, so nothing on the way can read or pose as you.", &[]),
                    onclick: move |_| {
                        copied_share.set(crate::clipboard::copy_text(&share));
                    },
                    span { class: "block w-3.5 h-3.5", dangerous_inner_html: crate::features::icons::COPY }
                    if copied_share() { {crate::i18n::text("address copied", &[])} } else { {crate::i18n::text("copy address", &[])} }
                }
            }
            if let Some(err) = publish_error {
                span { class: "text-[var(--text-dim)]", "·" }
                span { class: "text-[var(--danger)]",
                    title: crate::i18n::message(&(err).to_string()),
                    {crate::i18n::text("⚠ not published — {0}", std::slice::from_ref(&(err)))}
                }
            } else if has_shortcode && !listed_public {
                span { class: "text-[var(--text-dim)]", "·" }
                span { class: "text-[var(--text-muted)]",
                    title: crate::i18n::text("Reachable by code, but not shown in the public list. Turn on public listing when you create the server to appear there.", &[]),
                    {crate::i18n::text("unlisted", &[])}
                }
            }
            span { class: "flex-1" }
            Reachability { reachability }
            span { class: "{voice_color}", title: crate::i18n::message(&voice_title), {format!("● {}", crate::i18n::label(voice_label))} }
        }
    }
}

#[component]
fn Reachability(reachability: crate::host::Reachability) -> Element {
    use crate::host::Reachability as R;
    match reachability {
        R::Direct {
            method,
            media,
            note,
        } => {
            let title = if media {
                if method == "IPv6" {
                    "Global IPv6 addresses are available. Clients verify voice connectivity when joining; both sides need IPv6 and inbound firewall access.".to_string()
                } else {
                    format!(
                        "{method}: {}.",
                        note.unwrap_or(
                            "voice candidates are available; callers verify media connectivity"
                        )
                    )
                }
            } else {
                format!(
                    "{method} mapped the chat port, but not the voice ports: {}. Voice clients can still try local or relay routes.",
                    note.unwrap_or("the reason was not recorded")
                )
            };
            rsx! {
                span { class: "text-[var(--text-dim)]", "·" }
                span { class: "text-[var(--success)]", title: "{title}",
                    if method == "IPv6" { {crate::i18n::text("● IPv6 available", &[])} }
                    else if method == "manual forwarding" { {crate::i18n::text("● manual address (voice unverified)", &[])} }
                    else if media { {crate::i18n::text("● voice candidates available", &[])} } else { {crate::i18n::text("● chat port mapped", &[])} }
                }
            }
        }
        R::LanOnly { reason } => rsx! {
            span { class: "text-[var(--text-dim)]", "·" }
            span { class: "text-[var(--warn)]",
                title: crate::i18n::text("{0} Friends elsewhere can still join by code — the rendezvous relays them.", std::slice::from_ref(&(reason))),
                {crate::i18n::text("● this network only", &[])}
            }
        },
        R::LoopbackOnly => rsx! {
            span { class: "text-[var(--text-dim)]", "·" }
            span { class: "text-[var(--text-muted)]",
                title: crate::i18n::text("Direct connections weren't enabled, so the gateway only listens on this machine. Friends can still join by code through the rendezvous.", &[]),
                {crate::i18n::text("● local only", &[])}
            }
        },
    }
}

#[component]
fn EncryptionBadge() -> Element {
    // The encryption badge is a diagnostic; release builds stay clean.
    if !cfg!(debug_assertions) {
        return rsx! { Fragment {} };
    }
    let mut state = use_app_state();
    let snapshot = state.read();
    let in_voice = snapshot.voice.channel_id.is_some();
    let has_key = snapshot
        .voice
        .channel_id
        .is_some_and(|c| snapshot.media_keys.contains_key(&c));
    let broken = snapshot.media_undecryptable;
    drop(snapshot);

    if !in_voice || (!has_key && !broken) {
        return rsx! { Fragment {} };
    }

    let (label, color, title) = if broken {
        (
            "media undecryptable",
            "text-[var(--danger)]",
            "Encrypted media is arriving that this client cannot decrypt — usually a key that has not reached you yet. It often clears on its own within a second; if it does not, rejoin the channel.",
        )
    } else {
        (
            "media encrypted",
            "text-[var(--success)]",
            "Voice, screen share and camera are encrypted end to end. The SFU forwards frames it cannot decrypt — including one you run yourself.",
        )
    };
    rsx! {
        button {
            class: "dxf-encryption-badge shrink-0 px-2 py-1 text-[10px] uppercase tracking-wider {color} hover:underline",
            title: crate::i18n::text("{0} — click for the whole picture", &[(title).to_string()]),
            aria_label: crate::i18n::text(label, &[]),
            onclick: move |_| state.write().topology_open = true,
            span { class: "dxf-encryption-icon", dangerous_inner_html: super::icons::SHIELD }
            span { class: "dxf-encryption-label", "{label}" }
        }
    }
}

#[component]
fn TransportBadge() -> Element {
    let mut state = use_app_state();
    let snapshot = state.read();
    let transport = snapshot.transport;
    let route = snapshot.gateway_route.clone();
    let host = snapshot.host_info.clone();
    drop(snapshot);

    if let Some(info) = host {
        return rsx! { super::host_connections::HostConnections { info } };
    }

    let (label, color, title) = match transport {
        crate::state::Transport::Loopback => (
            "local",
            "text-[var(--success)]",
            "Chat and server control stay on this PC. Voice has a separate connection.",
        ),
        crate::state::Transport::Quic => (
            "direct",
            "text-[var(--success)]",
            "Connected straight to the host over encrypted QUIC, with the host authenticated by its key. Nobody in between can read this connection. The host can see your IP address.",
        ),
        crate::state::Transport::QuicRelayed => (
            "relayed",
            "text-[var(--success)]",
            "Carried by the rendezvous relay, still encrypted end to end: the relay forwards ciphertext and sees only the two keys. The host does not learn your address, the relay does.",
        ),
        crate::state::Transport::Proxied => (
            "proxied",
            "text-[var(--success)]",
            "Connected over TLS to a proxy in front of the gateway. The proxy's operator, usually the server's, can read this connection; nobody else on the path can.",
        ),
    };
    let route_label = route.unwrap_or_else(|| label.to_string());
    let route_label = crate::i18n::label(&route_label).to_owned();
    rsx! {
        button {
            class: "dxf-transport-badge shrink-0 px-2 py-1 text-[10px] uppercase tracking-wider {color} hover:underline",
            title: crate::i18n::text("{0} — click for the whole picture", &[crate::i18n::label(title).to_owned()]),
            onclick: move |_| state.write().topology_open = true,
            aria_label: crate::i18n::text("Server: {0}", std::slice::from_ref(&(route_label))),
            span { class: "dxf-route-full", {crate::i18n::text("Server: {0}", std::slice::from_ref(&(route_label)))} }
            span { class: "dxf-route-mobile",
                if cfg!(target_os = "android") {
                    span { class: "dxf-route-caption", {crate::i18n::text("Server", &[])} }
                }
                span { class: "dxf-route-value", "{route_label}" }
            }
        }
    }
}

#[component]
fn VoiceRouteBadge() -> Element {
    let state = use_app_state();
    let mut open = use_signal(|| false);
    let s = state.read();
    if s.voice.phase == crate::state::VoicePhase::Idle {
        return rsx! {};
    }
    let location = match s.voice_location.as_deref() {
        Some("host") => "host PC",
        Some("rendezvous") => "rendezvous",
        Some("configured") => "configured server",
        _ => "server unknown",
    };
    let location = crate::i18n::label(location);
    let phase = match s.voice.phase {
        crate::state::VoicePhase::Connecting => crate::i18n::text("Connecting…", &[]),
        crate::state::VoicePhase::Error => crate::i18n::text("Error", &[]),
        _ => {
            let send = s
                .voice_send_route
                .as_ref()
                .map(|r| crate::i18n::label(&r.label()).to_owned());
            let receive = s
                .voice_receive_route
                .as_ref()
                .map(|r| crate::i18n::label(&r.label()).to_owned());
            match (send, receive) {
                (Some(a), Some(b)) if a != b => "different send/receive routes".into(),
                (Some(a), Some(_)) => a,
                (Some(a), None) => crate::i18n::text("send {0}", &[a]),
                (None, Some(a)) => crate::i18n::text("receive {0}", &[a]),
                _ => "media route unknown".into(),
            }
        }
    };
    let phase = crate::i18n::label(&phase).to_owned();
    let mobile_phase = crate::i18n::text(
        match s.voice.phase {
            crate::state::VoicePhase::Connecting => "Connecting…",
            crate::state::VoicePhase::Connected => "Connected",
            _ => "Error",
        },
        &[],
    );
    let signaling = s
        .voice_endpoint
        .as_deref()
        .unwrap_or(crate::i18n::label("not connected"));
    let send = s
        .voice_send_route
        .as_ref()
        .map(|r| format!("{} ({})", crate::i18n::label(&r.label()), r.endpoint))
        .unwrap_or_else(|| crate::i18n::text("unknown", &[]));
    let receive = s
        .voice_receive_route
        .as_ref()
        .map(|r| format!("{} ({})", crate::i18n::label(&r.label()), r.endpoint))
        .unwrap_or_else(|| crate::i18n::text("unknown", &[]));
    let title = crate::i18n::text(
        "Voice server: {0}. Audio send: {1}. Audio receive: {2}. Signaling: {3}. This is your connection; other participants can use different routes. Chat/control uses the separate Server indicator.",
        &[
            location.to_owned(),
            send.clone(),
            receive.clone(),
            signaling.to_owned(),
        ],
    );
    let channel = s.server_voice_channel();
    let peers: Vec<_> = s
        .voice_states
        .iter()
        .filter(|v| v.channel_id == channel && channel.is_some())
        .filter(|v| {
            s.self_user
                .as_ref()
                .is_none_or(|u| u.pubkey != v.user_pubkey)
        })
        .map(|v| {
            (
                s.display_name(&v.user_pubkey),
                s.peer_routes.get(&v.user_pubkey).cloned(),
            )
        })
        .collect();
    rsx! {
        button { class: "dxf-voice-route-badge shrink-0 px-2 py-1 text-[10px] text-[var(--text-muted)] border-l border-[var(--border)]", title: "{title}",
            onclick: move |_| open.toggle(),
            span { class: "dxf-route-full", {crate::i18n::text("Voice: {0} · {1}", &[(location).to_string(), (phase).to_string()])} }
            span { class: "dxf-route-mobile",
                if cfg!(target_os = "android") {
                    span { dangerous_inner_html: super::icons::SPEAKER }
                    span { class: "dxf-voice-phase", {crate::i18n::text("Voice", &[])}, ": {mobile_phase}" }
                } else {
                    {crate::i18n::text("Voice", &[])}, ": {mobile_phase}"
                }
            }
        }
        if open() {
            div { class: "dxf-no-drag rounded-md border border-[var(--border)] bg-[var(--panel2)] p-4 text-[12px]",
                style: "position: fixed; top: 60px; right: 16px; z-index: 1000; max-width: min(520px, 90vw); max-height: 75vh; overflow: auto; white-space: normal;",
                div { class: "flex items-center justify-between gap-2",
                    strong { {crate::i18n::text("Connection routes", &[])} }
                    button { onclick: move |_| open.set(false), {crate::i18n::text("Close", &[])} }
                }
                p { {crate::i18n::text("Your voice server: {0}", &[(location).to_string()])} }
                p { {crate::i18n::text("Audio send: {0}", std::slice::from_ref(&(send)))} }
                p { {crate::i18n::text("Audio receive: {0}", std::slice::from_ref(&(receive)))} }
                p { style: "overflow-wrap: anywhere;", {crate::i18n::text("Voice signaling: {0}", &[(signaling).to_string()])} }
                p { {crate::i18n::text("Participants in this call — updated when routes change or participants join. No IP addresses are shared.", &[])} }
                for (name, report) in peers {
                    div { class: "border-t border-[var(--border)] py-2",
                        strong { "{name}" }
                        if let Some(report) = report {
                            p { {crate::i18n::text("Server: {0}", &[crate::i18n::label(&report.gateway).to_owned()])} }
                            p { {crate::i18n::text("Audio send: {0}", &[crate::i18n::label(report.sent.as_deref().unwrap_or("unknown")).to_owned()])} }
                            p { {crate::i18n::text("Audio receive: {0}", &[crate::i18n::label(report.received.as_deref().unwrap_or("unknown")).to_owned()])} }
                        } else {
                            p { {crate::i18n::text("Waiting for route report…", &[])} }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn GuildDialogHost() -> Element {
    let mut state = use_app_state();
    let open = use_memo(move || state.read().guild_dialog);
    let close = move |_| state.write().guild_dialog = None;

    match open() {
        None => rsx! { Fragment {} },
        Some(crate::state::GuildDialog::Accent(gid)) => rsx! {
            for guild_id in std::iter::once(gid) {
                super::accent_palette::GuildAccentDialog { key: "{guild_id}", guild_id, on_close: close }
            }
        },
        Some(crate::state::GuildDialog::Settings(gid)) => rsx! {
            crate::features::guild_settings::GuildSettingsDialog { guild_id: gid, on_close: close }
        },
        Some(crate::state::GuildDialog::Invite(gid)) => rsx! {
            super::invite::InviteDialog { guild_id: gid, on_close: close }
        },
        Some(crate::state::GuildDialog::VoiceInvite(cid)) => rsx! {
            for channel_id in std::iter::once(cid) {
                super::voice_invite::VoiceInviteDialog { key: "{channel_id}", channel_id, on_close: close }
            }
        },
        Some(crate::state::GuildDialog::Integrations(gid)) => rsx! {
            crate::features::integrations::IntegrationsDialog { guild_id: gid, on_close: close }
        },
        Some(crate::state::GuildDialog::Roles(gid)) => rsx! {
            crate::features::guild_settings::GuildSettingsDialog {
                guild_id: gid,
                open_on: crate::features::guild_settings::GuildTab::Roles,
                on_close: close,
            }
        },
        Some(crate::state::GuildDialog::AddGuild) => rsx! {
            crate::features::guild_hub::AddGuildDialog { on_close: close }
        },
    }
}

#[cfg(test)]
mod rail_layout_tests {
    use super::*;
    use crate::features::{stream_layout, workspace_layout};

    const RAIL: f64 = workspace_layout::RAIL_DEFAULT as f64;

    #[test]
    fn rail_keeps_its_pixels_across_templates_and_window_resize_cycles() {
        for (name, template, _) in LAYOUT_TEMPLATES {
            let mut base = stream_layout::Arrangement {
                cells: template(),
                free: if *name == "Default" {
                    default_free_layout()
                } else {
                    vec![]
                },
            };
            for width in [900.0, 1920.0, 3840.0, 1280.0, 2560.0] {
                let next =
                    workspace_layout::compact_rail(&base, (width, 1080.0), GRID_GAP, RAIL).unwrap();
                let rail = next.iter().find(|(id, _)| id == "guilds").unwrap().1;
                assert!((rail.w * width - GRID_GAP - RAIL).abs() < 1e-6, "{name}");
                let channels = next.iter().find(|(id, _)| id == "channels").unwrap().1;
                assert!((channels.x - rail.w).abs() < 1e-6, "{name}");
                let right = next.iter().map(|(_, r)| r.x + r.w).fold(0.0, f64::max);
                assert!((right - 1.0).abs() < 1e-6, "{name}");
                for (id, rect) in &next {
                    let cell = base.cells.iter().find(|(key, _)| key == id).unwrap().1;
                    assert!((rect.y - cell.y as f64 / 30.0).abs() < 1e-6);
                    assert!((rect.h - cell.h as f64 / 30.0).abs() < 1e-6);
                    assert!(rect.x >= 0.0 && rect.w > 0.0 && rect.x + rect.w <= 1.0 + 1e-6);
                }
                base.free = next;
                assert!(
                    workspace_layout::compact_rail(&base, (width, 1080.0), GRID_GAP, RAIL)
                        .is_none()
                );
            }
        }
    }

    #[test]
    fn compacting_preserves_stream_and_chat_stacking() {
        let base = stream_layout::above_chat(
            stream_layout::Arrangement {
                cells: default_layout(),
                free: default_free_layout(),
            },
            12,
            GRID_ROWS,
        );
        let next = workspace_layout::compact_rail(&base, (2560.0, 1440.0), GRID_GAP, RAIL).unwrap();
        let streams = next.iter().find(|(id, _)| id == "streams").unwrap().1;
        let chat = next.iter().find(|(id, _)| id == "chat").unwrap().1;
        assert_eq!((streams.x, streams.w), (chat.x, chat.w));
        assert!((streams.y + streams.h - chat.y).abs() < 1e-6);
        assert!((chat.y + chat.h - 1.0).abs() < 1e-6);
    }

    #[test]
    fn moved_rails_and_unmeasured_windows_keep_their_layout() {
        let mut base = stream_layout::Arrangement {
            cells: default_layout(),
            free: vec![],
        };
        for width in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(
                workspace_layout::compact_rail(&base, (width, 1080.0), GRID_GAP, RAIL).is_none()
            );
        }
        base.free
            .push(("guilds".into(), FloatRect::new(0.4, 0.1, 0.2, 0.8)));
        assert!(workspace_layout::compact_rail(&base, (2560.0, 1440.0), GRID_GAP, RAIL).is_none());
    }

    fn of(rects: &[(String, FloatRect)], id: &str) -> FloatRect {
        rects.iter().find(|(key, _)| key == id).unwrap().1
    }

    #[test]
    fn docking_top_or_bottom_keeps_the_others_side_by_side_in_what_is_left() {
        let base = stream_layout::Arrangement {
            cells: default_layout(),
            free: default_free_layout(),
        };
        let size = (1920.0, 1080.0);
        for dock in [workspace_layout::Dock::Top, workspace_layout::Dock::Bottom] {
            let docked = workspace_layout::dock_rail(&base, dock, size, GRID_GAP, RAIL).unwrap();
            let rail = of(&docked, "guilds");
            assert_eq!(workspace_layout::dock_of(rail), Some(dock));
            assert!((rail.h * size.1 - GRID_GAP - RAIL).abs() < 1e-6);
            let (channels, chat, members) = (
                of(&docked, "channels"),
                of(&docked, "chat"),
                of(&docked, "members"),
            );
            assert!(channels.x.abs() < 1e-6 && (members.x + members.w - 1.0).abs() < 1e-6);
            assert!((channels.x + channels.w - chat.x).abs() < 1e-6);
            for panel in [channels, chat, members] {
                assert!((panel.h - (1.0 - rail.h)).abs() < 1e-6);
                let overlaps =
                    panel.y < rail.y + rail.h - 1e-6 && rail.y < panel.y + panel.h - 1e-6;
                assert!(!overlaps, "{dock:?}");
            }

            let taller = stream_layout::Arrangement {
                cells: default_layout(),
                free: docked,
            };
            let held =
                workspace_layout::compact_rail(&taller, (1920.0, 1440.0), GRID_GAP, RAIL).unwrap();
            assert!((of(&held, "guilds").h * 1440.0 - GRID_GAP - RAIL).abs() < 1e-6);
            assert_eq!(workspace_layout::dock_of(of(&held, "guilds")), Some(dock));
            let chat = of(&held, "chat");
            assert!(chat.y >= 0.0 && chat.y + chat.h <= 1.0 + 1e-6);
        }
    }

    #[test]
    fn docking_back_to_the_left_restores_the_columns() {
        let base = stream_layout::Arrangement {
            cells: default_layout(),
            free: default_free_layout(),
        };
        let size = (1920.0, 1080.0);
        let top =
            workspace_layout::dock_rail(&base, workspace_layout::Dock::Top, size, GRID_GAP, RAIL)
                .unwrap();
        let back = workspace_layout::dock_rail(
            &stream_layout::Arrangement {
                cells: default_layout(),
                free: top,
            },
            workspace_layout::Dock::Left,
            size,
            GRID_GAP,
            RAIL,
        )
        .unwrap();
        let rail = of(&back, "guilds");
        assert_eq!(
            workspace_layout::dock_of(rail),
            Some(workspace_layout::Dock::Left)
        );
        assert!((of(&back, "channels").x - rail.w).abs() < 1e-6);
        for id in ["channels", "chat", "members"] {
            let panel = of(&back, id);
            assert!(panel.y.abs() < 1e-6 && (panel.h - 1.0).abs() < 1e-6, "{id}");
        }
    }
}

#[cfg(test)]
mod accent_tests {
    use super::{guild_accent_to_apply, workspace_accent_style};

    const GUILD: Option<String> = None;

    fn branded() -> Option<String> {
        Some("#3355ff".to_string())
    }

    #[test]
    fn a_guild_that_branded_itself_wins_by_default() {
        assert_eq!(
            guild_accent_to_apply(false, false, branded()),
            branded(),
            "the behaviour that existed before the setting"
        );
    }

    #[test]
    fn refusing_it_lets_the_personal_accent_through() {
        assert_eq!(guild_accent_to_apply(false, true, branded()), None);
    }

    #[test]
    fn direct_messages_are_nobodys_guild() {
        assert_eq!(guild_accent_to_apply(true, false, branded()), None);
    }

    #[test]
    fn an_unbranded_guild_overrides_nothing_either_way() {
        assert_eq!(guild_accent_to_apply(false, false, GUILD), None);
        assert_eq!(guild_accent_to_apply(false, true, GUILD), None);
    }

    #[test]
    fn leaving_a_branded_guild_explicitly_resets_every_accent_property() {
        for (dm, keep_mine, accent) in [
            (true, false, branded()),
            (false, true, branded()),
            (false, false, None),
        ] {
            let branded_style = workspace_accent_style(branded());
            let reset = workspace_accent_style(guild_accent_to_apply(dm, keep_mine, accent));
            for declaration in branded_style.split(';').filter(|s| !s.trim().is_empty()) {
                let property = declaration.split_once(':').unwrap().0.trim();
                assert!(
                    reset.contains(&format!("{property}: inherit;")),
                    "{property}"
                );
            }
        }
    }
}
