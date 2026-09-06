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

const PANEL: &str = "panel-hover w-full h-full bg-[var(--panel)] border border-[var(--border)] rounded-xl flex flex-col overflow-hidden";
const HEADER: &str = "h-12 px-3.5 flex items-center gap-2 border-b border-[var(--border)]";
const SECTION_LABEL: &str =
    "px-2 pt-3 pb-1.5 font-mono text-[9px] uppercase tracking-[0.18em] text-[var(--text-dim)]";

#[component]
pub fn ChannelsColumn() -> Element {
    let mut state = use_app_state();
    let gateway = use_gateway();
    let voice = use_voice_tx();

    let snapshot = state.read();
    let dm_mode = snapshot.dm_mode;
    let dms: Vec<DmInfo> = snapshot.dms.clone();
    // The effective state, not the channel's own flag: a guild muted as a whole
    // is silent, and a row that did not say so would be lying about what the
    // person will hear.
    let muted_dms: std::collections::HashSet<Id> = dms
        .iter()
        .filter(|d| snapshot.channel_muted(d.channel_id))
        .map(|d| d.channel_id)
        .collect();
    let muted_channels: std::collections::HashSet<Id> = snapshot
        .channels
        .iter()
        .filter(|c| snapshot.is_muted(c.id))
        .map(|c| c.id)
        .collect();
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
    let guild_banner = guild
        .as_ref()
        .and_then(|g| g.banner.as_deref())
        .and_then(|b| snapshot.media_src(b))
        .map(str::to_string);
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
    let settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let mut dm_confirming = use_signal::<Option<Id>>(|| None);
    let mut show_create = use_signal(|| false);
    let mut dragging = use_signal::<Option<Id>>(|| None);

    let banner = if dm_mode { None } else { guild_banner };

    rsx! {
        aside { class: PANEL,
            if let Some(src) = banner {
                div { class: "relative h-28 shrink-0 overflow-hidden border-b border-[var(--border)]",
                    img { class: "w-full h-full object-cover block", src: "{src}", alt: "guild banner" }
                    div {
                        class: "absolute inset-0",
                        style: "background: linear-gradient(180deg, transparent 40%, var(--panel-solid));",
                    }
                }
            }
            div { class: HEADER,
                h2 { class: "dxf-display text-[16px] font-bold tracking-tight text-[var(--text)] truncate flex-1",
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
                            let peer = dm.other_pubkey.clone();
                            let muted = muted_dms.contains(&cid);
                            let asking = dm_confirming() == Some(cid);
                            let del_cls = if asking {
                                "px-1.5 h-4 rounded-full border border-[var(--danger)] text-[9px] font-bold uppercase tracking-wide text-[var(--danger)] flex items-center"
                            } else {
                                "w-4 h-4 rounded-full hidden group-hover:flex items-center justify-center text-[10px] text-[var(--text-dim)] hover:text-[var(--danger)]"
                            };
                            rsx! {
                                div {
                                    key: "{cid}",
                                    class: "group w-full flex items-center gap-2 px-2 py-1 rounded text-sm transition-colors {cls}",
                                    button {
                                        class: "flex-1 min-w-0 flex items-center gap-2 text-left",
                                        onclick: move |_| {
                                            dm_confirming.set(None);
                                            select_dm(&mut state, &g2, cid);
                                        },
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
                                    if muted && !asking {
                                        span {
                                            class: "shrink-0 text-[9px] text-[var(--text-dim)] group-hover:hidden",
                                            title: "Muted",
                                            "🔕"
                                        }
                                    }
                                    if !asking {
                                        button {
                                            class: "shrink-0 w-4 h-4 rounded-full hidden group-hover:flex items-center justify-center text-[10px] text-[var(--text-dim)] hover:text-[var(--text)] transition-colors",
                                            title: if muted { "Unmute this conversation" } else { "Mute this conversation" },
                                            onclick: move |_| crate::state::set_dm_muted(state, settings, cid, !muted),
                                            if muted { "🔔" } else { "🔕" }
                                        }
                                    }
                                    button {
                                        class: "shrink-0 justify-center transition-colors {del_cls}",
                                        title: "Delete this conversation on this machine. The relays keep their copy, and a new message reopens it.",
                                        onclick: move |_| {
                                            if !asking {
                                                dm_confirming.set(Some(cid));
                                                return;
                                            }
                                            crate::state::forget_dm(state, settings, &peer);
                                            dm_confirming.set(None);
                                        },
                                        if asking { "Del?" } else { "✕" }
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
                        div { class: SECTION_LABEL, "Text" }
                        for channel in text_channels.iter() {
                            {
                                let ch = (*channel).clone();
                                let cid = ch.id;
                                let active = selected_channel == Some(cid);
                                let cls = if active {
                                    "text-[var(--text)] font-medium bg-[var(--panel2)]"
                                } else {
                                    "text-[var(--text-dim)] hover:text-[var(--text-muted)] hover:bg-white/[0.03]"
                                };
                                let g2 = gateway.clone();
                                let ctx_ch = ch.clone();
                                let g_drop = gateway.clone();
                                let drop_group = guild_order.clone();
                                rsx! {
                                    button {
                                        key: "{cid}",
                                        class: "relative w-full h-8 flex items-center gap-2 px-2.5 rounded-lg text-left text-[13.5px] transition-colors {cls}",
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
                                            e.prevent_default();
                                            let c = e.client_coordinates();
                                            chan_menu.set(Some(ChanMenu {
                                                channel: ctx_ch.clone(),
                                                x: c.x,
                                                y: c.y,
                                                mode: ChanMenuMode::Menu,
                                            }));
                                        },
                                        if active {
                                            span {
                                                class: "absolute",
                                                style: "left:-8px; top:50%; transform:translateY(-50%); width:3px; height:18px; border-radius:0 3px 3px 0; background: var(--accent);",
                                            }
                                        }
                                        span { class: "font-mono text-[13px] opacity-70", draggable: false, "#" }
                                        span { class: "truncate flex-1", draggable: false, "{ch.name}" }
                                        if muted_channels.contains(&ch.id) {
                                            span { class: "text-[10px] text-[var(--text-dim)]", title: "Muted", "🔕" }
                                        }
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
                        div { class: SECTION_LABEL, "Voice" }
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
                                        // Muting is the only item a plain member gets and it means
                                        // nothing on a voice channel, so without this the menu
                                        // opens empty for them.
                                        oncontextmenu: move |e: MouseEvent| {
                                            if !can_manage_channels {
                                                return;
                                            }
                                            e.prevent_default();
                                            let c = e.client_coordinates();
                                            chan_menu.set(Some(ChanMenu {
                                                channel: ctx_ch.clone(),
                                                x: c.x,
                                                y: c.y,
                                                mode: ChanMenuMode::Menu,
                                            }));
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
                                                v_leave.send(VoiceCmd::Disconnect { done: None });
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
                    can_manage: can_manage_channels,
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
    can_manage: bool,
    guild_order: Vec<Channel>,
    on_close: EventHandler<()>,
    on_mode: EventHandler<ChanMenuMode>,
) -> Element {
    let gateway = use_gateway();
    let mut state = use_app_state();
    let mut settings = use_context::<Signal<crate::settings::ClientSettings>>();
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
                            if matches!(ch.kind, ChannelKind::Text) {
                                {
                                let cid = ch.id;
                                let muted = state.read().channel_muted(cid);
                                rsx! {
                                    button {
                                        class: "w-full text-left px-3 py-1.5 rounded text-[var(--text)] hover:bg-white/[0.04] transition-colors",
                                        onclick: move |_| {
                                            let now = !muted;
                                            state.write().set_channel_muted(cid, now);
                                            let mut next = settings.read().clone();
                                            next.set_muted_channel(cid, now);
                                            settings.set(next.clone());
                                            crate::settings::save(&next);
                                            on_close.call(());
                                        },
                                        if muted { "Unmute channel" } else { "Mute channel" }
                                    }
                                }
                                }
                            }
                            if can_manage {
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
    // Green, not accent: amber already means "the channel you are reading", and
    // being connected here is true at the same time as that, not instead of it.
    let row_cls = if connected {
        "text-[var(--up)]"
    } else {
        "text-[var(--text-muted)] hover:text-[var(--text)] hover:bg-white/[0.03]"
    };
    let row_style = if connected {
        "background: color-mix(in srgb, var(--up) 12%, transparent);"
    } else {
        ""
    };
    let state = use_app_state();
    let users_by_id = state.read();
    let sharers: Vec<String> = users_by_id.screen_sharers_in(channel.id).to_vec();

    rsx! {
        div { class: "rounded",
            button {
                class: "relative w-full h-8 flex items-center gap-2 px-2.5 rounded-lg text-left text-[13.5px] transition-colors {row_cls}",
                style: "{row_style}",
                onclick: move |_| {
                    if connected { on_leave.call(()) } else { on_join.call(()) }
                },
                span {
                    class: "block w-4 h-4 shrink-0 text-[var(--text-dim)]",
                    draggable: false,
                    dangerous_inner_html: crate::features::icons::SPEAKER,
                }
                span { class: "truncate flex-1", draggable: false, "{channel.name}" }
                if connected {
                    span { class: "text-[9px] text-[var(--up)] font-semibold uppercase tracking-wider", "live" }
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
pub(crate) fn ConnectionStats() -> Element {
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
    let settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let self_pubkey = state.read().self_user.as_ref().map(|u| u.pubkey.clone());
    let sharing = state.read().screen_sharing;
    let camera_on = state.read().camera_on;
    let camera_starting = state.read().camera_starting;
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

    let screen_capture_available = state.read().screen_capture_available;
    let native_capture = crate::sysvideo::supported();
    let camera_capture_available = state.read().camera_capture_available;
    let voice_bitrate_kbps = state.read().voice_bitrate_kbps;
    let self_npub = use_context::<crate::identity::Identity>().npub();
    let self_npub_short = match self_npub.char_indices().nth(10).map(|(i, _)| i) {
        Some(cut) if self_npub.len() > 18 => {
            format!(
                "{}…{}",
                &self_npub[..cut],
                &self_npub[self_npub.len() - 4..]
            )
        }
        _ => self_npub.clone(),
    };

    let voice_channel_id = self_voice.channel_id;
    let (voice_where, voice_sub) = {
        let s = state.read();
        let ch = voice_channel_id.and_then(|cid| s.channels.iter().find(|c| c.id == cid));
        let where_name = ch.map(|c| c.name.clone()).unwrap_or_else(|| "voice".into());
        let guild = ch
            .and_then(|c| s.guilds.iter().find(|g| g.id == c.guild_id))
            .map(|g| g.name.clone())
            .unwrap_or_default();
        let n = voice_channel_id
            .map(|cid| {
                s.voice_states
                    .iter()
                    .filter(|v| v.channel_id == Some(cid))
                    .count()
            })
            .unwrap_or(0);
        (where_name, format!("{guild} · {n} connected"))
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

    // The trigger lives in the title bar; this panel only owns the device
    // signals the popover needs, so the flag travels through AppState.
    let show_audio_settings = use_memo(move || state.read().audio_settings);
    {
        let v_audio = voice.clone();
        use_effect(move || {
            if show_audio_settings() {
                v_audio.send(crate::features::voice::VoiceCmd::ListDevices);
                let _ = document::eval(&crate::features::camera::list_cameras_js());
            }
        });
    }

    // Either one moves the level the threshold is then judged against, and the
    // AGC moves it *up*, so the old `pre > post` test could never fire for it.

    rsx! {
        div { class: "border-t border-[var(--border)]",
            if show_banner {
                div { class: "px-3 pt-2.5 pb-2 border-b border-[var(--border)]",
                    div { class: "flex items-center gap-2",
                        span {
                            class: "w-2 h-2 rounded-full shrink-0",
                            style: "background:{dot_color};",
                            title: "{phase_text}",
                        }
                        span {
                            class: "text-[12.5px] font-semibold truncate",
                            style: "color:{dot_color};",
                            "{voice_where}"
                        }
                        span { class: "ml-auto shrink-0 font-mono text-[10.5px] text-[var(--text-dim)]",
                            "{voice_bitrate_kbps} kbps"
                        }
                    }
                    div { class: "mt-px text-[11px] text-[var(--text-dim)] truncate", "{voice_sub}" }
                }
            }
            // Always present, so you can arrive already muted; the share and
            // camera chips join it only once there is a call to point them at.
            div { class: "px-3 py-2 flex items-center gap-1.5",
                    button {
                        class: if muted {
                            "flex-1 h-9 flex items-center justify-center rounded-lg border transition-colors border-[var(--danger)] text-[var(--danger)]"
                        } else {
                            "flex-1 h-9 flex items-center justify-center rounded-lg border transition-colors border-[var(--border)] text-[var(--text-muted)] hover:text-[var(--text)] hover:border-[var(--border-strong)]"
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
                            "flex-1 h-9 flex items-center justify-center rounded-lg border transition-colors border-[var(--danger)] text-[var(--danger)]"
                        } else {
                            "flex-1 h-9 flex items-center justify-center rounded-lg border transition-colors border-[var(--border)] text-[var(--text-muted)] hover:text-[var(--text)] hover:border-[var(--border-strong)]"
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
                    if show_banner {
                        button {
                            class: if camera_on {
                                "flex-1 h-9 flex items-center justify-center rounded-lg border transition-colors border-[var(--accent)] bg-[var(--accent-soft)] text-[var(--accent)]"
                            } else {
                                "flex-1 h-9 flex items-center justify-center rounded-lg border transition-colors border-[var(--border)] text-[var(--text-muted)] hover:text-[var(--text)] hover:border-[var(--border-strong)]"
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
                                "flex-1 h-9 flex items-center justify-center rounded-lg border transition-colors border-[var(--accent)] bg-[var(--accent-soft)] text-[var(--accent)]"
                            } else {
                                "flex-1 h-9 flex items-center justify-center rounded-lg border transition-colors border-[var(--border)] text-[var(--text-muted)] hover:text-[var(--text)] hover:border-[var(--border-strong)]"
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
                            class: "shrink-0 h-9 px-3 flex items-center gap-1.5 rounded-lg border text-[11.5px] font-semibold text-[var(--danger)] transition-colors",
                            style: "background: color-mix(in srgb, var(--danger) 10%, transparent); border-color: color-mix(in srgb, var(--danger) 35%, transparent);",
                            title: "Leave voice",
                            onclick: move |_| {
                                g_for_hang.send(ClientMessage::LeaveVoice);
                                v_for_hang.send(VoiceCmd::Disconnect { done: None });
                            },
                            span { class: "block w-4 h-4", dangerous_inner_html: crate::features::icons::PHONE_OFF }
                            "Leave"
                        }
                    }
            }
            if let Some(err) = voice_error {
                div { class: "px-3 pb-2 text-[10px] text-[var(--danger)] break-all",
                    "{err}"
                }
            }
            div { class: "h-14 px-2 flex items-center gap-1 border-t border-[var(--border)]",
                crate::features::profiles::ProfileEditor {
                    class: "flex-1 min-w-0 h-11 px-1.5 flex items-center gap-2.5 rounded-lg text-left hover:bg-[var(--panel2)] transition-colors",
                    crate::features::profiles::Avatar {
                        pubkey: self_pubkey.clone().unwrap_or_default(),
                        name: name.clone(),
                        size: "w-8 h-8",
                    }
                    div { class: "flex-1 min-w-0",
                        div { class: "flex items-center gap-1.5",
                            span { class: "text-[12.5px] font-medium text-[var(--text)] truncate", "{name}" }
                            span { class: "text-[9px] font-semibold text-[var(--text-dim)] shrink-0", "Lv {level}" }
                        }
                        // The key under the name: this is the account, and the
                        // account is the key (trap 12).
                        span { class: "block font-mono text-[10px] text-[var(--text-dim)] truncate",
                            title: "{self_npub}",
                            "{self_npub_short}"
                        }
                        div { class: "mt-1 h-0.5 rounded-full overflow-hidden", style: "background: var(--bg2);",
                            div {
                                class: "h-full rounded-full",
                                style: "width: {xp_pct}%; background: linear-gradient(90deg, #8fb0ff, var(--accent));",
                            }
                        }
                    }
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
        s.mark_dm_read(channel_id);
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
