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
use crate::state::{
    AppState, ConnectionStatus, SessionParams, VoicePhase, use_app_state, use_gateway,
};

const GRID_ROWS: u32 = 30;
const GRID_GAP: f64 = 8.0;

fn persist_layout(
    mut settings: Signal<crate::settings::ClientSettings>,
    layout: dioxus_grid_layout::LayoutStore,
) {
    let mut next = settings.read().clone();
    next.layout_cells = layout
        .snapshot()
        .into_iter()
        .map(|(id, p)| (id, [p.x, p.y, p.w, p.h]))
        .collect();
    next.layout_free = layout
        .free_snapshot()
        .into_iter()
        .map(|(id, r)| (id, [r.x, r.y, r.w, r.h]))
        .collect();
    settings.set(next.clone());
    crate::settings::save(&next);
}

fn default_layout() -> Vec<(String, GridPosition)> {
    vec![
        ("guilds".into(), GridPosition::new(0, 0, 1, GRID_ROWS)),
        ("channels".into(), GridPosition::new(1, 0, 2, GRID_ROWS)),
        ("chat".into(), GridPosition::new(3, 0, 7, GRID_ROWS)),
        ("members".into(), GridPosition::new(10, 0, 2, GRID_ROWS)),
    ]
}

const UNPLUG_ICON_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M18.36 6.64a9 9 0 1 1-12.73 0"/><line x1="12" y1="2" x2="12" y2="12"/></svg>"##;

#[component]
pub fn WorkspaceView(params: SessionParams, on_disconnect: EventHandler<String>) -> Element {
    let state = use_signal(AppState::empty);
    let settings = use_context::<Signal<crate::settings::ClientSettings>>();

    let (gateway_tx, voice_tx, nostr_tx) = use_hook(|| {
        {
            let saved = settings.read();
            let mut app = state;
            let mut w = app.write();
            w.mic_sensitivity = saved.mic_sensitivity.clamp(1, 1000);
            w.mic_volume = saved.mic_volume.min(200);
            w.auto_gain_control = saved.auto_gain_control;
            w.noise_cancellation = saved.noise_cancellation;
            w.bypass_system_audio_processing =
                saved.bypass_system_audio_processing && crate::rawmic::supported();
            w.denoise_atten_lim_db = saved.denoise_atten_lim_db.clamp(
                crate::features::voice::DENOISE_ATTEN_LIM_DB_MIN,
                crate::features::voice::DENOISE_ATTEN_LIM_DB_MAX,
            );
            w.voice_bitrate_kbps = match saved.voice_bitrate_kbps {
                24 => 24,
                _ => 48,
            };
            w.selected_input_device = saved.selected_input_device.clone();
            w.selected_output_device = saved.selected_output_device.clone();
            w.dm_cleared_at = saved.dm_cleared_at.iter().cloned().collect();
            w.dm_read_at = saved.dm_read_at.iter().cloned().collect();
            w.muted_channels = saved.muted_channels.iter().copied().collect();
            w.muted_guilds = saved.muted_guilds.iter().copied().collect();
        }
        // Audio prefs must be restored before this: the service seeds its live
        // controls from AppState on the first poll.
        let voice_tx = spawn_voice_service(state);
        let gateway_tx = spawn_gateway(params.clone(), state, voice_tx.clone(), move |reason| {
            on_disconnect.call(reason);
        });
        let relays = {
            let saved = settings.read();
            if saved.dm_relays.is_empty() {
                crate::nostr::relay::DEFAULT_RELAYS
                    .iter()
                    .map(|s| s.to_string())
                    .collect()
            } else {
                saved.dm_relays.clone()
            }
        };
        let nostr_tx = crate::nostr::service::spawn_nostr(params.identity.clone(), relays, state);
        (gateway_tx, voice_tx, nostr_tx)
    });
    provide_context(gateway_tx.clone());
    provide_context(nostr_tx.clone());
    provide_context(crate::features::voice::VoiceTx(voice_tx.clone()));
    provide_context(state);
    provide_context(params.identity.clone());
    crate::state::use_dm_read_persistence(state);

    let mut layout = use_layout_store(|| {
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
            return;
        }
        let mut store = layout;
        for (id, [x, y, w, h]) in &saved.layout_free {
            store.set_free(id.clone(), FloatRect::new(*x, *y, *w, *h));
        }
    });

    let mut edit_mode = use_signal(|| false);
    let status = state.read().status;

    let guild_accent_style = {
        let s = state.read();
        let accent = if s.dm_mode {
            None
        } else {
            s.selected_guild.and_then(|gid| {
                s.guilds
                    .iter()
                    .find(|g| g.id == gid)
                    .and_then(|g| g.accent.clone())
            })
        };
        accent
            .map(|a| crate::app::accent_vars(&a))
            .unwrap_or_default()
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

    rsx! {
        div { class: "h-full w-full flex flex-col bg-[var(--bg)] p-2 gap-2 {mac_top_pad}",
            style: "{guild_accent_style}",
            VoiceSounds {}
            crate::features::sounds::MessageSounds {}
            VoiceSpeakingBridge {}
            ErrorToast {}
            crate::features::activities::ActivityHost {}
            crate::features::screenshare::ScreenShareBridge {}
            crate::features::screenshare::ScreenSourcePicker {}
            crate::features::screenshare::ScreenSelfPreview {}
            crate::features::camera::CameraBridge {}
            crate::mediakey::MediaKeyBridge {}
            crate::features::camera::CameraSelfPreview {}
            crate::features::camera::CameraGridWindow {}
            crate::features::screenshare::ScreenWatchWindow {}
            crate::features::profiles::ProfileCard {}
            crate::features::chat::ImageViewer {}
            GuildDialogHost {}

            div { class: "dxf-drag-region flex items-center gap-2 {mac_titlebar_clear}",
                onmousedown: move |_| crate::app::start_window_drag(),
                div { class: "shrink-0 flex items-center gap-2 px-1",
                    crate::app::DiscordiaLogo { class: "w-6 h-6" }
                    span { class: "dxf-display dxf-wordmark text-lg font-bold tracking-tight", "Discordia" }
                }
                HostBanner {}
                // One chip: what the connection is, and the button that ends it.
                // Named, so it is never read as the voice hang-up downstairs.
                div {
                    class: "dxf-no-drag shrink-0 flex items-center rounded-md border border-[var(--border)] bg-[var(--panel2)] overflow-hidden",
                    onmousedown: move |e| e.stop_propagation(),
                    TransportBadge {}
                    EncryptionBadge {}
                    button {
                        class: "h-8 px-2 flex items-center gap-1.5 border-l border-[var(--border)] text-[10px] uppercase tracking-wider text-[var(--text-muted)] hover:text-[var(--danger)] transition-colors",
                        title: "Disconnect from this server",
                        onclick: move |_| on_disconnect.call(String::new()),
                        span { class: "block w-4 h-4", dangerous_inner_html: UNPLUG_ICON_SVG }
                        "Disconnect"
                    }
                }
                // Settings and appearance sit in the corner of the window, not
                // beside your name: they are about the app, not about you.
                div { class: "dxf-no-drag ml-auto shrink-0 flex items-center gap-1.5",
                    onmousedown: move |e| e.stop_propagation(),
                    crate::features::appearance::AppearanceButton {}
                    button {
                        class: "w-8 h-8 flex items-center justify-center rounded-lg border border-[var(--border)] bg-[var(--panel2)] text-[var(--text-muted)] hover:text-[var(--accent)] hover:border-[var(--accent)] transition-colors",
                        title: "Settings",
                        onclick: move |_| {
                            let open = !state.read().audio_settings;
                            state.clone().write().audio_settings = open;
                        },
                        span { class: "block w-4 h-4", dangerous_inner_html: crate::features::icons::GEAR }
                    }
                }
            }

            div { class: "flex-1 overflow-auto min-h-0",
                GridLayout {
                    cols: 12, rows: GRID_ROWS, gap: GRID_GAP,
                    store: layout, editable: edit_mode(),
                    mode: LayoutMode::Free,
                    on_change: move |_: Vec<(String, GridPosition)>| persist_layout(settings, layout),
                    GridItem { id: "guilds", x: 0, y: 0, w: 1, h: GRID_ROWS, min_w: 1, min_h: 10,
                        GuildsSidebar {}
                    }
                    GridItem { id: "channels", x: 1, y: 0, w: 2, h: GRID_ROWS, min_w: 2, min_h: 10,
                        ChannelsColumn {}
                    }
                    GridItem { id: "chat", x: 3, y: 0, w: 7, h: GRID_ROWS, min_w: 3, min_h: 10,
                        // The log is the darkest surface in the window: the
                        // panels around it read as chrome only if they sit above it.
                        div { class: "panel-hover w-full h-full flex flex-col bg-[var(--bg)] border border-[var(--border)] rounded-xl overflow-hidden",
                            if status == ConnectionStatus::Connecting {
                                div { class: "flex-1 flex items-center justify-center text-[var(--text-muted)] text-sm",
                                    "Connecting…"
                                }
                            } else {
                                ChatView {}
                            }
                        }
                    }
                    GridItem { id: "members", x: 10, y: 0, w: 2, h: GRID_ROWS, min_w: 2, min_h: 10,
                        MembersPanel {}
                    }
                }
            }

            div { class: "fixed bottom-3 right-3 z-40 flex items-center gap-1.5",
                if edit_mode() {
                    button {
                        class: "border border-[var(--border)] rounded px-3 py-1 text-[10px] uppercase tracking-wider bg-[var(--panel)] hover:border-[var(--danger)] text-[var(--text-muted)] hover:text-[var(--danger)] transition-colors",
                        title: "Put the panels back the way they started",
                        onclick: move |_| {
                            layout.restore(default_layout(), Vec::new());
                            persist_layout(settings, layout);
                        },
                        "Reset"
                    }
                }
                button {
                    class: "border border-[var(--border)] rounded px-3 py-1 text-[10px] uppercase tracking-wider bg-[var(--panel)] hover:border-[var(--accent)] text-[var(--text-muted)] hover:text-[var(--accent)] transition-colors",
                    onclick: move |_| edit_mode.set(!edit_mode()),
                    if edit_mode() { "Done" } else { "Edit layout" }
                }
            }
        }
    }
}

#[component]
fn ErrorToast() -> Element {
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
            span { class: "text-xs text-[var(--text)] flex-1", "{msg}" }
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
            VoicePhase::Idle | VoicePhase::Error if prev == VoicePhase::Connected => {
                sfx("disconnect")
            }
            _ => {}
        }
    });

    let viewing = use_memo(move || state.read().screen_viewing.clone());
    let mut last_viewing = use_signal(|| None::<String>);
    use_effect(move || {
        let now = viewing();
        if now == *last_viewing.peek() {
            return;
        }
        let was_watching = last_viewing.peek().is_some();
        last_viewing.set(now.clone());
        match now {
            Some(_) => sfx("watch-start"),
            None if was_watching => sfx("watch-stop"),
            None => {}
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

    let muted = use_memo(move || state.read().voice.muted);
    let mut last_muted = use_signal(|| false);
    use_effect(move || {
        let now = muted();
        if now == *last_muted.peek() {
            return;
        }
        last_muted.set(now);
        if now {
            sfx("mute");
        } else {
            sfx("unmute");
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
    let mut last_channel = use_signal(|| None::<Id>);
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
        if ch != *last_channel.peek() {
            last_channel.set(ch);
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
        if ch != *last_channel.peek() {
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

#[component]
fn HostBanner() -> Element {
    let state = use_app_state();
    let snapshot = state.read();
    let Some(info) = snapshot.host_info.clone() else {
        return rsx! { Fragment {} };
    };
    drop(snapshot);

    let mut copied = use_signal(|| false);
    let lan_text = info.lan_url.clone();
    let shortcode = info.shortcode.clone();
    let publish_error = info.publish_error.clone();
    let listed_public = info.listed_public;
    let has_shortcode = info.shortcode.is_some();
    let reachability = info.reachability.clone();
    let (voice_label, voice_color) = if info.livekit_url.is_empty() {
        ("voice unavailable", "text-[var(--warn)]")
    } else if info.voice_bundled {
        ("voice ready", "text-[var(--success)]")
    } else {
        ("voice via rendezvous", "text-[var(--success)]")
    };

    rsx! {
        div { class: "dxf-no-drag shrink-0 h-8 pl-2.5 pr-1 bg-[var(--panel2)] border border-[var(--border)] rounded-lg flex items-center gap-2 text-xs",
            onmousedown: move |e| e.stop_propagation(),
            span {
                class: "w-2 h-2 rounded-full shrink-0",
                style: "background: var(--success);",
                title: "Hosting from this machine",
            }
            span { class: "text-[11.5px] text-[var(--text-dim)]", "self-hosted" }
            // The code is what you hand a friend, so it is the one thing here
            // sized to be read and copied rather than skimmed.
            if let Some(code) = shortcode {
                code { class: "font-mono text-[12px] text-[var(--text)] select-all", "{code}" }
                button {
                    class: "w-6 h-6 shrink-0 flex items-center justify-center rounded text-[var(--text-dim)] hover:text-[var(--accent)] transition-colors",
                    title: if copied() { "Copied" } else { "Copy the invite code" },
                    onclick: move |_| {
                        let js = crate::features::screenshare::js_str(&code);
                        let _ = document::eval(&format!(
                            "navigator.clipboard && navigator.clipboard.writeText({js});"
                        ));
                        copied.set(true);
                    },
                    span {
                        class: "block w-3.5 h-3.5",
                        dangerous_inner_html: crate::features::icons::COPY,
                    }
                }
            }
            span {
                class: "pl-2 font-mono text-[10.5px] text-[var(--text-dim)] truncate max-w-52 border-l border-[var(--border)]",
                title: "The address friends on this network can use",
                "{lan_text}"
            }
            if let Some(err) = publish_error {
                span { class: "text-[var(--text-dim)]", "·" }
                span { class: "text-[var(--danger)]",
                    title: "{err}",
                    "⚠ not published — {err}"
                }
            } else if has_shortcode && !listed_public {
                span { class: "text-[var(--text-dim)]", "·" }
                span { class: "text-[var(--text-muted)]",
                    title: "Reachable by code, but not shown in the public list. Turn on public listing when you create the server to appear there.",
                    "unlisted"
                }
            }
            span { class: "flex-1" }
            Reachability { reachability }
            span { class: "{voice_color}", "● {voice_label}" }
        }
    }
}

#[component]
fn Reachability(reachability: crate::host::Reachability) -> Element {
    use crate::host::Reachability as R;
    match reachability {
        R::Direct {
            endpoint,
            method,
            media,
        } => {
            let title = if media {
                format!(
                    "{method} mapped this machine's ports. Friends reach you at {endpoint} without the relay, voice included."
                )
            } else {
                format!(
                    "{method} mapped the chat port ({endpoint}), but not the voice ports — calls still go through a relay's SFU, or stay on this network."
                )
            };
            rsx! {
                span { class: "text-[var(--text-dim)]", "·" }
                span { class: "text-[var(--success)]", title: "{title}",
                    if media { "● reachable directly" } else { "● reachable directly (chat only)" }
                }
            }
        }
        R::LanOnly { reason } => rsx! {
            span { class: "text-[var(--text-dim)]", "·" }
            span { class: "text-[var(--warn)]",
                title: "{reason} Friends elsewhere can still join by code — the rendezvous relays them.",
                "● this network only"
            }
        },
        R::LoopbackOnly => rsx! {
            span { class: "text-[var(--text-dim)]", "·" }
            span { class: "text-[var(--text-muted)]",
                title: "Direct connections weren't enabled, so the gateway only listens on this machine. Friends can still join by code through the rendezvous.",
                "● local only"
            }
        },
    }
}

#[component]
fn EncryptionBadge() -> Element {
    let state = use_app_state();
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
        span { class: "shrink-0 px-2 py-1 text-[10px] uppercase tracking-wider {color}",
            title: "{title}",
            "{label}"
        }
    }
}

#[component]
fn TransportBadge() -> Element {
    let state = use_app_state();
    let snapshot = state.read();
    if snapshot.host_info.is_some() {
        return rsx! { Fragment {} };
    }
    let transport = snapshot.transport;
    drop(snapshot);

    let (label, color, title) = match transport {
        crate::state::Transport::Loopback => return rsx! { Fragment {} },
        crate::state::Transport::Private => (
            "private",
            "text-[var(--success)]",
            "Connected straight to the host over an encrypted QUIC transport, with the host authenticated by its public key. Nobody in between can read this connection. The host can see your IP address.",
        ),
        crate::state::Transport::Direct => (
            "direct",
            "text-[var(--warn)]",
            "Connected straight to the host — no relay in the middle — but in the clear: every hop on the path can read this connection. The host can see your IP address.",
        ),
        crate::state::Transport::Relayed => (
            "relayed",
            "text-[var(--text-muted)]",
            "Carried by a rendezvous relay, which can read everything on this connection. The host never learns your address.",
        ),
    };
    rsx! {
        span { class: "shrink-0 px-2 py-1 text-[10px] uppercase tracking-wider {color}",
            title: "{title}",
            "{label}"
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
        Some(crate::state::GuildDialog::Settings(gid)) => rsx! {
            crate::features::guild_settings::GuildSettingsDialog { guild_id: gid, on_close: close }
        },
        Some(crate::state::GuildDialog::Integrations(gid)) => rsx! {
            crate::features::integrations::IntegrationsDialog { guild_id: gid, on_close: close }
        },
        Some(crate::state::GuildDialog::Roles(gid)) => rsx! {
            crate::features::roles::RolesDialog { guild_id: gid, on_close: close }
        },
    }
}
