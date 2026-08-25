use dioxus::prelude::*;
use dioxus_grid_layout::NoDrag;

use crate::features::voice::{VoiceCmd, use_voice_tx};
use crate::identity::discriminator;
use crate::protocol::{Channel, ChannelKind, ClientMessage, Id, Permission, VoiceState};
use crate::state::DmInfo;
use crate::state::{AppState, GatewayTx, VoicePhase, use_app_state, use_gateway};

/// `guild` is **every** channel in the guild, in draw order — `position` is
/// assigned 0..n across kinds, so numbering each kind from 0 corrupts it.
fn reorder_positions(guild: &[Channel], moved: Id, target: Id) -> Vec<(Id, u32)> {
    if moved == target {
        return Vec::new();
    }
    let (Some(from), Some(to)) = (
        guild.iter().position(|c| c.id == moved),
        guild.iter().position(|c| c.id == target),
    ) else {
        return Vec::new();
    };
    if std::mem::discriminant(&guild[from].kind) != std::mem::discriminant(&guild[to].kind) {
        return Vec::new();
    }

    let mut order: Vec<&Channel> = guild.iter().collect();
    let dragged = order.remove(from);
    order.insert(to, dragged);

    let (lo, hi) = (from.min(to), from.max(to));
    let mut slots: Vec<u32> = guild[lo..=hi].iter().map(|c| c.position).collect();
    slots.sort_unstable();
    let guard = &guild[lo.saturating_sub(1)..(hi + 2).min(guild.len())];
    let mut window: Vec<u32> = guard.iter().map(|c| c.position).collect();
    window.sort_unstable();
    if window.windows(2).all(|w| w[0] != w[1]) {
        return order[lo..=hi]
            .iter()
            .zip(slots)
            .filter(|(c, slot)| c.position != *slot)
            .map(|(c, slot)| (c.id, slot))
            .collect();
    }

    order
        .iter()
        .enumerate()
        .filter(|(i, c)| c.position != *i as u32)
        .map(|(i, c)| (c.id, i as u32))
        .collect()
}

fn send_reorder(gw: &GatewayTx, group: &[Channel], moved: Id, target: Id) {
    let positions = reorder_positions(group, moved, target);
    if positions.is_empty() {
        return;
    }
    let Some(guild_id) = group.first().map(|c| c.guild_id) else {
        return;
    };
    gw.send(ClientMessage::ReorderChannels {
        guild_id,
        positions,
    });
}

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
    Edit,
    ConfirmDelete,
}

#[derive(Clone, Copy, PartialEq)]
enum AudioDrag {
    Move { dx: f64, dy: f64 },
}

const PANEL: &str = "panel-hover w-full h-full bg-[var(--panel)] border border-[var(--border)] rounded-lg flex flex-col overflow-hidden";
const HEADER: &str = "h-11 px-3 flex items-center border-b border-[var(--border)]";
const SECTION_LABEL: &str =
    "px-2 py-1 text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)]";

#[component]
pub fn ChannelsColumn() -> Element {
    let mut state = use_app_state();
    let gateway = use_gateway();
    let voice = use_voice_tx();

    let snapshot = state.read();
    let dm_mode = snapshot.dm_mode;
    let dms: Vec<DmInfo> = snapshot.dms.clone();
    // Resolved here, not stored: a DM row is drawn before any source of a name
    // has arrived.
    let dm_names: std::collections::HashMap<String, String> = dms
        .iter()
        .map(|d| {
            (
                d.other_pubkey.clone(),
                snapshot.display_name(&d.other_pubkey),
            )
        })
        .collect();
    let selected_guild = snapshot.selected_guild;
    let selected_channel = snapshot.selected_channel;
    let guild =
        selected_guild.and_then(|gid| snapshot.guilds.iter().find(|g| g.id == gid).cloned());
    let channels: Vec<Channel> = selected_guild
        .map(|gid| {
            let mut v: Vec<Channel> = snapshot
                .channels
                .iter()
                .filter(|c| c.guild_id == gid)
                .cloned()
                .collect();
            v.sort_by(|a, b| {
                a.position
                    .cmp(&b.position)
                    .then_with(|| a.name.cmp(&b.name))
            });
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
    let guild_order: Vec<Channel> = text_channels
        .iter()
        .chain(voice_channels.iter())
        .map(|c| (*c).clone())
        .collect();

    let mut chan_menu = use_signal::<Option<ChanMenu>>(|| None);
    let mut show_create = use_signal(|| false);
    let mut dragging = use_signal::<Option<Id>>(|| None);

    let banner = if dm_mode {
        None
    } else {
        guild.as_ref().and_then(|g| g.banner.clone())
    };

    rsx! {
        aside { class: PANEL,
            if let Some(src) = banner {
                div { class: "relative h-20 shrink-0 overflow-hidden border-b border-[var(--border)]",
                    img { class: "w-full h-full object-cover block", src: "{src}", alt: "guild banner" }
                    div {
                        class: "absolute inset-0",
                        style: "background: linear-gradient(180deg, transparent 40%, var(--panel-solid));",
                    }
                }
            }
            div { class: HEADER,
                h2 { class: "text-sm text-[var(--accent)] truncate font-medium flex-1",
                    if dm_mode {
                        "Direct Messages"
                    } else {
                        {guild.as_ref().map(|g| g.name.clone()).unwrap_or_else(|| "No guild".into())}
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
                    StartDmByKey {}
                    if dms.is_empty() {
                        div { class: "px-2 text-xs text-[var(--text-dim)] leading-relaxed",
                            "No conversations yet. Paste someone's npub above, or click a member."
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
                            let uname = dm_names.get(&dm.other_pubkey).cloned().unwrap_or_default();
                            let disc = discriminator(&dm.other_pubkey);
                            rsx! {
                                button {
                                    key: "{cid}",
                                    class: "w-full flex items-center gap-2 px-2 py-1 rounded text-left text-sm transition-colors {cls}",
                                    onclick: move |_| select_dm(&mut state, &g2, cid),
                                    crate::features::profiles::Avatar {
                                        pubkey: dm.other_pubkey.clone(),
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
                                let g_drop = gateway.clone();
                                let drop_group = guild_order.clone();
                                rsx! {
                                    button {
                                        key: "{cid}",
                                        class: "w-full flex items-center gap-1.5 px-2 py-1 rounded text-left text-sm transition-colors {cls}",
                                        draggable: can_manage_channels,
                                        ondragstart: move |_| dragging.set(Some(cid)),
                                        ondragover: move |e: Event<DragData>| {
                                            if dragging().is_some() {
                                                e.prevent_default();
                                            }
                                        },
                                        ondrop: move |e: Event<DragData>| {
                                            e.prevent_default();
                                            let moved = dragging();
                                            dragging.set(None);
                                            if let Some(moved) = moved {
                                                send_reorder(&g_drop, &drop_group, moved, cid);
                                            }
                                        },
                                        ondragend: move |_| dragging.set(None),
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
                                        span { class: "text-[var(--text-dim)]", draggable: false, "#" }
                                        span { class: "truncate flex-1", draggable: false, "{ch.name}" }
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
                                let g_drop = gateway.clone();
                                let drop_group = guild_order.clone();
                                rsx! {
                                    div {
                                        key: "{cid}",
                                        draggable: can_manage_channels,
                                        ondragstart: move |_| dragging.set(Some(cid)),
                                        ondragover: move |e: Event<DragData>| {
                                            if dragging().is_some() {
                                                e.prevent_default();
                                            }
                                        },
                                        ondrop: move |e: Event<DragData>| {
                                            e.prevent_default();
                                            let moved = dragging();
                                            dragging.set(None);
                                            if let Some(moved) = moved {
                                                send_reorder(&g_drop, &drop_group, moved, cid);
                                            }
                                        },
                                        ondragend: move |_| dragging.set(None),
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
                    guild_order: guild_order.clone(),
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

#[component]
fn ChannelMenuPopover(
    menu: ChanMenu,
    guild_order: Vec<Channel>,
    on_close: EventHandler<()>,
    on_mode: EventHandler<ChanMenuMode>,
) -> Element {
    let gateway = use_gateway();
    let ch = menu.channel.clone();
    let siblings: Vec<Channel> = guild_order
        .iter()
        .filter(|c| std::mem::discriminant(&c.kind) == std::mem::discriminant(&ch.kind))
        .cloned()
        .collect();
    let at = siblings.iter().position(|c| c.id == ch.id);
    let move_up = at.filter(|i| *i > 0).map(|i| siblings[i - 1].id);
    let move_down = at
        .filter(|i| *i + 1 < siblings.len())
        .map(|i| siblings[i + 1].id);
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
                            if let Some(above) = move_up {
                                {
                                    let gw = gateway.clone();
                                    let order = guild_order.clone();
                                    let id = ch.id;
                                    rsx! {
                                        button {
                                            class: "w-full text-left px-3 py-1.5 rounded text-[var(--text)] hover:bg-white/[0.04] transition-colors",
                                            onclick: move |_| {
                                                send_reorder(&gw, &order, id, above);
                                                on_close.call(());
                                            },
                                            "Move up"
                                        }
                                    }
                                }
                            }
                            if let Some(below) = move_down {
                                {
                                    let gw = gateway.clone();
                                    let order = guild_order.clone();
                                    let id = ch.id;
                                    rsx! {
                                        button {
                                            class: "w-full text-left px-3 py-1.5 rounded text-[var(--text)] hover:bg-white/[0.04] transition-colors",
                                            onclick: move |_| {
                                                send_reorder(&gw, &order, id, below);
                                                on_close.call(());
                                            },
                                            "Move down"
                                        }
                                    }
                                }
                            }
                            if matches!(ch.kind, ChannelKind::Text) {
                                button {
                                    class: "w-full text-left px-3 py-1.5 rounded text-[var(--text)] hover:bg-white/[0.04] transition-colors",
                                    onclick: move |_| {
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
    let state = use_app_state();
    let users_by_id = state.read();
    let sharers: Vec<String> = users_by_id.screen_sharers_in(channel.id).to_vec();

    rsx! {
        div { class: "rounded",
            button {
                class: "w-full flex items-center gap-1.5 px-2 py-1 rounded text-left text-sm transition-colors {row_cls}",
                onclick: move |_| {
                    if connected { on_leave.call(()) } else { on_join.call(()) }
                },
                span { class: "text-[var(--text-dim)] text-xs", draggable: false, "♪" }
                span { class: "truncate flex-1", draggable: false, "{channel.name}" }
                if connected {
                    span { class: "text-[9px] text-[var(--accent)] font-semibold uppercase tracking-wider", "live" }
                }
            }
            if !occupants.is_empty() {
                div {
                    class: "ml-5 mt-0.5 space-y-0.5",
                    draggable: false,
                    for vs in occupants.iter() {
                        {
                            let name = users_by_id.display_name(&vs.user_pubkey);
                            let is_self = self_pubkey.as_deref() == Some(vs.user_pubkey.as_str());
                            let is_sharing = sharers.iter().any(|p| p == &vs.user_pubkey);
                            rsx! {
                                VoiceOccupant {
                                    key: "{vs.user_pubkey}",
                                    pubkey: vs.user_pubkey.clone(),
                                    name,
                                    speaking: vs.speaking,
                                    remote_muted: vs.muted,
                                    remote_deafened: vs.deafened,
                                    is_self,
                                    is_sharing,
                                    has_camera: vs.camera_on,
                                    can_watch: is_sharing && connected && !is_self,
                                    can_watch_camera: vs.camera_on && connected && !is_self,
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn rate_or_dash(v: Option<u32>, unit: &str) -> String {
    match v {
        Some(n) => format!("{n} {unit}"),
        None => format!("— {unit}"),
    }
}

#[component]
fn ConnectionStats() -> Element {
    let state = use_app_state();
    let voice = use_voice_tx();

    use_hook(|| {
        voice.send(VoiceCmd::SetStatsPolling { enabled: true });
    });
    {
        let voice = voice.clone();
        use_drop(move || {
            voice.send(VoiceCmd::SetStatsPolling { enabled: false });
        });
    }

    let mut rows: Vec<(String, crate::state::TrackStats)> = {
        let s = state.read();
        s.voice_stats
            .iter()
            .map(|(pk, st)| (s.display_name(pk), *st))
            .collect()
    };
    rows.sort_by(|a, b| a.0.cmp(&b.0));

    rsx! {
        div { class: "mt-1 border-t border-[var(--border)] pt-1.5",
            if rows.is_empty() {
                span { class: "text-[10px] text-[var(--text-dim)] block",
                    "Waiting for the first reading — join a voice channel."
                }
            }
            for (name, st) in rows.iter() {
                div { key: "{name}", class: "flex items-baseline gap-1.5 text-[10px] font-mono py-0.5",
                    span { class: "truncate flex-1 text-[var(--text-muted)]", "{name}" }
                    match st {
                        crate::state::TrackStats::Inbound { loss_pct, jitter_ms, buffer_ms, concealment_events } => rsx! {
                            span {
                                class: if *loss_pct >= 1.0 { "text-[var(--warn)]" } else { "text-[var(--text-dim)]" },
                                title: "Packets that never arrived",
                                "{loss_pct:.1}% loss"
                            }
                            span { class: "text-[var(--text-dim)]", title: "Network jitter", "{jitter_ms:.0}ms jit" }
                            span {
                                class: "text-[var(--text-dim)]",
                                title: "Delay the decoder is holding, on top of our own playback buffer",
                                "{buffer_ms:.0}ms buf"
                            }
                            span {
                                class: "text-[var(--text-dim)]",
                                title: "Times loss had to be concealed — loss that became audible",
                                "{concealment_events} conc"
                            }
                        },
                        crate::state::TrackStats::Outbound { bitrate_kbps, packets_per_sec, target_kbps } => rsx! {
                            span {
                                class: "text-[var(--text-dim)]",
                                title: "What the encoder produced, measured between readings (payload only, so the wire is a little higher) — it is aiming for {target_kbps} kbit/s, and redundancy against packet loss roughly doubles what that costs",
                                {rate_or_dash(*bitrate_kbps, "kbit/s out")}
                            }
                            span {
                                class: "text-[var(--text-dim)]",
                                title: "Packets a second — around 50 while you're transmitting, near zero while silence is held back",
                                {rate_or_dash(*packets_per_sec, "pkt/s")}
                            }
                        },
                    }
                }
            }
        }
    }
}

#[component]
fn VoiceOccupant(
    pubkey: String,
    name: String,
    speaking: bool,
    remote_muted: bool,
    remote_deafened: bool,
    is_self: bool,
    is_sharing: bool,
    has_camera: bool,
    can_watch: bool,
    can_watch_camera: bool,
) -> Element {
    let mut state = use_app_state();
    let voice = use_voice_tx();
    let mut show_volume = use_signal(|| false);

    let volume = state
        .read()
        .user_volumes
        .get(&pubkey)
        .copied()
        .unwrap_or(100);
    let locally_muted = state.read().user_muted.contains(&pubkey);
    let health_dot = state
        .read()
        .voice_quality
        .get(&pubkey)
        .copied()
        .and_then(|h| h.dot(is_self));

    let dot = if speaking && !locally_muted {
        "bg-[var(--accent)]"
    } else {
        "bg-[var(--text-dim)]"
    };
    let apply = {
        let pubkey = pubkey.clone();
        let voice = voice.clone();
        move || {
            let gain = state.read().voice_gain_of(&pubkey);
            voice.send(crate::features::voice::VoiceCmd::SetUserVolume {
                pubkey: pubkey.clone(),
                gain,
            });
        }
    };
    let apply_slider = apply.clone();
    let pk_slider = pubkey.clone();
    let pk_mute = pubkey.clone();
    let pk_watch = pubkey.clone();
    let pk_camera = pubkey.clone();
    let is_watching_screen = state.read().screen_viewing.as_deref() == Some(pubkey.as_str());
    let is_watching_camera = state.read().cameras_watching.contains(&pubkey);

    rsx! {
        div { class: "px-2 py-0.5",
            div { class: "flex items-center gap-1.5 text-xs text-[var(--text-muted)]",
                span { class: "w-1.5 h-1.5 rounded-full shrink-0 {dot}" }
                span { class: "truncate flex-1",
                    "{name}"
                    if is_self { " (you)" }
                }
                if remote_deafened {
                    span { class: "text-[9px] text-[var(--text-dim)] uppercase tracking-wider", "deafened" }
                } else if remote_muted {
                    span { class: "text-[9px] text-[var(--text-dim)] uppercase tracking-wider", "muted" }
                }
                if let Some((color, label)) = health_dot {
                    span {
                        class: "w-1.5 h-1.5 rounded-full shrink-0",
                        style: "background:{color};",
                        title: "{label}",
                    }
                }
                if !is_self {
                    button {
                        class: if locally_muted {
                            "w-5 h-5 flex items-center justify-center rounded text-[var(--danger)] shrink-0"
                        } else if volume != 100 {
                            "w-5 h-5 flex items-center justify-center rounded text-[var(--accent)] shrink-0"
                        } else {
                            "w-5 h-5 flex items-center justify-center rounded text-[var(--text-dim)] hover:text-[var(--text)] shrink-0"
                        },
                        title: if locally_muted { "Muted for you — click for volume" } else { "Volume (only affects your playback)" },
                        onclick: move |_| {
                            let now = !show_volume();
                            show_volume.set(now);
                        },
                        dangerous_inner_html: if locally_muted {
                            crate::features::icons::SPEAKER_OFF
                        } else {
                            crate::features::icons::SPEAKER
                        },
                    }
                }
                if is_sharing {
                    button {
                        class: if is_watching_screen {
                            "w-5 h-5 flex items-center justify-center rounded text-[var(--danger)] bg-[var(--danger)]/20 shrink-0 disabled:opacity-70 disabled:cursor-default"
                        } else {
                            "w-5 h-5 flex items-center justify-center rounded text-[var(--danger)] hover:bg-[var(--danger)]/10 shrink-0 disabled:opacity-70 disabled:cursor-default"
                        },
                        disabled: !can_watch,
                        title: if !can_watch {
                            "Sharing their screen"
                        } else if is_watching_screen {
                            "Stop watching their screen"
                        } else {
                            "Watch their screen"
                        },
                        onclick: move |_| {
                            if can_watch {
                                let mut s = state.write();
                                s.screen_viewing = if s.screen_viewing.as_deref() == Some(pk_watch.as_str()) {
                                    None
                                } else {
                                    Some(pk_watch.clone())
                                };
                            }
                        },
                        dangerous_inner_html: crate::features::icons::SCREEN,
                    }
                }
                if has_camera && !is_self {
                    button {
                        class: if is_watching_camera {
                            "w-5 h-5 flex items-center justify-center rounded text-[var(--danger)] bg-[var(--danger)]/20 shrink-0 disabled:opacity-70 disabled:cursor-default"
                        } else {
                            "w-5 h-5 flex items-center justify-center rounded text-[var(--danger)] hover:bg-[var(--danger)]/10 shrink-0 disabled:opacity-70 disabled:cursor-default"
                        },
                        disabled: !can_watch_camera,
                        title: if !can_watch_camera {
                            "Their camera is on — join the channel to watch"
                        } else if is_watching_camera {
                            "Hide their camera"
                        } else {
                            "Watch their camera"
                        },
                        onclick: move |_| {
                            if !can_watch_camera {
                                return;
                            }
                            let mut s = state.write();
                            if !s.cameras_watching.remove(&pk_camera) {
                                s.cameras_watching.insert(pk_camera.clone());
                            }
                        },
                        dangerous_inner_html: crate::features::icons::CAMERA,
                    }
                }
            }
            if show_volume() && !is_self {
                div { class: "flex items-center gap-1.5 mt-1 mb-0.5",
                    button {
                        class: if locally_muted {
                            "text-[9px] uppercase tracking-wider text-[var(--danger)] font-semibold shrink-0"
                        } else {
                            "text-[9px] uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--text)] shrink-0"
                        },
                        title: "Mute this person for you only",
                        onclick: move |_| {
                            let now = !locally_muted;
                            {
                                let mut s = state.write();
                                if now { s.user_muted.insert(pk_mute.clone()); } else { s.user_muted.remove(&pk_mute); }
                            }
                            apply();
                        },
                        if locally_muted { "unmute" } else { "mute" }
                    }
                    input {
                        r#type: "range",
                        min: "0",
                        max: "200",
                        value: "{volume}",
                        disabled: locally_muted,
                        class: "flex-1 accent-[var(--accent)] disabled:opacity-40",
                        oninput: move |e| {
                            let val: u32 = e.value().parse().unwrap_or(100).clamp(0, 200);
                            state.write().user_volumes.insert(pk_slider.clone(), val);
                            apply_slider();
                        },
                    }
                    span { class: "text-[9px] text-[var(--text-dim)] w-8 text-right shrink-0", "{volume}%" }
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
    let persist_settings = move |_: FormEvent| crate::settings::save(&settings.read());
    let screen_capture_available = state.read().screen_capture_available;
    let native_capture = crate::sysvideo::supported();
    let self_pubkey = state.read().self_user.as_ref().map(|u| u.pubkey.clone());
    let sharing = state.read().screen_sharing;
    let camera_on = state.read().camera_on;
    let camera_starting = state.read().camera_starting;
    let camera_capture_available = state.read().camera_capture_available;
    let name = self_username.clone().unwrap_or_else(|| "—".into());
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
    let (dot_color, phase_text) = match self_voice.phase {
        VoicePhase::Idle => ("var(--text-dim)", "voice idle"),
        VoicePhase::Connecting => ("var(--warn)", "connecting…"),
        VoicePhase::Connected => ("var(--success)", "voice connected"),
        VoicePhase::Error => ("var(--danger)", "voice error"),
    };
    let voice_error = self_voice.error.clone();

    let muted = self_voice.muted;
    let deafened = self_voice.deafened;
    let mute_label = if muted { "unmute" } else { "mute" };
    let g_for_mute = gateway.clone();
    let v_for_mute = voice.clone();
    let g_for_deafen = gateway.clone();
    let v_for_deafen = voice.clone();
    let g_for_hang = gateway.clone();
    let v_for_hang = voice.clone();
    let g_for_share = gateway.clone();
    let voice_channel = self_voice.channel_id;

    let mut show_audio_settings = use_signal(|| false);
    let mut show_stats = use_signal(|| false);
    let mut audio_x = use_signal(|| 880.0_f64);
    let mut audio_y = use_signal(|| 48.0_f64);
    let mut audio_drag = use_signal(|| None::<AudioDrag>);

    let available_input_devices = state.read().available_input_devices.clone();
    let available_output_devices = state.read().available_output_devices.clone();
    let selected_input_device = state.read().selected_input_device.clone();
    let selected_output_device = state.read().selected_output_device.clone();
    let available_cameras = state.read().available_cameras.clone();
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
    let gate_open = self_voice.speaking;

    let v_for_audio_button = voice.clone();
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
    let show_pre = noise_cancellation && mic_level_pre > mic_level;
    let threshold_pct = crate::features::voice::peak_to_meter_pct(mic_sensitivity);
    let sensitivity_display = crate::features::voice::peak_to_db_label(mic_sensitivity);
    let mic_level_display = crate::features::voice::peak_to_db_label(mic_level);
    let screenshare_quality = settings.read().screenshare_quality.clone();
    let screenshare_audio = settings.read().screenshare_audio;
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
                        button {
                            class: if camera_on {
                                "w-7 h-7 flex items-center justify-center rounded text-[var(--accent)] transition-colors"
                            } else {
                                "w-7 h-7 flex items-center justify-center rounded text-[var(--text-muted)] hover:text-[var(--accent)] transition-colors"
                            },
                            disabled: !camera_capture_available,
                            title: if !camera_capture_available {
                                "This build's webview has no camera support."
                            } else if camera_starting {
                                "Starting your camera…"
                            } else if camera_on { "Turn your camera off" } else { "Turn your camera on" },
                            onclick: move |_| {
                                crate::features::camera::toggle_camera(state, settings, !camera_on);
                            },
                            dangerous_inner_html: if camera_on {
                                crate::features::icons::CAMERA
                            } else {
                                crate::features::icons::CAMERA_OFF
                            },
                        }
                        button {
                            class: if sharing {
                                "w-7 h-7 flex items-center justify-center rounded text-[var(--accent)] transition-colors"
                            } else {
                                "w-7 h-7 flex items-center justify-center rounded text-[var(--text-muted)] hover:text-[var(--accent)] transition-colors"
                            },
                            title: if !screen_capture_available {
                                "Screen capture isn't available in this webview — pressing this will explain why."
                            } else if sharing { "Stop sharing your screen" } else { "Share your screen" },
                            onclick: move |_| {
                                let now = !sharing;
                                if !now {
                                    state.write().screen_sharing = false;
                                    if let Some(cid) = voice_channel {
                                        g_for_share.send(ClientMessage::SetScreenShare { channel_id: cid, sharing: false });
                                    }
                                }

                                                            let q = settings.read().screenshare_quality.clone();
                                                            let a = settings.read().screenshare_audio;
                                                            if native_capture {
                                                                if now && state.peek().screen_video_token.is_none() {
                                                                    state.write().error_toast = Some(
                                                                        "This server is too old to accept a natively \
                                                                         captured screen share.".into()
                                                                    );
                                                                } else if now {
                                                                    crate::features::screenshare::open_screen_picker(state);
                                                                } else {
                                                                    let mut s = state.write();
                                                                    s.screen_native_audio = false;
                                                                    s.screen_share_target = None;
                                                                }
                                                            } else if now {
                                                                let _ = document::eval(&crate::features::screenshare::share_js(true, &q, a));
                                                            } else {
                                                                let _ = document::eval(&crate::features::screenshare::share_js(false, "", true));
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
                    div { class: "mt-0.5 h-1 rounded-full overflow-hidden", style: "background: var(--bg2);",
                        div {
                            class: "h-full rounded-full",
                            style: "width: {xp_pct}%; background: linear-gradient(90deg, #8fb0ff, var(--accent));",
                        }
                    }
                }
                crate::features::profiles::ProfileEditor {}
                crate::features::appearance::AppearanceButton {}

                button {
                    class: "w-7 h-7 flex items-center justify-center rounded text-[var(--text-muted)] hover:text-[var(--accent)] transition-colors",
                    title: "Settings",
                    onclick: move |_| {
                        let now = !show_audio_settings();
                        show_audio_settings.set(now);
                        if now {
                            let v = v_for_audio_button.clone();
                            v.send(crate::features::voice::VoiceCmd::ListDevices);
                            let _ = document::eval(&crate::features::camera::list_cameras_js());
                        }
                    },
                    dangerous_inner_html: crate::features::icons::GEAR,
                }

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
                        class: "fixed z-40 flex flex-col bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg shadow-lg overflow-hidden w-96",
                        style: "left: {audio_x}px; top: {audio_y}px; max-height: calc(100vh - {audio_y}px - 12px);",
                        onclick: move |e| e.stop_propagation(),

                        div {
                            class: "h-8 px-2 flex items-center gap-2 border-b border-[var(--border)] shrink-0 cursor-move select-none",
                            onmousedown: move |e| {
                                let c = e.client_coordinates();
                                audio_drag.set(Some(AudioDrag::Move { dx: c.x - audio_x(), dy: c.y - audio_y() }));
                            },
                            span { class: "text-[11px] font-medium text-[var(--text)] flex-1", "Settings" }
                            button {
                                class: "text-[var(--text-dim)] hover:text-[var(--text)] text-base leading-none",
                                onmousedown: move |e| e.stop_propagation(),
                                onclick: move |_| show_audio_settings.set(false),
                                "✕"
                            }
                        }

                        div { class: "p-2 flex-1 overflow-y-auto",
                            if reconnecting {
                                div { class: "mb-2 flex items-center text-[12px] text-[var(--text-muted)]",
                                    span { class: "dx-spinner" }
                                    span { "Reconnecting audio…" }
                                }
                            }

                            div { class: "mt-3 mb-1.5 pb-1 border-b border-[var(--border)]",
                                span { class: "text-[10px] font-semibold uppercase tracking-wide text-[var(--text)]", "Audio" }
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
                            div { class: "mt-3 mb-1.5 pb-1 border-b border-[var(--border)]",
                                span { class: "text-[10px] font-semibold uppercase tracking-wide text-[var(--text)]", "Microphone" }
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
                                        if show_pre {
                                            div {
                                                class: "absolute inset-y-0 left-0 rounded-full transition-all duration-75",
                                                style: "width: {mic_level_pre_pct}%; background: var(--text-dim); opacity: 0.35;",
                                            }
                                        }
                                        div {
                                            class: "absolute inset-y-0 left-0 rounded-full transition-all duration-75",
                                            style: "width: {mic_level_pct}%; background: linear-gradient(90deg, var(--up), var(--accent), var(--danger));",
                                        }
                                        div {
                                            class: "absolute top-0 bottom-0 w-0.5 bg-white/70 pointer-events-none",
                                            style: "left: {threshold_pct}%;",
                                        }
                                    }
                                    if show_pre {
                                        span { class: "text-[10px] text-[var(--text-dim)] mt-0.5 block",
                                            "Faint bar: before noise cancellation. The gap is what it removed."
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
                                    class: "w-full mt-1 accent-[var(--accent)]",
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
                                if auto_gain_control && mic_volume != 100 {
                                    span { class: "text-[10px] text-[var(--text-dim)] mt-0.5 block",
                                        "Auto gain is on and will pull this back — turn it off below to keep this level."
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
                            div { class: "mt-3 mb-1.5 pb-1 border-b border-[var(--border)]",
                                span { class: "text-[10px] font-semibold uppercase tracking-wide text-[var(--text)]", "Transmission" }
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
                            div { class: "mt-3 mb-1.5 pb-1 border-b border-[var(--border)]",
                                span { class: "text-[10px] font-semibold uppercase tracking-wide text-[var(--text)]", "Video" }
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
                                    ConnectionStats {}
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
                        g_for_mute.send(ClientMessage::SetVoiceMute { muted: new_muted, deafened });
                        v_for_mute.send(VoiceCmd::SetMute { muted: new_muted });
                    },
                    dangerous_inner_html: if muted { crate::features::icons::MIC_OFF } else { crate::features::icons::MIC },
                }

                button {
                    class: if deafened {
                        "w-7 h-7 flex items-center justify-center rounded text-[var(--danger)] hover:text-[var(--accent-strong)] transition-colors"
                    } else {
                        "w-7 h-7 flex items-center justify-center rounded text-[var(--text-muted)] hover:text-[var(--accent)] transition-colors"
                    },
                    title: if deafened { "undeafen" } else { "deafen" },
                    onclick: move |_| {
                        let (next_muted, next_deafened) = state.write().voice.toggle_deafen();
                        g_for_deafen.send(ClientMessage::SetVoiceMute { muted: next_muted, deafened: next_deafened });
                        v_for_deafen.send(VoiceCmd::SetDeafen { deafened: next_deafened });
                        v_for_deafen.send(VoiceCmd::SetMute { muted: next_muted });
                    },
                    dangerous_inner_html: if deafened {
                        crate::features::icons::HEADPHONES_OFF
                    } else {
                        crate::features::icons::HEADPHONES
                    },
                }
            }
        }
    }
}

pub(crate) fn select_text_channel(
    state: &mut Signal<AppState>,
    gateway: &GatewayTx,
    channel_id: Id,
) {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn chan(name: &str, position: u32) -> Channel {
        kinded(name, position, ChannelKind::Text)
    }

    fn kinded(name: &str, position: u32, kind: ChannelKind) -> Channel {
        Channel {
            id: Id::new_v4(),
            guild_id: Id::nil(),
            name: name.into(),
            kind,
            topic: None,
            read_only: false,
            position,
            slowmode_secs: 0,
        }
    }

    fn group(names: &[(&str, u32)]) -> Vec<Channel> {
        let mut v: Vec<Channel> = names.iter().map(|(n, p)| chan(n, *p)).collect();
        v.sort_by(|a, b| {
            a.position
                .cmp(&b.position)
                .then_with(|| a.name.cmp(&b.name))
        });
        v
    }

    fn applied(group: &[Channel], updates: &[(Id, u32)]) -> Vec<String> {
        let mut after: Vec<Channel> = group.to_vec();
        for (id, pos) in updates {
            if let Some(c) = after.iter_mut().find(|c| c.id == *id) {
                c.position = *pos;
            }
        }
        after.sort_by(|a, b| {
            a.position
                .cmp(&b.position)
                .then_with(|| a.name.cmp(&b.name))
        });
        after.into_iter().map(|c| c.name).collect()
    }

    #[test]
    fn dragging_down_lands_on_the_target_slot() {
        let g = group(&[("a", 0), ("b", 1), ("c", 2), ("d", 3)]);
        let updates = reorder_positions(&g, g[0].id, g[2].id);
        assert_eq!(applied(&g, &updates), ["b", "c", "a", "d"]);
    }

    #[test]
    fn dragging_up_lands_on_the_target_slot() {
        let g = group(&[("a", 0), ("b", 1), ("c", 2), ("d", 3)]);
        let updates = reorder_positions(&g, g[3].id, g[1].id);
        assert_eq!(applied(&g, &updates), ["a", "d", "b", "c"]);
    }

    #[test]
    fn a_group_that_never_had_positions_gets_them() {
        let g = group(&[("alpha", 0), ("beta", 0), ("gamma", 0)]);
        let updates = reorder_positions(&g, g[2].id, g[0].id);
        assert_eq!(applied(&g, &updates), ["gamma", "alpha", "beta"]);
        assert_eq!(
            updates.len(),
            2,
            "gamma is already at 0 and needs no update"
        );
    }

    #[test]
    fn only_the_rows_that_move_are_sent() {
        let g = group(&[("a", 0), ("b", 1), ("c", 2), ("d", 3), ("e", 4)]);
        let updates = reorder_positions(&g, g[0].id, g[1].id);
        assert_eq!(updates.len(), 2);
        assert_eq!(applied(&g, &updates), ["b", "a", "c", "d", "e"]);
    }

    #[test]
    fn a_mixed_guild_numbers_across_both_kinds() {
        let guild = vec![
            kinded("general", 0, ChannelKind::Text),
            kinded("random", 1, ChannelKind::Text),
            kinded("Lobby", 2, ChannelKind::Voice),
            kinded("Gaming", 3, ChannelKind::Voice),
        ];
        let updates = reorder_positions(&guild, guild[3].id, guild[2].id);
        assert_eq!(
            updates,
            vec![(guild[3].id, 2), (guild[2].id, 3)],
            "moving one voice channel past the other must touch those two and              nothing else — the text channels above them do not move"
        );
    }

    #[test]
    fn a_drop_across_kinds_does_nothing() {
        let guild = vec![
            kinded("general", 0, ChannelKind::Text),
            kinded("Lobby", 1, ChannelKind::Voice),
        ];
        assert!(reorder_positions(&guild, guild[0].id, guild[1].id).is_empty());
        assert!(reorder_positions(&guild, guild[1].id, guild[0].id).is_empty());
    }

    #[test]
    fn a_one_slot_move_costs_two_messages_however_big_the_guild() {
        let guild: Vec<Channel> = (0..40u32)
            .map(|i| chan(&format!("c{i:02}"), i * 3 + 5))
            .collect();
        let updates = reorder_positions(&guild, guild[10].id, guild[11].id);
        assert_eq!(
            updates,
            vec![(guild[11].id, 35), (guild[10].id, 38)],
            "two rows swap the two position values they already held; the other              38 channels are not renumbered and cost no message"
        );
    }

    #[test]
    fn the_span_is_what_costs_messages() {
        let guild: Vec<Channel> = (0..40u32)
            .map(|i| chan(&format!("c{i:02}"), i * 3 + 5))
            .collect();
        let updates = reorder_positions(&guild, guild[5].id, guild[9].id);
        assert_eq!(
            updates.len(),
            5,
            "five rows lie in the span, and only those"
        );
        let moved: Vec<Id> = updates.iter().map(|(id, _)| *id).collect();
        assert!(
            !moved.contains(&guild[0].id) && !moved.contains(&guild[39].id),
            "nothing outside the span may be touched"
        );
    }

    #[test]
    fn a_tie_just_outside_the_span_cannot_swallow_the_dragged_row() {
        let guild = vec![chan("a", 1), chan("zzz", 2), chan("c", 3), chan("d", 3)];
        let updates = reorder_positions(&guild, guild[1].id, guild[2].id);
        assert_eq!(
            applied(&guild, &updates),
            ["a", "c", "zzz", "d"],
            "the dragged row must land where it was dropped, not behind a row              it happens to tie with"
        );
    }

    #[test]
    fn dropping_a_row_on_itself_sends_nothing() {
        let g = group(&[("a", 0), ("b", 1)]);
        assert!(reorder_positions(&g, g[0].id, g[0].id).is_empty());
    }

    #[test]
    fn dropping_onto_something_that_left_sends_nothing() {
        let g = group(&[("a", 0), ("b", 1)]);
        let gone = Id::new_v4();
        assert!(reorder_positions(&g, g[0].id, gone).is_empty());
        assert!(reorder_positions(&g, gone, g[0].id).is_empty());
    }
}

#[component]
fn StartDmByKey() -> Element {
    let nostr = use_context::<crate::nostr::service::NostrTx>();
    let mut input = use_signal(String::new);
    let mut error = use_signal(|| Option::<String>::None);

    let mut start = move || {
        let raw = input().trim().to_string();
        if raw.is_empty() {
            return;
        }
        match crate::identity::pubkey_from_input(&raw) {
            Ok(pubkey) => {
                error.set(None);
                input.set(String::new());
                nostr.send(crate::nostr::service::NostrCmd::Open { peer: pubkey });
            }
            Err(e) => error.set(Some(e)),
        }
    };

    rsx! {
        div { class: "px-1 pb-2 space-y-1",
            input {
                class: "w-full bg-[var(--bg2)] border border-[var(--border)] rounded px-2 py-1 text-xs outline-none focus:border-[var(--accent)]",
                r#type: "text",
                placeholder: "npub1… or hex key",
                value: "{input}",
                oninput: move |e| { input.set(e.value()); error.set(None); },
                onkeydown: move |e| {
                    if e.key() == Key::Enter { start(); }
                },
            }
            if let Some(err) = error() {
                div { class: "px-1 text-[10px] text-[var(--danger,#f87171)]", "{err}" }
            }
        }
    }
}
