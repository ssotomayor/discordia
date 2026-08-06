use dioxus::prelude::*;
use dioxus_grid_layout::NoDrag;

use crate::features::voice::{VoiceCmd, use_voice_tx};
use crate::identity::discriminator;
use crate::protocol::{Channel, ChannelKind, ClientMessage, DmInfo, Id, Permission, VoiceState};
use crate::state::{AppState, GatewayTx, VoicePhase, use_app_state, use_gateway};

/// Right-click management menu over a channel row (`ManageChannels` only).
#[derive(Clone, PartialEq)]
struct ChanMenu {
    channel: Channel,
    x: f64,
    y: f64,
    mode: ChanMenuMode,
}

#[derive(Clone, PartialEq)]
enum ChanMenuMode {
    Menu,
    /// Inline rename + topic form (buffers are in the popover component).
    Edit,
    ConfirmDelete,
}

/// Drag interaction for the audio settings popover. Same Move model as the
/// floating activity/screen-share windows — no resize grip (popover is small).
#[derive(Clone, Copy, PartialEq)]
enum AudioDrag {
    Move { dx: f64, dy: f64 },
}

const PANEL: &str = "panel-hover w-full h-full bg-[var(--panel)] border border-[var(--border)] rounded-lg flex flex-col overflow-hidden";
const HEADER: &str = "h-11 px-3 flex items-center border-b border-[var(--border)]";
const SECTION_LABEL: &str = "px-2 py-1 text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)]";

#[component]
pub fn ChannelsColumn() -> Element {
    let mut state = use_app_state();
    let gateway = use_gateway();
    let voice = use_voice_tx();

    let snapshot = state.read();
    let dm_mode = snapshot.dm_mode;
    let dms: Vec<DmInfo> = snapshot.dms.clone();
    let selected_guild = snapshot.selected_guild;
    let selected_channel = snapshot.selected_channel;
    let guild = selected_guild.and_then(|gid| snapshot.guilds.iter().find(|g| g.id == gid).cloned());
    let channels: Vec<Channel> = selected_guild
        .map(|gid| {
            let mut v: Vec<Channel> = snapshot
                .channels
                .iter()
                .filter(|c| c.guild_id == gid)
                .cloned()
                .collect();
            v.sort_by(|a, b| a.position.cmp(&b.position).then_with(|| a.name.cmp(&b.name)));
            v
        })
        .unwrap_or_default();
    let voice_states: Vec<VoiceState> = snapshot.voice_states.clone();
    let self_user = snapshot.self_user.clone();
    let self_voice = snapshot.voice.clone();
    let can_manage_channels = selected_guild
        .map(|gid| snapshot.can(gid, Permission::ManageChannels))
        .unwrap_or(false);
    drop(snapshot);

    let text_channels: Vec<&Channel> = channels
        .iter()
        .filter(|c| matches!(c.kind, ChannelKind::Text))
        .collect();
    let voice_channels: Vec<&Channel> = channels
        .iter()
        .filter(|c| matches!(c.kind, ChannelKind::Voice))
        .collect();

    let mut chan_menu = use_signal::<Option<ChanMenu>>(|| None);
    let mut show_create = use_signal(|| false);

    rsx! {
        aside { class: PANEL,
            div { class: HEADER,
                h2 { class: "text-sm text-[var(--accent)] truncate font-medium flex-1",
                    if dm_mode {
                        "Direct Messages"
                    } else {
                        {guild.as_ref().map(|g| g.name.clone()).unwrap_or_else(|| "No server".into())}
                    }
                }
                if !dm_mode && can_manage_channels {
                    button {
                        class: "w-6 h-6 flex items-center justify-center rounded border border-dashed border-[var(--border)] text-[var(--text-muted)] hover:text-[var(--accent)] hover:border-[var(--accent)] text-sm leading-none transition-colors",
                        title: "Create a channel",
                        onclick: move |_| show_create.set(!show_create()),
                        "+"
                    }
                }
            }

            if show_create() {
                if let Some(gid) = selected_guild {
                    CreateChannelForm {
                        guild_id: gid,
                        on_done: move |_| show_create.set(false),
                    }
                }
            }

            NoDrag {
            if dm_mode {
                div { class: "flex-1 overflow-y-auto px-2 py-3 space-y-1",
                    if dms.is_empty() {
                        div { class: "px-2 text-xs text-[var(--text-dim)] leading-relaxed",
                            "No conversations yet. Click a member to start a direct message."
                        }
                    }
                    for dm in dms.iter().cloned() {
                        {
                            let cid = dm.channel_id;
                            let active = selected_channel == Some(cid);
                            let cls = if active {
                                "text-[var(--accent)] bg-[var(--accent-soft)]"
                            } else {
                                "text-[var(--text-muted)] hover:text-[var(--text)] hover:bg-white/[0.03]"
                            };
                            let g2 = gateway.clone();
                            let uname = dm.other.username.clone();
                            let disc = discriminator(&dm.other.pubkey);
                            rsx! {
                                button {
                                    key: "{cid}",
                                    class: "w-full flex items-center gap-2 px-2 py-1 rounded text-left text-sm transition-colors {cls}",
                                    onclick: move |_| select_dm(&mut state, &g2, cid),
                                    crate::features::profiles::Avatar {
                                        pubkey: dm.other.pubkey.clone(),
                                        name: uname.clone(),
                                        size: "w-6 h-6",
                                        text: "text-[10px]",
                                    }
                                    span { class: "truncate flex-1",
                                        "{uname}"
                                        span { class: "text-[var(--text-dim)] font-mono text-[10px] ml-0.5", "#{disc}" }
                                    }
                                }
                            }
                        }
                    }
                }
            } else {
            div { class: "flex-1 overflow-y-auto px-2 py-3 space-y-3",
                if !text_channels.is_empty() {
                    div {
                        div { class: SECTION_LABEL, "Text channels" }
                        for channel in text_channels.iter() {
                            {
                                let ch = (*channel).clone();
                                let cid = ch.id;
                                let active = selected_channel == Some(cid);
                                let cls = if active {
                                    "text-[var(--accent)] bg-[var(--accent-soft)]"
                                } else {
                                    "text-[var(--text-muted)] hover:text-[var(--text)] hover:bg-white/[0.03]"
                                };
                                let g2 = gateway.clone();
                                let ctx_ch = ch.clone();
                                rsx! {
                                    button {
                                        key: "{cid}",
                                        class: "w-full flex items-center gap-1.5 px-2 py-1 rounded text-left text-sm transition-colors {cls}",
                                        onclick: move |_| select_text_channel(&mut state, &g2, cid),
                                        oncontextmenu: move |e: MouseEvent| {
                                            if can_manage_channels {
                                                e.prevent_default();
                                                let c = e.client_coordinates();
                                                chan_menu.set(Some(ChanMenu {
                                                    channel: ctx_ch.clone(),
                                                    x: c.x,
                                                    y: c.y,
                                                    mode: ChanMenuMode::Menu,
                                                }));
                                            }
                                        },
                                        span { class: "text-[var(--text-dim)]", "#" }
                                        span { class: "truncate flex-1", "{ch.name}" }
                                        if ch.read_only {
                                            span { class: "text-[10px] text-[var(--text-dim)]", title: "Read-only", "🔒" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                if !voice_channels.is_empty() {
                    div {
                        div { class: SECTION_LABEL, "Voice channels" }
                        for channel in voice_channels.iter() {
                            {
                                let ch = (*channel).clone();
                                let cid = ch.id;
                                let in_this = self_voice.channel_id == Some(cid);
                                let g_join = gateway.clone();
                                let g_leave = gateway.clone();
                                let v_leave = voice.clone();
                                let occupants: Vec<VoiceState> = voice_states
                                    .iter()
                                    .filter(|v| v.channel_id == Some(cid))
                                    .cloned()
                                    .collect();
                                let ctx_ch = ch.clone();
                                rsx! {
                                    div {
                                        key: "{cid}",
                                        oncontextmenu: move |e: MouseEvent| {
                                            if can_manage_channels {
                                                e.prevent_default();
                                                let c = e.client_coordinates();
                                                chan_menu.set(Some(ChanMenu {
                                                    channel: ctx_ch.clone(),
                                                    x: c.x,
                                                    y: c.y,
                                                    mode: ChanMenuMode::Menu,
                                                }));
                                            }
                                        },
                                        VoiceChannelRow {
                                            channel: ch.clone(),
                                            connected: in_this,
                                            occupants: occupants,
                                            self_pubkey: self_user.as_ref().map(|u| u.pubkey.clone()),
                                            on_join: move |_| {
                                                g_join.send(ClientMessage::JoinVoice { channel_id: cid });
                                            },
                                            on_leave: move |_| {
                                                g_leave.send(ClientMessage::LeaveVoice);
                                                v_leave.send(VoiceCmd::Disconnect);
                                            },
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            }

            UserPanel { self_voice: self_voice, self_username: self_user.map(|u| u.username) }
            }

            if let Some(m) = chan_menu() {
                ChannelMenuPopover {
                    menu: m,
                    on_close: move |_| chan_menu.set(None),
                    on_mode: move |mode: ChanMenuMode| {
                        if let Some(cur) = chan_menu.write().as_mut() {
                            cur.mode = mode;
                        }
                    },
                }
            }
        }
    }
}

/// Inline "create a channel" form under the header (mirrors CreateGuild's
/// expand-in-place pattern).
#[component]
fn CreateChannelForm(guild_id: Id, on_done: EventHandler<()>) -> Element {
    let gateway = use_gateway();
    let mut name = use_signal(String::new);
    let mut kind = use_signal(|| ChannelKind::Text);

    let mut submit = move || {
        let n = name().trim().to_string();
        if !n.is_empty() {
            gateway.send(ClientMessage::CreateChannel {
                guild_id,
                name: n,
                kind: kind(),
                topic: None,
            });
        }
        name.set(String::new());
        on_done.call(());
    };

    rsx! {
        form {
            class: "px-2 py-2 border-b border-[var(--border)] flex flex-col gap-1.5",
            onsubmit: move |_| submit(),
            input {
                class: "w-full bg-transparent border border-[var(--border)] focus:border-[var(--accent)] rounded px-2 py-1 text-xs text-[var(--text)] outline-none transition-colors",
                placeholder: "Channel name…",
                value: "{name}",
                autofocus: true,
                maxlength: 64,
                oninput: move |e| name.set(e.value()),
            }
            div { class: "flex gap-1",
                for (k, label) in [(ChannelKind::Text, "# Text"), (ChannelKind::Voice, "♪ Voice")] {
                    button {
                        r#type: "button",
                        class: if kind() == k {
                            "flex-1 rounded px-1 py-0.5 text-[10px] uppercase tracking-wider text-[var(--accent)] border border-[var(--accent)] transition-colors"
                        } else {
                            "flex-1 rounded px-1 py-0.5 text-[10px] uppercase tracking-wider text-[var(--text-dim)] border border-[var(--border)] hover:text-[var(--text-muted)] transition-colors"
                        },
                        onclick: move |_| kind.set(k),
                        "{label}"
                    }
                }
                button {
                    r#type: "submit",
                    class: "rounded px-2 py-0.5 text-[10px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors",
                    "Add"
                }
            }
        }
    }
}

/// Floating channel-management popover: rename/topic, read-only toggle (text
/// channels), delete with confirm.
#[component]
fn ChannelMenuPopover(
    menu: ChanMenu,
    on_close: EventHandler<()>,
    on_mode: EventHandler<ChanMenuMode>,
) -> Element {
    let gateway = use_gateway();
    let ch = menu.channel.clone();
    // Edit-form buffers, seeded from the channel.
    let mut name = use_signal(|| ch.name.clone());
    let mut topic = use_signal(|| ch.topic.clone().unwrap_or_default());

    rsx! {
        div {
            class: "fixed inset-0 z-50",
            onclick: move |_| on_close.call(()),
            oncontextmenu: move |e| { e.prevent_default(); on_close.call(()); },
            div {
                class: "dxf-pop-in absolute min-w-48 bg-[var(--panel-solid)] border border-[var(--border)] rounded-md shadow-lg p-1 text-sm",
                style: "left: {menu.x}px; top: {menu.y}px;",
                onclick: move |e| e.stop_propagation(),
                match menu.mode {
                    ChanMenuMode::Menu => {
                        let gw_ro = gateway.clone();
                        let ch_ro = ch.clone();
                        rsx! {
                            button {
                                class: "w-full text-left px-3 py-1.5 rounded text-[var(--text)] hover:bg-white/[0.04] transition-colors",
                                onclick: move |_| on_mode.call(ChanMenuMode::Edit),
                                "Edit name & topic"
                            }
                            if matches!(ch.kind, ChannelKind::Text) {
                                button {
                                    class: "w-full text-left px-3 py-1.5 rounded text-[var(--text)] hover:bg-white/[0.04] transition-colors",
                                    onclick: move |_| {
                                        // Full-replace toggle of read_only.
                                        gw_ro.send(ClientMessage::UpdateChannel {
                                            channel_id: ch_ro.id,
                                            name: ch_ro.name.clone(),
                                            topic: ch_ro.topic.clone(),
                                            read_only: !ch_ro.read_only,
                                            position: ch_ro.position,
                                            slowmode_secs: ch_ro.slowmode_secs,
                                        });
                                        on_close.call(());
                                    },
                                    if ch.read_only { "🔓 Make writable" } else { "🔒 Make read-only" }
                                }
                            }
                            button {
                                class: "w-full text-left px-3 py-1.5 rounded text-[var(--danger)] hover:bg-[var(--danger)]/10 transition-colors",
                                onclick: move |_| on_mode.call(ChanMenuMode::ConfirmDelete),
                                "Delete channel"
                            }
                        }
                    },
                    ChanMenuMode::Edit => {
                        let gw = gateway.clone();
                        let ch2 = ch.clone();
                        rsx! {
                            form {
                                class: "px-2 py-1.5 flex flex-col gap-1.5 min-w-52",
                                onsubmit: move |_| {
                                    let n = name().trim().to_string();
                                    if !n.is_empty() {
                                        let t = topic().trim().to_string();
                                        gw.send(ClientMessage::UpdateChannel {
                                            channel_id: ch2.id,
                                            name: n,
                                            topic: if t.is_empty() { None } else { Some(t) },
                                            read_only: ch2.read_only,
                                            position: ch2.position,
                                            slowmode_secs: ch2.slowmode_secs,
                                        });
                                    }
                                    on_close.call(());
                                },
                                input {
                                    class: "w-full bg-transparent border border-[var(--border)] focus:border-[var(--accent)] rounded px-2 py-1 text-xs text-[var(--text)] outline-none transition-colors",
                                    value: "{name}",
                                    maxlength: 64,
                                    autofocus: true,
                                    oninput: move |e| name.set(e.value()),
                                }
                                input {
                                    class: "w-full bg-transparent border border-[var(--border)] focus:border-[var(--accent)] rounded px-2 py-1 text-xs text-[var(--text)] outline-none transition-colors",
                                    placeholder: "Topic (optional)",
                                    value: "{topic}",
                                    maxlength: 120,
                                    oninput: move |e| topic.set(e.value()),
                                }
                                button {
                                    r#type: "submit",
                                    class: "rounded px-2 py-1 text-[10px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors",
                                    "Save"
                                }
                            }
                        }
                    },
                    ChanMenuMode::ConfirmDelete => {
                        let gw = gateway.clone();
                        let cid = ch.id;
                        rsx! {
                            div { class: "px-3 py-1.5 text-xs text-[var(--text-muted)]",
                                "Delete #{ch.name}? Its messages are gone for good."
                            }
                            div { class: "flex gap-1 px-1 pb-0.5",
                                button {
                                    class: "flex-1 px-2 py-1 rounded text-xs uppercase tracking-wider text-[var(--danger)] border border-[var(--danger)]/40 hover:bg-[var(--danger)]/10 transition-colors",
                                    onclick: move |_| {
                                        gw.send(ClientMessage::DeleteChannel { channel_id: cid });
                                        on_close.call(());
                                    },
                                    "Delete"
                                }
                                button {
                                    class: "flex-1 px-2 py-1 rounded text-xs uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--text-muted)] transition-colors",
                                    onclick: move |_| on_close.call(()),
                                    "Cancel"
                                }
                            }
                        }
                    },
                }
            }
        }
    }
}

#[component]
fn VoiceChannelRow(
    channel: Channel,
    connected: bool,
    occupants: Vec<VoiceState>,
    self_pubkey: Option<String>,
    on_join: EventHandler<()>,
    on_leave: EventHandler<()>,
) -> Element {
    let row_cls = if connected {
        "text-[var(--accent)] bg-[var(--accent-soft)]"
    } else {
        "text-[var(--text-muted)] hover:text-[var(--text)] hover:bg-white/[0.03]"
    };
    let mut state = use_app_state();
    let users_by_id = state.read();
    let sharers: Vec<String> = users_by_id.screen_sharers_in(channel.id).to_vec();

    rsx! {
        div { class: "rounded",
            button {
                class: "w-full flex items-center gap-1.5 px-2 py-1 rounded text-left text-sm transition-colors {row_cls}",
                onclick: move |_| {
                    if connected { on_leave.call(()) } else { on_join.call(()) }
                },
                span { class: "text-[var(--text-dim)] text-xs", "♪" }
                span { class: "truncate flex-1", "{channel.name}" }
                if connected {
                    span { class: "text-[9px] text-[var(--accent)] font-semibold uppercase tracking-wider", "live" }
                }
            }
            if !occupants.is_empty() {
                div { class: "ml-5 mt-0.5 space-y-0.5",
                    for vs in occupants.iter() {
                        {
                            let name = users_by_id
                                .user_of(&vs.user_pubkey)
                                .map(|u| u.username.clone())
                                .unwrap_or_else(|| crate::identity::truncate_pubkey(&vs.user_pubkey));
                            let is_self = self_pubkey.as_deref() == Some(vs.user_pubkey.as_str());
                            let dot = if vs.speaking { "bg-[var(--accent)]" } else { "bg-[var(--text-dim)]" };
                            let mute_badge = if vs.muted { Some("muted") } else { None };
                            let is_sharing = sharers.iter().any(|p| p == &vs.user_pubkey);
                            // Clickable to watch only when you're in the channel
                            // (so the JS room is connected) and it's not yourself.
                            let can_watch = is_sharing && connected && !is_self;
                            let watch_pk = vs.user_pubkey.clone();
                            rsx! {
                                div {
                                    key: "{vs.user_pubkey}",
                                    class: "flex items-center gap-1.5 px-2 py-0.5 text-xs text-[var(--text-muted)]",
                                    span { class: "w-1.5 h-1.5 rounded-full {dot}" }
                                    span { class: "truncate flex-1",
                                        "{name}"
                                        if is_self { " (you)" }
                                    }
                                    if let Some(badge) = mute_badge {
                                        span { class: "text-[9px] text-[var(--text-dim)] uppercase tracking-wider", "{badge}" }
                                    }
                                    if is_sharing {
                                        button {
                                            class: "flex items-center gap-1 text-[9px] uppercase tracking-wider text-[var(--danger)] font-semibold disabled:opacity-70 disabled:cursor-default",
                                            disabled: !can_watch,
                                            title: if can_watch { "Watch screen" } else { "Sharing screen" },
                                            onclick: move |_| {
                                                if can_watch {
                                                    state.write().screen_viewing = Some(watch_pk.clone());
                                                }
                                            },
                                            span { class: "w-1.5 h-1.5 rounded-full bg-[var(--danger)] dxf-dot-pulse", style: "color:var(--danger);" }
                                            "live"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn UserPanel(self_voice: crate::state::VoiceSession, self_username: Option<String>) -> Element {
    let gateway = use_gateway();
    let voice = use_voice_tx();
    let mut state = use_app_state();
    let mut settings = use_context::<Signal<crate::settings::ClientSettings>>();
    // Whether the embedded webview exposes getDisplayMedia; populated by the ScreenShareBridge.
    let screen_capture_available = state.read().screen_capture_available;
    let self_pubkey = state.read().self_user.as_ref().map(|u| u.pubkey.clone());
    let sharing = state.read().screen_sharing;
    let name = self_username.clone().unwrap_or_else(|| "—".into());
    // Own level + progress in the CURRENTLY SELECTED guild (XP is per-guild).
    let (level, xp_pct) = {
        let s = state.read();
        let xp = match (s.selected_guild, self_pubkey.as_ref()) {
            (Some(gid), Some(pk)) => s
                .members
                .iter()
                .find(|m| m.guild_id == gid && &m.user.pubkey == pk)
                .map(|m| m.xp)
                .unwrap_or(0),
            _ => 0,
        };
        let (lvl, into, span) = crate::protocol::level_progress(xp);
        (lvl, (into as f64 / span.max(1) as f64 * 100.0) as u32)
    };

    let show_banner = !matches!(self_voice.phase, VoicePhase::Idle);
    // Compact status: a colored dot (green = connected, pulsing) replaces the
    // old "VOICE connected" text so the row doesn't overflow the narrow column.
    let (dot_color, phase_text) = match self_voice.phase {
        VoicePhase::Idle => ("var(--text-dim)", "voice idle"),
        VoicePhase::Connecting => ("var(--warn)", "connecting…"),
        VoicePhase::Connected => ("var(--success)", "voice connected"),
        VoicePhase::Error => ("var(--danger)", "voice error"),
    };
    let voice_error = self_voice.error.clone();

    let muted = self_voice.muted;
    let mute_label = if muted { "unmute" } else { "mute" };
    let g_for_mute = gateway.clone();
    let v_for_mute = voice.clone();
    let g_for_hang = gateway.clone();
    let v_for_hang = voice.clone();
    let g_for_share = gateway.clone();
    let voice_channel = self_voice.channel_id;

    // UI state for audio settings popover. The popover is a freely-draggable
    // floating window (not anchored to the gear button), so we track pixel
    // position + an in-flight drag. Initial coords approximate the old
    // `right-3 top-12` anchor in a ~1280px viewport; the user moves it from
    // there. Position persists across open/close within the session.
    let mut show_audio_settings = use_signal(|| false);
    let mut audio_x = use_signal(|| 1000.0_f64);
    let mut audio_y = use_signal(|| 48.0_f64);
    let mut audio_drag = use_signal(|| None::<AudioDrag>);

    // Snapshot device lists & selections so RSX body can use them without
    // attempting inline `let` bindings inside the macro.
    let available_input_devices = state.read().available_input_devices.clone();
    let available_output_devices = state.read().available_output_devices.clone();
    let selected_input_device = state.read().selected_input_device.clone();
    let selected_output_device = state.read().selected_output_device.clone();
    let mic_sensitivity = state.read().mic_sensitivity;
    let mic_level = state.read().mic_level;

    // Clone voice sender for each closure so move into one closure doesn't
    // prevent reuse in others.
    let v_for_audio_button = voice.clone();
    let v_for_input_change = voice.clone();
    let v_for_output_change = voice.clone();
    let v_for_sensitivity = voice.clone();

    // Snapshot current voice phase so the popover can show reconnection state.
    let voice_phase = state.read().voice.phase;
    let reconnecting = matches!(voice_phase, VoicePhase::Connecting);
    // VU bar + threshold marker percentages (peak is stored as ×1000).
    let mic_level_pct = (mic_level as f64 / 1000.0 * 100.0).clamp(0.0, 100.0) as u32;
    let threshold_pct = (mic_sensitivity as f64 / 1000.0 * 100.0).clamp(0.0, 100.0) as u32;
    let sensitivity_display = mic_sensitivity / 10;
    // Screen-share preset lives only in local settings — it's a capture-side
    // choice the server never sees.
    let screenshare_quality = settings.read().screenshare_quality.clone();
    let screenshare_hint = crate::features::screenshare::QUALITY_PRESETS
        .iter()
        .find(|(id, _, _)| *id == screenshare_quality)
        .map(|(_, _, hint)| *hint)
        .unwrap_or("");

    rsx! {
        div { class: "border-t border-[var(--border)]",
            if show_banner {
                div { class: "px-3 py-2 border-b border-[var(--border)]",
                    div { class: "flex items-center gap-2",
                        span {
                            class: "w-2.5 h-2.5 rounded-full shrink-0",
                            style: "background:{dot_color};",
                            title: "{phase_text}",
                        }
                        div { class: "flex-1" }
                        // Screen share toggle. Calls getDisplayMedia inside this
                        // click gesture (must not be deferred to an effect).
                        button {
                            class: if sharing {
                                "w-7 h-7 flex items-center justify-center rounded text-[var(--accent)] transition-colors"
                            } else {
                                "w-7 h-7 flex items-center justify-center rounded text-[var(--text-muted)] hover:text-[var(--accent)] transition-colors"
                            },
                            title: if !screen_capture_available {
                                "Screen sharing unavailable in this build (embedded webview lacks screen capture). Install WebView2 or use the browser."
                            } else if sharing { "Stop sharing your screen" } else { "Share your screen" },
                            onclick: move |_| {
                                let now = !sharing;
                                state.write().screen_sharing = now;
                                                            if let Some(cid) = voice_channel {
                                                                g_for_share.send(ClientMessage::SetScreenShare { channel_id: cid, sharing: now });
                                                            }

                                                            if now {
                                                                // Execute the user-gesture prompt + start helper from the
                                                                // screenshare module. This calls requestAndStartShare()
                                                                // which prompts for getDisplayMedia inside the click.
                                                                // If the feature is unavailable the button is disabled and
                                                                // this branch won't run.
                                                                let q = settings.read().screenshare_quality.clone();
                                                                let _ = document::eval(&crate::features::screenshare::share_js(true, &q));
                                                            } else {
                                                                // Turning off — stop immediately.
                                                                let _ = document::eval(&crate::features::screenshare::share_js(false, ""));
                                                            }
                                                        },
                            dangerous_inner_html: crate::features::icons::SCREEN,
                        }
                        button {
                            class: "w-7 h-7 flex items-center justify-center rounded text-[var(--danger)] hover:text-[var(--accent-strong)] transition-colors",
                            title: "Leave voice",
                            onclick: move |_| {
                                g_for_hang.send(ClientMessage::LeaveVoice);
                                v_for_hang.send(VoiceCmd::Disconnect);
                            },
                            dangerous_inner_html: crate::features::icons::PHONE_OFF,
                        }
                    }
                    if let Some(err) = voice_error {
                        div { class: "text-[10px] text-[var(--danger)] mt-1 break-all",
                            "{err}"
                        }
                    }
                }
            }
            div { class: "h-12 px-3 flex items-center gap-2",
                crate::features::profiles::Avatar {
                    pubkey: self_pubkey.clone().unwrap_or_default(),
                    name: name.clone(),
                    size: "w-7 h-7",
                }
                div { class: "flex-1 min-w-0",
                    div { class: "flex items-center gap-1.5",
                        span { class: "text-sm text-[var(--text)] truncate", "{name}" }
                        span { class: "text-[9px] font-semibold text-[var(--text-dim)] shrink-0", "Lv {level}" }
                    }
                    // Thin XP progress bar (comp 2).
                    div { class: "mt-0.5 h-1 rounded-full overflow-hidden", style: "background: var(--bg2);",
                        div {
                            class: "h-full rounded-full",
                            style: "width: {xp_pct}%; background: linear-gradient(90deg, #8fb0ff, var(--accent));",
                        }
                    }
                }
                crate::features::profiles::ProfileEditor {}
                crate::features::appearance::AppearanceButton {}

                // Audio settings button — opens a small popover to pick input/output.
                button {
                    class: "w-7 h-7 flex items-center justify-center rounded text-[var(--text-muted)] hover:text-[var(--accent)] transition-colors",
                    title: "Audio settings",
                    onclick: move |_| {
                        let now = !show_audio_settings();
                        show_audio_settings.set(now);
                        // Ask voice service to refresh device lists when opening.
                        if now {
                            let v = v_for_audio_button.clone();
                            v.send(crate::features::voice::VoiceCmd::ListDevices);
                        }
                    },
                    dangerous_inner_html: crate::features::icons::GEAR,
                }

                // Popover (render when signal true). The popover is a freely
                // draggable floating window: while a drag is in flight a full
                // viewport overlay captures pointer move/up so the cursor can
                // leave the small header without dropping the drag (same model
                // as the activity / screen-share windows). A transparent
                // dismiss layer (z-30) under the popover closes it on outside
                // click without interfering with the drag overlay (z-50).
                if show_audio_settings() {
                    div {
                        class: "fixed inset-0 z-30",
                        onclick: move |_| show_audio_settings.set(false),
                    }
                    if audio_drag().is_some() {
                        div {
                            class: "fixed inset-0 z-50",
                            onmousemove: move |e| {
                                let c = e.client_coordinates();
                                if let Some(AudioDrag::Move { dx, dy }) = audio_drag() {
                                    audio_x.set(c.x - dx);
                                    audio_y.set(c.y - dy);
                                }
                            },
                            onmouseup: move |_| audio_drag.set(None),
                        }
                    }
                    div {
                        class: "fixed z-40 flex flex-col bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg shadow-lg overflow-hidden w-64",
                        style: "left: {audio_x}px; top: {audio_y}px;",
                        // Stop propagation so clicks inside the popover (selects,
                        // close button, drag handle) don't bubble up to the
                        // dismiss overlay and close the window prematurely.
                        onclick: move |e| e.stop_propagation(),

                        // Drag handle header: grab anywhere on the bar to move
                        // the window. The close button lives here now (the old
                        // footer Close is gone), mirroring ActivityWindow.
                        div {
                            class: "h-8 px-2 flex items-center gap-2 border-b border-[var(--border)] shrink-0 cursor-move select-none",
                            onmousedown: move |e| {
                                let c = e.client_coordinates();
                                audio_drag.set(Some(AudioDrag::Move { dx: c.x - audio_x(), dy: c.y - audio_y() }));
                            },
                            span { class: "text-[11px] font-medium text-[var(--text)] flex-1", "Audio settings" }
                            button {
                                class: "text-[var(--text-dim)] hover:text-[var(--text)] text-base leading-none",
                                onmousedown: move |e| e.stop_propagation(),
                                onclick: move |_| show_audio_settings.set(false),
                                "✕"
                            }
                        }

                        div { class: "p-2",
                            // Reconnection indicator
                            if reconnecting {
                                div { class: "mb-2 flex items-center text-[12px] text-[var(--text-muted)]",
                                    span { class: "dx-spinner" }
                                    span { "Reconnecting audio…" }
                                }
                            }

                            // Input device select
                            div { class: "mb-2",
                                span { class: "text-[11px] text-[var(--text-muted)]", "Input" }
                                select {
                                    class: "w-full mt-1 bg-[var(--panel-solid)] text-[var(--text)] border border-[var(--border)] rounded px-2 py-1 text-sm disabled:opacity-60",
                                    style: "color: var(--text); background: var(--panel-solid);",
                                    disabled: "{reconnecting}",
                                    onchange: move |e| {
                                        let val = e.value();
                                        // update AppState and ask voice service to persist selection
                                        let val_cloned = val.clone();
                                        // Persist to client settings
                                        let mut next = settings.read().clone();
                                        if val_cloned.is_empty() { next.selected_input_device = None; } else { next.selected_input_device = Some(val_cloned.clone()); }
                                        settings.set(next.clone());
                                        crate::settings::save(&next);

                                        let mut s = state.write();
                                        s.selected_input_device = if val_cloned.is_empty() { None } else { Some(val_cloned.clone()) };
                                        let v = v_for_input_change.clone();
                                        let _ = v.send(crate::features::voice::VoiceCmd::SetDevices { input: s.selected_input_device.clone(), output: None });
                                    },
                                    option { value: "", "System default" }
                                    for dev in available_input_devices.iter() {
                                        option { selected: selected_input_device.as_ref().map(|n| n == dev).unwrap_or(false), value: "{dev}", "{dev}" }
                                    }
                                }
                            }
                            // VU bar — live mic level with threshold marker.
                            // Only meaningful while a voice session is capturing;
                            // otherwise show a hint so the user knows why it's flat.
                            div { class: "mb-2",
                                div { class: "flex items-center justify-between",
                                    span { class: "text-[11px] text-[var(--text-muted)]", "Level" }
                                    if reconnecting {
                                        span { class: "text-[10px] text-[var(--text-dim)]", "reconnecting" }
                                    } else if voice_phase == VoicePhase::Connected {
                                        span { class: "text-[10px] text-[var(--text-dim)]", "{mic_level_pct}%" }
                                    } else {
                                        span { class: "text-[10px] text-[var(--text-dim)]", "join voice" }
                                    }
                                }
                                if voice_phase == VoicePhase::Connected && !reconnecting {
                                    div {
                                        class: "relative w-full h-2 mt-1 rounded-full overflow-hidden",
                                        style: "background: var(--bg2);",
                                        div {
                                            class: "h-full rounded-full transition-all duration-75",
                                            style: "width: {mic_level_pct}%; background: linear-gradient(90deg, var(--up), var(--accent), var(--danger));",
                                        }
                                        // Threshold marker — white vertical line at the
                                        // sensitivity position. When the fill passes it,
                                        // the user is detected as speaking.
                                        div {
                                            class: "absolute top-0 bottom-0 w-0.5 bg-white/70 pointer-events-none",
                                            style: "left: {threshold_pct}%;",
                                        }
                                    }
                                } else {
                                    div {
                                        class: "w-full h-2 mt-1 rounded-full",
                                        style: "background: var(--bg2);",
                                    }
                                }
                            }
                            // Mic sensitivity slider — adjusts the speaking-detection
                            // threshold (1..=1000, matching the peak's ×1000 scale).
                            // Lower = more sensitive (picks up quiet speech);
                            // higher = less sensitive (ignores background noise).
                            // Displayed as a percentage for intuitivity.
                            div { class: "mb-2",
                                div { class: "flex items-center justify-between",
                                    span { class: "text-[11px] text-[var(--text-muted)]", "Sensitivity" }
                                    span { class: "text-[10px] text-[var(--text-dim)]", "{sensitivity_display}%" }
                                }
                                input {
                                    r#type: "range",
                                    min: "1",
                                    max: "1000",
                                    value: "{mic_sensitivity}",
                                    class: "w-full mt-1 accent-[var(--accent)]",
                                    oninput: move |e| {
                                        let val: u32 = e.value().parse().unwrap_or(25).clamp(1, 1000);
                                        // Persist to client settings
                                        let mut next = settings.read().clone();
                                        next.mic_sensitivity = val;
                                        settings.set(next.clone());
                                        crate::settings::save(&next);
                                        // Update AppState + tell voice service (takes effect
                                        // on the next 150ms speaking-detection tick).
                                        state.write().mic_sensitivity = val;
                                        let v = v_for_sensitivity.clone();
                                        let _ = v.send(crate::features::voice::VoiceCmd::SetSensitivity { threshold: val });
                                    },
                                }
                            }
                            // Output device select
                            div { class: "mb-2",
                                span { class: "text-[11px] text-[var(--text-muted)]", "Output" }
                                select {
                                    class: "w-full mt-1 bg-[var(--panel-solid)] text-[var(--text)] border border-[var(--border)] rounded px-2 py-1 text-sm disabled:opacity-60",
                                    style: "color: var(--text); background: var(--panel-solid);",
                                    disabled: "{reconnecting}",
                                    onchange: move |e| {
                                        let val = e.value();
                                        let val_cloned = val.clone();
                                        // Persist to client settings
                                        let mut next = settings.read().clone();
                                        if val_cloned.is_empty() { next.selected_output_device = None; } else { next.selected_output_device = Some(val_cloned.clone()); }
                                        settings.set(next.clone());
                                        crate::settings::save(&next);

                                        let mut s = state.write();
                                        s.selected_output_device = if val_cloned.is_empty() { None } else { Some(val_cloned.clone()) };
                                        let v = v_for_output_change.clone();
                                        let _ = v.send(crate::features::voice::VoiceCmd::SetDevices { input: None, output: s.selected_output_device.clone() });
                                    },
                                    option { value: "", "System default" }
                                    for dev in available_output_devices.iter() {
                                        option { selected: selected_output_device.as_ref().map(|n| n == dev).unwrap_or(false), value: "{dev}", "{dev}" }
                                    }
                                }
                            }
                            // Screen-share quality. Applies to the next share —
                            // the encoding is fixed when the track is published,
                            // so changing it mid-share has no effect.
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
                            }
                        }
                    }
                }

                button {
                    class: if muted {
                        "w-7 h-7 flex items-center justify-center rounded text-[var(--danger)] hover:text-[var(--accent-strong)] transition-colors"
                    } else {
                        "w-7 h-7 flex items-center justify-center rounded text-[var(--text-muted)] hover:text-[var(--accent)] transition-colors"
                    },
                    title: mute_label,
                    onclick: move |_| {
                        let new_muted = !muted;
                        g_for_mute.send(ClientMessage::SetVoiceMute { muted: new_muted, deafened: new_muted });
                        v_for_mute.send(VoiceCmd::SetMute { muted: new_muted });
                    },
                    dangerous_inner_html: if muted { crate::features::icons::MIC_OFF } else { crate::features::icons::MIC },
                }
            }
        }
    }
}

fn select_text_channel(state: &mut Signal<AppState>, gateway: &GatewayTx, channel_id: Id) {
    let needs_fetch = {
        let mut s = state.write();
        s.selected_channel = Some(channel_id);
        !s.messages.contains_key(&channel_id)
    };
    if needs_fetch {
        gateway.send(ClientMessage::FetchMessages {
            channel_id,
            limit: 50,
            before_ms: None,
        });
    }
}

fn select_dm(state: &mut Signal<AppState>, gateway: &GatewayTx, channel_id: Id) {
    let needs_fetch = {
        let mut s = state.write();
        s.dm_mode = true;
        s.selected_channel = Some(channel_id);
        s.dm_unread.remove(&channel_id);
        !s.messages.contains_key(&channel_id)
    };
    if needs_fetch {
        gateway.send(ClientMessage::FetchMessages {
            channel_id,
            limit: 50,
            before_ms: None,
        });
    }
}

