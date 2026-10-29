use dioxus::prelude::*;
use dioxus_grid_layout::NoDrag;

use crate::features::voice::{VoiceCmd, use_voice_tx};
use crate::identity::discriminator;
use crate::protocol::{
    Channel, ChannelKind, ClientMessage, DividerStyle, Id, Permission, VoiceState,
};
use crate::state::DmInfo;
use crate::state::{AppState, GatewayTx, VoicePhase, use_app_state, use_gateway};

/// `guild` is **every** channel in the guild, in draw order.
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Edge {
    Before,
    After,
}

/// Where `moved` will sit if dropped on `target`, as `reorder_positions`
/// places it: a row takes the target's slot, so it lands past it going down.
fn drop_edge(guild: &[Channel], moved: Id, target: Id) -> Option<Edge> {
    let from = guild.iter().position(|c| c.id == moved)?;
    let to = guild.iter().position(|c| c.id == target)?;
    if from == to {
        return None;
    }
    Some(if from < to { Edge::After } else { Edge::Before })
}

/// Both `top` and `bottom` always set: a style that drops a property keeps the
/// old one (trap 31), and the bar would stretch across the row.
#[component]
fn DropLine(edge: Edge) -> Element {
    let side = match edge {
        Edge::Before => "top:-3px; bottom:auto;",
        Edge::After => "top:auto; bottom:-3px;",
    };
    rsx! {
        span {
            class: "pointer-events-none absolute z-10",
            style: "{side} left:2px; right:2px; height:2px; border-radius:2px; background-color: var(--accent); box-shadow: 0 0 6px var(--accent);",
            span {
                class: "absolute",
                style: "left:-3px; top:-2px; width:6px; height:6px; border-radius:9999px; background-color: var(--accent);",
            }
        }
    }
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

#[component]
pub fn ChannelsColumn() -> Element {
    let mut state = use_app_state();
    let gateway = use_gateway();
    let (voice_drag_root, voice_drag_mount) = super::voice_drag::use_drag(state, gateway.clone());

    let snapshot = state.read();
    let dm_mode = snapshot.dm_mode;
    let has_dm_call = snapshot.dm_call.is_some();
    let dms: Vec<DmInfo> = snapshot.dms_by_recency(crate::state::DmStanding::Inbox);
    let requests = snapshot.dm_request_count();
    let dm_unread = snapshot.dm_unread.clone();
    let channel_unread = snapshot.channel_unread.clone();
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
    let invite_guild = selected_guild.filter(|gid| super::invite::can_invite(&snapshot, *gid));
    let guild_banner = guild
        .as_ref()
        .and_then(|g| g.banner.as_deref())
        .and_then(|b| snapshot.media_src(b))
        .map(str::to_string);
    drop(snapshot);

    let guild_order = channels.clone();

    let mut chan_menu = use_signal::<Option<ChanMenu>>(|| None);
    let settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let mut dm_confirming = use_signal::<Option<Id>>(|| None);
    let mut show_requests = use_signal(|| false);
    let mut show_create = use_signal(|| false);
    let mut dragging = use_signal::<Option<Id>>(|| None);
    let mut drop_at = use_signal::<Option<(Id, Edge)>>(|| None);

    let banner = if dm_mode { None } else { guild_banner };

    rsx! {
        aside { class: "dxf-channels-panel {PANEL}",
            if let Some(src) = banner {
                div { class: "dxf-guild-banner relative h-28 shrink-0 overflow-hidden border-b border-[var(--border)]",
                    img { class: "w-full h-full object-cover block", src: "{src}", alt: crate::i18n::text("guild banner", &[]) }
                    div {
                        class: "absolute inset-0",
                        style: "background: linear-gradient(180deg, transparent 40%, var(--panel-solid));",
                    }
                }
            }
            div { class: "dxf-channels-heading {HEADER}",
                h2 { class: "dxf-display text-[16px] font-bold tracking-tight text-[var(--text)] truncate flex-1",
                    if dm_mode {
                        {crate::i18n::text("Direct Messages", &[])}
                    } else {
                        {guild.as_ref().map(|g| g.name.clone()).unwrap_or_else(|| crate::i18n::text("No guild", &[]))}
                    }
                }
                if !dm_mode && let Some(gid) = invite_guild {
                    button {
                        class: "w-6 h-6 flex items-center justify-center rounded text-[var(--text-muted)] hover:text-[var(--accent)] transition-colors",
                        title: crate::i18n::text("Invite people", &[]),
                        onclick: move |_| state.write().guild_dialog = Some(crate::state::GuildDialog::Invite(gid)),
                        span { class: "block w-4 h-4", dangerous_inner_html: super::icons::USER_PLUS }
                    }
                }
                if !dm_mode && can_manage_channels {
                    button {
                        class: "w-6 h-6 flex items-center justify-center rounded border border-dashed border-[var(--border)] text-[var(--text-muted)] hover:text-[var(--accent)] hover:border-[var(--accent)] text-sm leading-none transition-colors",
                        title: crate::i18n::text("Create a channel", &[]),
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
                div { class: "dxf-dm-list flex-1 overflow-y-auto px-2 py-3 space-y-1",
                    StartDmByKey {}
                    if requests > 0 || show_requests() {
                        button {
                            class: "w-full flex items-center gap-2 px-2 py-1 rounded text-sm text-[var(--text-muted)] hover:text-[var(--text)] hover:bg-white/[0.03] transition-colors",
                            onclick: move |_| show_requests.toggle(),
                            span { class: "flex-1 text-left", {crate::i18n::text("Requests", &[])} }
                            span { class: "text-[var(--text-dim)] font-mono text-[10px]", "{requests}" }
                        }
                    }
                    if show_requests() {
                        super::dm_requests::RequestList {
                            on_open: move |_| {
                                if let Some(mut tab) = try_consume_context::<Signal<super::workspace::MobileTab>>() {
                                    tab.set(super::workspace::MobileTab::Chat);
                                }
                            }
                        }
                    }
                    if dms.is_empty() {
                        div { class: "px-2 text-xs text-[var(--text-dim)] leading-relaxed",
                            {crate::i18n::text("No conversations yet. Paste someone's npub above, or click a member.", &[])}
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
                            let unread = dm_unread.get(&cid).copied().unwrap_or(0);
                            let asking = dm_confirming() == Some(cid);
                            let del_cls = if asking {
                                "px-1.5 h-4 rounded-full border border-[var(--danger)] text-[9px] font-bold uppercase tracking-wide text-[var(--danger)] flex items-center"
                            } else {
                                "w-4 h-4 rounded-full hidden group-hover:flex items-center justify-center text-[10px] text-[var(--text-dim)] hover:text-[var(--danger)]"
                            };
                            rsx! {
                                div {
                                    key: "{cid}",
                                    class: "dxf-dm-row group w-full flex items-center gap-2 px-2 py-1 rounded text-sm transition-colors {cls}",
                                    button {
                                        class: "dxf-dm-open flex-1 min-w-0 flex items-center gap-2 text-left",
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
                                    if unread > 0 && !asking {
                                        span { class: "shrink-0 min-w-5 h-5 px-1.5 rounded-full bg-[var(--accent)] text-[#1a1206] text-[11.5px] font-bold flex items-center justify-center group-hover:hidden",
                                            "{unread}"
                                        }
                                    }
                                    if muted && !asking {
                                        span {
                                            class: "shrink-0 text-[9px] text-[var(--text-dim)] group-hover:hidden",
                                            title: crate::i18n::text("Muted", &[]),
                                            "🔕"
                                        }
                                    }
                                    if !asking {
                                        button {
                                            class: "shrink-0 w-4 h-4 rounded-full hidden group-hover:flex items-center justify-center text-[10px] text-[var(--text-dim)] hover:text-[var(--text)] transition-colors",
                                            title: if muted { crate::i18n::text("Unmute this conversation", &[]) } else { crate::i18n::text("Mute this conversation", &[]) },
                                            onclick: move |_| crate::state::set_dm_muted(state, settings, cid, !muted),
                                            if muted { "🔔" } else { "🔕" }
                                        }
                                    }
                                    button {
                                        class: "shrink-0 justify-center transition-colors {del_cls}",
                                        title: crate::i18n::text("Delete this conversation on this machine. The relays keep their copy, and a new message reopens it.", &[]),
                                        onclick: move |_| {
                                            if !asking {
                                                dm_confirming.set(Some(cid));
                                                return;
                                            }
                                            crate::state::forget_dm(state, settings, &peer);
                                            dm_confirming.set(None);
                                        },
                                        if asking { {crate::i18n::text("Del?", &[])} } else { "✕" }
                                    }
                                }
                            }
                        }
                    }
                }
            } else {
            div {
                id: "{voice_drag_root}",
                onmounted: voice_drag_mount,
                class: "flex-1 overflow-y-auto px-2 py-3 space-y-3",
                // Pointer drag, not HTML5 DnD: Windows turns HTML5 DnD off while
                // the wry file-drop handler is installed. The rail commits it.
                onpointerup: {
                    let gateway = gateway.clone();
                    let order = guild_order.clone();
                    move |_| {
                        let moved = dragging();
                        let at = drop_at();
                        dragging.set(None);
                        drop_at.set(None);
                        if let (Some(moved), Some((target, _))) = (moved, at) {
                            send_reorder(&gateway, &order, moved, target);
                        }
                    }
                },
                onpointerleave: move |_| {
                    dragging.set(None);
                    drop_at.set(None);
                },
                onpointercancel: move |_| {
                    dragging.set(None);
                    drop_at.set(None);
                },
                div {
                    for channel in channels.iter() {
                        {
                            let ch = channel.clone();
                            let cid = ch.id;
                            let drop_group = guild_order.clone();
                            let line = drop_at().filter(|(id, _)| *id == cid).map(|(_, edge)| edge);
                            let dim = if dragging() == Some(cid) { "opacity-40" } else { "" };
                            if ch.kind.is_divider() {
                                let menu_ch = ch.clone();
                                rsx! {
                                    div {
                                        key: "{cid}",
                                        class: "relative mt-2.5 mb-0.5 px-2 flex items-center gap-2 select-none {dim}",
                                        onpointerdown: move |e: PointerEvent| {
                                            if can_manage_channels
                                                && e.held_buttons().contains(
                                                    dioxus::html::input_data::MouseButton::Primary,
                                                )
                                            {
                                                dragging.set(Some(cid));
                                                drop_at.set(None);
                                            }
                                        },
                                        onpointerenter: move |_| {
                                            let Some(moved) = dragging() else { return };
                                            let next = drop_edge(&drop_group, moved, cid).map(|edge| (cid, edge));
                                            if *drop_at.peek() != next {
                                                drop_at.set(next);
                                            }
                                        },
                                        oncontextmenu: move |e: MouseEvent| {
                                            if !can_manage_channels {
                                                return;
                                            }
                                            e.prevent_default();
                                            let c = e.client_coordinates();
                                            chan_menu.set(Some(ChanMenu {
                                                channel: menu_ch.clone(),
                                                x: c.x,
                                                y: c.y,
                                                mode: ChanMenuMode::Menu,
                                            }));
                                        },
                                        if let Some(edge) = line {
                                            DropLine { edge }
                                        }
                                        DividerLabel { name: ch.name.clone(), style: ch.divider, guild_id: ch.guild_id }
                                        if ch.access.is_some() {
                                            span {
                                                class: "text-[10px] text-[var(--text-dim)]",
                                                title: crate::i18n::text("Hidden from everyone but the roles and people allowed", &[]),
                                                "🔒"
                                            }
                                        }
                                    }
                                }
                            } else if ch.kind == ChannelKind::Voice {
                                let in_this = self_voice.channel_id == Some(cid);
                                let g_join = gateway.clone();
                                let occupants: Vec<VoiceState> = voice_states
                                    .iter()
                                    .filter(|v| v.channel_id == Some(cid))
                                    .cloned()
                                    .collect();
                                let ctx_ch = ch.clone();
                                rsx! {
                                    div {
                                        key: "{cid}",
                                        class: "relative {dim}",
                                        onpointerdown: move |e: PointerEvent| {
                                            if can_manage_channels
                                                && e.held_buttons().contains(
                                                    dioxus::html::input_data::MouseButton::Primary,
                                                )
                                            {
                                                dragging.set(Some(cid));
                                                drop_at.set(None);
                                            }
                                        },
                                        onpointerenter: move |_| {
                                            let Some(moved) = dragging() else { return };
                                            let next = drop_edge(&drop_group, moved, cid).map(|edge| (cid, edge));
                                            if *drop_at.peek() != next {
                                                drop_at.set(next);
                                            }
                                        },
                                        oncontextmenu: move |e: MouseEvent| {
                                            if !can_manage_channels && !super::voice_invite::can_invite_to(&state.peek(), &ctx_ch) {
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
                                        if let Some(edge) = line {
                                            DropLine { edge }
                                        }
                                        VoiceChannelRow {
                                            channel: ch.clone(),
                                            connected: in_this,
                                            occupants: occupants,
                                            self_pubkey: self_user.as_ref().map(|u| u.pubkey.clone()),
                                            on_join: move |_| {
                                                if state.peek().dm_call.is_some() {
                                                    state.write().error_toast = Some("End your DM call before joining a voice channel.".into());
                                                    return;
                                                }
                                                tracing::debug!(channel_id = %cid, "join voice clicked");
                                                g_join.send(state.read().voice.join_message(cid));
                                            },
                                        }
                                    }
                                }
                            } else {
                                let active = selected_channel == Some(cid);
                                let cls = if active {
                                    "text-[var(--text)] font-medium bg-[var(--panel2)]"
                                } else {
                                    "text-[var(--text-dim)] hover:text-[var(--text-muted)] hover:bg-white/[0.03]"
                                };
                                let g2 = gateway.clone();
                                let ctx_ch = ch.clone();
                                rsx! {
                                    button {
                                        key: "{cid}",
                                        class: "dxf-text-channel relative w-full h-8 flex items-center gap-2 px-2.5 rounded-lg text-left text-[13.5px] transition-colors {cls} {dim}",
                                        onpointerdown: move |e: PointerEvent| {
                                            if can_manage_channels
                                                && e.held_buttons().contains(
                                                    dioxus::html::input_data::MouseButton::Primary,
                                                )
                                            {
                                                dragging.set(Some(cid));
                                                drop_at.set(None);
                                            }
                                        },
                                        onpointerenter: move |_| {
                                            let Some(moved) = dragging() else { return };
                                            let next = drop_edge(&drop_group, moved, cid).map(|edge| (cid, edge));
                                            if *drop_at.peek() != next {
                                                drop_at.set(next);
                                            }
                                        },
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
                                        if let Some(edge) = line {
                                            DropLine { edge }
                                        }
                                        if active {
                                            span {
                                                class: "absolute",
                                                style: "left:-8px; top:50%; transform:translateY(-50%); width:3px; height:18px; border-radius:0 3px 3px 0; background: var(--accent);",
                                            }
                                        }
                                        span { class: "font-mono text-[13px] opacity-70", draggable: false, "#" }
                                        span { class: "truncate flex-1", draggable: false, "{ch.name}" }
                                        if channel_unread.contains(&cid) {
                                            span {
                                                title: crate::i18n::text("Unread messages", &[]),
                                                aria_label: crate::i18n::text("Unread messages", &[]),
                                                style: "width:6px;height:6px;border-radius:50%;background:var(--accent);flex-shrink:0;",
                                            }
                                        }
                                        if muted_channels.contains(&ch.id) {
                                            span { class: "text-[10px] text-[var(--text-dim)]", title: crate::i18n::text("Muted", &[]), "🔕" }
                                        }
                                        if ch.read_only {
                                            span { class: "text-[10px] text-[var(--text-dim)]", title: crate::i18n::text("Read-only", &[]), "🔒" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            }

            if has_dm_call {
                div { style: "flex-shrink: 0; min-height: 0; max-height: 45%; overflow-y: auto;",
                    crate::features::dm_call::CallPanel { embedded: true }
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
    let style = use_signal(DividerStyle::default);

    let mut submit = move || {
        let n = name().trim().to_string();
        if !n.is_empty() {
            gateway.send(ClientMessage::CreateChannel {
                divider: style(),
                guild_id,
                name: n,
                kind: kind(),
                topic: None,
            });
        }
        name.set(String::new());
        on_done.call(());
    };
    let dividing = kind() == ChannelKind::Divider;

    rsx! {
        form {
            class: "px-2 py-2 border-b border-[var(--border)] flex flex-col gap-1.5",
            onsubmit: move |_| submit(),
            input {
                class: "w-full bg-transparent border border-[var(--border)] focus:border-[var(--accent)] rounded px-2 py-1 text-xs text-[var(--text)] outline-none transition-colors",
                placeholder: if dividing { crate::i18n::text("Divider text or emoji…", &[]) } else { crate::i18n::text("Channel name…", &[]) },
                value: "{name}",
                autofocus: true,
                maxlength: 64,
                oninput: move |e| name.set(e.value()),
            }
            div { class: "flex gap-1",
                for (k, label) in [
                    (ChannelKind::Text, "# Text"),
                    (ChannelKind::Voice, "♪ Voice"),
                    (ChannelKind::Divider, "— Divider"),
                ] {
                    ToggleChip { on: kind() == k, onclick: move |_| kind.set(k), {crate::i18n::label(label)} }
                }
                button {
                    r#type: "submit",
                    class: "rounded px-2 py-0.5 text-[10px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors",
                    {crate::i18n::text("Add", &[])}
                }
            }
            if dividing {
                DividerStylePicker { style }
            }
        }
    }
}

#[component]
fn ToggleChip(on: bool, onclick: EventHandler<MouseEvent>, children: Element) -> Element {
    rsx! {
        button {
            r#type: "button",
            class: if on {
                "flex-1 rounded px-1 py-0.5 text-[10px] uppercase tracking-wider text-[var(--accent)] border border-[var(--accent)] transition-colors"
            } else {
                "flex-1 rounded px-1 py-0.5 text-[10px] uppercase tracking-wider text-[var(--text-dim)] border border-[var(--border)] hover:text-[var(--text-muted)] transition-colors"
            },
            onclick: move |e| onclick.call(e),
            {children}
        }
    }
}

#[component]
fn DividerStylePicker(style: Signal<DividerStyle>) -> Element {
    rsx! {
        div { class: "flex gap-1",
            ToggleChip { on: style() == DividerStyle::Title, onclick: move |_| style.set(DividerStyle::Title), {crate::i18n::text("Title ——", &[])} }
            ToggleChip { on: style() == DividerStyle::Centered, onclick: move |_| style.set(DividerStyle::Centered), {crate::i18n::text("—— Line ——", &[])} }
        }
    }
}

/// A divider's name as its style draws it; emoji in it are pictures.
#[component]
fn DividerLabel(name: String, style: DividerStyle, guild_id: Id) -> Element {
    let label = rsx! {
        span { class: "min-w-0 text-[10px] font-semibold uppercase tracking-wider text-[var(--text-dim)] truncate",
            crate::features::chat::EmojiText { text: name, guild_id: Some(guild_id), compact: true }
        }
    };
    match style {
        DividerStyle::Title => rsx! {
            {label}
            div { class: "flex-1 h-px bg-[var(--border)]" }
        },
        DividerStyle::Centered => rsx! {
            div { class: "flex-1 h-px bg-[var(--border)]" }
            {label}
            div { class: "flex-1 h-px bg-[var(--border)]" }
        },
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
    let at = guild_order.iter().position(|c| c.id == ch.id);
    let move_up = at.filter(|i| *i > 0).map(|i| guild_order[i - 1].id);
    let move_down = at
        .filter(|i| *i + 1 < guild_order.len())
        .map(|i| guild_order[i + 1].id);
    let mut name = use_signal(|| ch.name.clone());
    let mut topic = use_signal(|| ch.topic.clone().unwrap_or_default());
    let style = use_signal(|| ch.divider);

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
                            if super::voice_invite::can_invite_to(&state.read(), &ch) {
                                button {
                                    class: "w-full text-left px-3 py-1.5 rounded text-[var(--text)] hover:bg-white/[0.04] transition-colors",
                                    onclick: move |_| {
                                        state.write().guild_dialog = Some(crate::state::GuildDialog::VoiceInvite(ch.id));
                                        on_close.call(());
                                    },
                                    {crate::i18n::text("Invite to voice by DM", &[])}
                                }
                            }
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
                                        if muted { {crate::i18n::text("Unmute channel", &[])} } else { {crate::i18n::text("Mute channel", &[])} }
                                    }
                                }
                                }
                            }
                            if can_manage && ch.kind.can_be_hidden() {
                                {
                                    let cid = ch.id;
                                    rsx! {
                                        button {
                                            class: "w-full text-left px-3 py-1.5 rounded text-[var(--text)] hover:bg-white/[0.04] transition-colors",
                                            onclick: move |_| {
                                                state.write().channel_access_open = Some(cid);
                                                on_close.call(());
                                            },
                                            {crate::i18n::text("Who can see this…", &[])}
                                        }
                                    }
                                }
                            }
                            if can_manage {
                                button {
                                    class: "w-full text-left px-3 py-1.5 rounded text-[var(--text)] hover:bg-white/[0.04] transition-colors",
                                    onclick: move |_| on_mode.call(ChanMenuMode::Edit),
                                    if ch.kind.is_divider() { {crate::i18n::text("Edit divider", &[])} } else { {crate::i18n::text("Edit name & topic", &[])} }
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
                                                {crate::i18n::text("Move up", &[])}
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
                                                {crate::i18n::text("Move down", &[])}
                                            }
                                        }
                                    }
                                }
                                if matches!(ch.kind, ChannelKind::Text) {
                                    button {
                                        class: "w-full text-left px-3 py-1.5 rounded text-[var(--text)] hover:bg-white/[0.04] transition-colors",
                                        onclick: move |_| {
                                            gw_ro.send(ClientMessage::UpdateChannel {
                                                divider: None,
                                                channel_id: ch_ro.id,
                                                name: ch_ro.name.clone(),
                                                topic: ch_ro.topic.clone(),
                                                read_only: !ch_ro.read_only,
                                                position: ch_ro.position,
                                                slowmode_secs: ch_ro.slowmode_secs,
                                                markdown: None,
                                            });
                                            on_close.call(());
                                        },
                                        if ch.read_only { {crate::i18n::text("🔓 Make writable", &[])} } else { {crate::i18n::text("🔒 Make read-only", &[])} }
                                    }
                                    {
                                        let gw_md = gateway.clone();
                                        let ch_md = ch.clone();
                                        rsx! {
                                            button {
                                                class: "w-full text-left px-3 py-1.5 rounded text-[var(--text)] hover:bg-white/[0.04] transition-colors",
                                                title: crate::i18n::text("Headings, lists, quotes, code and links render in this channel; bold, italic and underline work everywhere", &[]),
                                                onclick: move |_| {
                                                    gw_md.send(ClientMessage::UpdateChannel {
                                                        divider: None,
                                                        channel_id: ch_md.id,
                                                        name: ch_md.name.clone(),
                                                        topic: ch_md.topic.clone(),
                                                        read_only: ch_md.read_only,
                                                        position: ch_md.position,
                                                        slowmode_secs: ch_md.slowmode_secs,
                                                        markdown: Some(!ch_md.markdown),
                                                    });
                                                    on_close.call(());
                                                },
                                                if ch.markdown { {crate::i18n::text("Turn off markdown", &[])} } else { {crate::i18n::text("Allow markdown", &[])} }
                                            }
                                        }
                                    }
                                }
                                button {
                                    class: "w-full text-left px-3 py-1.5 rounded text-[var(--danger)] hover:bg-[var(--danger)]/10 transition-colors",
                                    onclick: move |_| on_mode.call(ChanMenuMode::ConfirmDelete),
                                    if ch.kind.is_divider() { {crate::i18n::text("Delete divider", &[])} } else { {crate::i18n::text("Delete channel", &[])} }
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
                                            divider: ch2.kind.is_divider().then_some(style()),
                                            channel_id: ch2.id,
                                            name: n,
                                            topic: if t.is_empty() { None } else { Some(t) },
                                            read_only: ch2.read_only,
                                            position: ch2.position,
                                            slowmode_secs: ch2.slowmode_secs,
                                            markdown: None,
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
                                if ch.kind.is_divider() {
                                    DividerStylePicker { style }
                                } else {
                                    div { class: "relative flex items-center gap-1",
                                        input {
                                            class: "flex-1 min-w-0 bg-transparent border border-[var(--border)] focus:border-[var(--accent)] rounded px-2 py-1 text-xs text-[var(--text)] outline-none transition-colors",
                                            placeholder: crate::i18n::text("Topic (optional)", &[]),
                                            value: "{topic}",
                                            maxlength: TOPIC_MAX_CHARS,
                                            oninput: move |e| topic.set(e.value()),
                                        }
                                        TopicEmojiPicker {
                                            guild_id: ch.guild_id,
                                            anchor_left: true,
                                            on_pick: move |emoji: String| {
                                                let mut t = topic.write();
                                                if t.chars().count() + emoji.chars().count() <= TOPIC_MAX_CHARS {
                                                    t.push_str(&emoji);
                                                }
                                            },
                                        }
                                    }
                                }
                                button {
                                    r#type: "submit",
                                    class: "rounded px-2 py-1 text-[10px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors",
                                    {crate::i18n::text("Save", &[])}
                                }
                            }
                        }
                    },
                    ChanMenuMode::ConfirmDelete => {
                        let gw = gateway.clone();
                        let cid = ch.id;
                        rsx! {
                            div { class: "px-3 py-1.5 text-xs text-[var(--text-muted)]",
                                if ch.kind.is_divider() {
                                    {crate::i18n::text("Delete this divider?", &[])}
                                } else {
                                    {crate::i18n::text("Delete #{0}? Its messages are gone for good.", std::slice::from_ref(&(ch . name)))}
                                }
                            }
                            div { class: "flex gap-1 px-1 pb-0.5",
                                button {
                                    class: "flex-1 px-2 py-1 rounded text-xs uppercase tracking-wider text-[var(--danger)] border border-[var(--danger)]/40 hover:bg-[var(--danger)]/10 transition-colors",
                                    onclick: move |_| {
                                        gw.send(ClientMessage::DeleteChannel { channel_id: cid });
                                        on_close.call(());
                                    },
                                    {crate::i18n::text("Delete", &[])}
                                }
                                button {
                                    class: "flex-1 px-2 py-1 rounded text-xs uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--text-muted)] transition-colors",
                                    onclick: move |_| on_close.call(()),
                                    {crate::i18n::text("Cancel", &[])}
                                }
                            }
                        }
                    },
                }
            }
        }
    }
}

/// The server's `sanitize::TOPIC`; the form stops where the server would cut.
const TOPIC_MAX_CHARS: usize = 120;

/// This guild's emoji then the named set, appended to the topic as `:code:` or
/// the character. `EmojiText` draws either wherever the topic is shown.
#[component]
pub(crate) fn TopicEmojiPicker(
    guild_id: Id,
    on_pick: EventHandler<String>,
    /// Drawn on the button in place of 🙂, for a picker that holds one choice.
    #[props(default)]
    current: Option<String>,
    #[props(default = "Add an emoji to the topic".to_string())] title: String,
    /// Grow rightward, for a picker near the window's left edge.
    #[props(default)]
    anchor_left: bool,
) -> Element {
    let state = use_app_state();
    let mut open = use_signal(|| false);
    let guild_emojis: Vec<(String, String)> = {
        let s = state.read();
        s.emojis_of(guild_id)
            .iter()
            .map(|e| {
                let url = s.emoji_images.get(&e.image).cloned().unwrap_or_default();
                (e.shortcode.clone(), url)
            })
            .collect()
    };
    rsx! {
        button {
            r#type: "button",
            class: if open() {
                "w-7 h-7 shrink-0 flex items-center justify-center rounded-lg border border-[var(--accent)] bg-[var(--panel2)] text-base leading-none transition-colors"
            } else {
                "w-7 h-7 shrink-0 flex items-center justify-center rounded-lg border border-[var(--border)] bg-[var(--panel2)] text-base leading-none hover:border-[var(--accent)] transition-colors"
            },
            title: "{title}",
            onclick: move |_| open.toggle(),
            match current.clone() {
                Some(text) => rsx! { crate::features::chat::EmojiText { text, guild_id: Some(guild_id), compact: true } },
                None => rsx! { "🙂" },
            }
        }
        if open() {
            div {
                class: "dxf-pop-in absolute top-full mt-1 z-30 p-2 flex flex-col gap-2 bg-[var(--panel-solid)] border border-[var(--border)] rounded-xl shadow-lg overflow-y-auto",
                style: if anchor_left {
                    "left: 0; width: 18rem; max-height: 18rem;"
                } else {
                    "right: 0; width: 18rem; max-height: 18rem;"
                },
                if !guild_emojis.is_empty() {
                    div { class: PICKER_HEADING, {crate::i18n::text("This server", &[])} }
                    div { class: "grid grid-cols-8 gap-1",
                        for (code, url) in guild_emojis.into_iter() {
                            {
                                let emoji = format!(":{code}:");
                                rsx! {
                                    button {
                                        key: "{code}",
                                        r#type: "button",
                                        class: PICKER_TILE,
                                        title: "{emoji}",
                                        onclick: move |_| { on_pick.call(emoji.clone()); open.set(false); },
                                        if url.is_empty() {
                                            span { class: "text-[10px] text-[var(--text-dim)]", "…" }
                                        } else {
                                            img { src: "{url}", style: "height: 1.5rem; width: auto; max-width: 1.75rem; object-fit: contain;" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                div { class: PICKER_HEADING, {crate::i18n::text("Standard", &[])} }
                div { class: "grid grid-cols-8 gap-1",
                    for (emoji, name) in crate::features::chat::EMOJIS.iter().copied() {
                        button {
                            key: "{name}",
                            r#type: "button",
                            class: PICKER_TILE,
                            style: "font-size: 1.25rem;",
                            title: ":{name}:",
                            onclick: move |_| { on_pick.call(emoji.to_string()); open.set(false); },
                            "{emoji}"
                        }
                    }
                }
            }
        }
    }
}

const PICKER_HEADING: &str =
    "px-0.5 text-[10px] font-semibold uppercase tracking-wider text-[var(--text-dim)]";
const PICKER_TILE: &str = "w-8 h-8 flex items-center justify-center rounded-lg leading-none hover:bg-white/[0.06] transition-colors";

#[component]
fn VoiceChannelRow(
    channel: Channel,
    connected: bool,
    occupants: Vec<VoiceState>,
    self_pubkey: Option<String>,
    on_join: EventHandler<()>,
) -> Element {
    // Green, not accent: amber already means "the channel you are reading", and
    // being connected here is true at the same time as that, not instead of it.
    let row_cls = if connected {
        "text-[var(--up)]"
    } else {
        "text-[var(--text-muted)] hover:text-[var(--text)] hover:bg-white/[0.03]"
    };
    // Being in the call already, a click has nothing to do; leaving is the
    // hang-up button's, and a stray click here used to drop the call.
    let row_style = if connected {
        "background: color-mix(in srgb, var(--up) 12%, transparent); cursor: default;"
    } else {
        ""
    };
    let channel_id = channel.id;
    let mut state = use_app_state();
    let users_by_id = state.read();
    let sharers: Vec<String> = users_by_id.screen_sharers_in(channel.id).to_vec();

    rsx! {
        div {
            class: "rounded",
            "data-voice-channel": "{channel_id}",
            div { class: "flex items-center gap-1",
            button {
                class: "relative flex-1 min-w-0 min-h-8 py-1 flex items-center gap-2 px-2.5 rounded-lg text-left text-[13.5px] transition-colors {row_cls}",
                style: "{row_style}",
                onclick: move |_| {
                    if !connected {
                        on_join.call(());
                    }
                },
                span {
                    class: "block w-4 h-4 shrink-0 text-[var(--text-dim)]",
                    draggable: false,
                    dangerous_inner_html: crate::features::icons::SPEAKER,
                }
                span { class: "flex-1 min-w-0", draggable: false,
                    span { class: "block truncate", "{channel.name}" }
                    if let Some(topic) = channel.topic.clone() {
                        span {
                            class: "block truncate text-[11px] leading-tight text-[var(--text-dim)]",
                            title: "{topic}",
                            crate::features::chat::EmojiText { text: topic.clone(), guild_id: Some(channel.guild_id), compact: true }
                        }
                    }
                }
                if channel.access.is_some() {
                    span {
                        class: "text-[10px] text-[var(--text-dim)]",
                        title: crate::i18n::text("Hidden from everyone but the roles and people allowed", &[]),
                        "🔒"
                    }
                }
            }
            if connected && super::voice_invite::can_invite_to(&users_by_id, &channel) {
                button {
                    r#type: "button",
                    class: "w-8 h-8 shrink-0 flex items-center justify-center rounded text-[var(--text-dim)] hover:text-[var(--accent)]",
                    title: crate::i18n::text("Invite to voice by DM", &[]),
                    aria_label: crate::i18n::text("Invite to voice by DM", &[]),
                    onpointerdown: move |e: PointerEvent| e.stop_propagation(),
                    onclick: move |e: MouseEvent| {
                        e.stop_propagation();
                        state.write().guild_dialog = Some(crate::state::GuildDialog::VoiceInvite(channel_id));
                    },
                    span { dangerous_inner_html: super::icons::USER_PLUS }
                }
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
                                    channel_id: channel.id,
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
        let _ = document::eval(&crate::features::screenshare::screen_stats_js(true));
    });
    {
        let voice = voice.clone();
        use_drop(move || {
            voice.send(VoiceCmd::SetStatsPolling { enabled: false });
            let _ = document::eval(&crate::features::screenshare::screen_stats_js(false));
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
    let (screen_stats, screen_stats_in, native_screen_share) = {
        let s = state.read();
        (
            s.screen_share_stats.clone(),
            s.screen_share_in_stats.clone(),
            s.screen_sharing && crate::sysvideo::supported(),
        )
    };

    rsx! {
        div { class: "mt-1 border-t border-[var(--border)] pt-1.5",
            if rows.is_empty() {
                span { class: "text-[10px] text-[var(--text-dim)] block",
                    {crate::i18n::text("Waiting for the first reading — join a voice channel.", &[])}
                }
            }
            for (name, st) in rows.iter() {
                div { key: "{name}", class: "flex flex-wrap items-baseline gap-1.5 text-[10px] font-mono py-0.5",
                    span { class: "truncate flex-1 text-[var(--text-muted)]", "{name}" }
                    match st {
                        crate::state::TrackStats::Unavailable => rsx! {
                            span { class: "text-[var(--text-dim)]", {crate::i18n::label("Audio statistics temporarily unavailable")} }
                        },
                        crate::state::TrackStats::Inbound { loss_pct, recent_loss_pct, discarded_packets, concealed_pct, jitter_ms, buffer_ms, concealment_events } => rsx! {
                            span {
                                class: if recent_loss_pct.is_some_and(|v| v >= 1.0) { "text-[var(--warn)]" } else { "text-[var(--text-dim)]" },
                                title: crate::i18n::text("Packet loss over the last 5 seconds. Lifetime loss: {0}%.", &[format!("{loss_pct:.1}")]),
                                {crate::i18n::text("{0}% loss", &[recent_loss_pct.map_or("—".into(), |v| format!("{v:.1}"))])}
                            }
                            span {
                                class: "text-[var(--text-dim)]",
                                title: crate::i18n::text("Discarded audio blocks over the last 5 seconds, including unused RED redundancy. This is not a percentage of lost voice.", &[]),
                                {crate::i18n::text("{0} discarded", &[discarded_packets.map_or("—".into(), |v| v.to_string())])}
                            }
                            span {
                                class: if concealed_pct.is_some_and(|v| v >= 1.0) { "text-[var(--warn)]" } else { "text-[var(--text-dim)]" },
                                title: crate::i18n::text("Non-silent audio concealed over the last 5 seconds: {0}%. Silent concealment is excluded. Total concealment events: {1}.", &[concealed_pct.map_or("—".into(), |v| format!("{v:.1}")), concealment_events.to_string()]),
                                {crate::i18n::text("{0}% concealed", &[concealed_pct.map_or("—".into(), |v| format!("{v:.1}"))])}
                            }
                            span { class: "text-[var(--text-dim)]", title: crate::i18n::text("Network jitter", &[]), {crate::i18n::text("{0}ms jit", &[format!("{0:.0}" , jitter_ms)])} }
                            span {
                                class: "text-[var(--text-dim)]",
                                title: crate::i18n::text("Average target jitter buffer delay since this track started", &[]),
                                {crate::i18n::text("{0}ms buf", &[format!("{0:.0}" , buffer_ms)])}
                            }
                            span {
                                class: "text-[var(--text-dim)]",
                                title: crate::i18n::text("Non-silent audio concealed over the last 5 seconds: {0}%. Silent concealment is excluded. Total concealment events: {1}.", &[concealed_pct.map_or("—".into(), |v| format!("{v:.1}")), concealment_events.to_string()]),
                                {crate::i18n::text("{0} conc", &[(concealment_events).to_string()])}
                            }
                        },
                        crate::state::TrackStats::Outbound { bitrate_kbps, packets_per_sec, target_kbps, loss_pct } => rsx! {
                            if let Some(loss) = loss_pct {
                                span {
                                    class: if *loss >= 1.0 { "text-[var(--warn)]" } else { "text-[var(--text-dim)]" },
                                    title: crate::i18n::text("Outbound packet loss reported by SFU feedback", &[]),
                                    {crate::i18n::text("{0}% loss", &[format!("{loss:.1}")])}
                                }
                            }
                            span {
                                class: "text-[var(--text-dim)]",
                                title: crate::i18n::text("Sent audio payload, including redundancy. Current voice bitrate ceiling: {0} kbit/s.", &[(target_kbps).to_string()]),
                                {rate_or_dash(*bitrate_kbps, "kbit/s out")}
                            }
                            span {
                                class: "text-[var(--text-dim)]",
                                title: crate::i18n::text("Packets a second — around 50 while you're transmitting, near zero while silence is held back", &[]),
                                {rate_or_dash(*packets_per_sec, "pkt/s")}
                            }
                        },
                    }
                }
            }
            ScreenShareStatsRow {
                stats: screen_stats,
                direction: "outbound",
                native_screen_share,
            }
            ScreenShareStatsRow {
                stats: screen_stats_in,
                direction: "inbound",
                native_screen_share: false,
            }
        }
    }
}

#[component]
fn ScreenShareStatsRow(
    stats: Option<crate::state::ScreenShareStats>,
    direction: &'static str,
    native_screen_share: bool,
) -> Element {
    let direction_label = crate::i18n::label(direction);
    let Some(stats) = stats else {
        return rsx! {
            div { class: "mt-1 border-t border-[var(--border)] pt-1.5 text-[10px] text-[var(--text-dim)]",
                span { class: "text-[var(--text-muted)]", {crate::i18n::text("Screen share {0}: ", &[direction_label.to_owned()])} }
                if direction == "outbound" && native_screen_share {
                    {crate::i18n::text("Waiting for native capture stats.", &[])}
                } else if direction == "inbound" {
                    {crate::i18n::text("Waiting for a remote screen share.", &[])}
                } else {
                    {crate::i18n::text("Waiting for a local WebView screen share.", &[])}
                }
            }
        };
    };

    let capture_size = match (stats.capture_width, stats.capture_height) {
        (Some(width), Some(height)) => format!("{width}x{height}"),
        _ => "—".into(),
    };
    let capture_fps = stats
        .capture_fps
        .map(|v| format!("{v:.0}"))
        .unwrap_or_else(|| "—".into());
    let encoded_size = match (stats.encoded_width, stats.encoded_height) {
        (Some(width), Some(height)) => format!("{width}x{height}"),
        _ => "—".into(),
    };
    let encoded_fps = stats
        .encoded_fps
        .map(|v| format!("{v:.0}"))
        .unwrap_or_else(|| "—".into());
    let codec = stats.codec.as_deref().unwrap_or("—");
    let implementation = stats
        .codec_implementation
        .as_deref()
        .unwrap_or_else(|| crate::i18n::label("not reported"));
    let power_efficient = match stats.power_efficient {
        Some(true) => crate::i18n::label("yes"),
        Some(false) => crate::i18n::label("no"),
        None => crate::i18n::label("not reported"),
    };
    let limitation = stats
        .quality_limitation_reason
        .as_deref()
        .map(crate::i18n::label)
        .unwrap_or_else(|| crate::i18n::label("not reported"));
    let frames = stats
        .frames
        .map(|v| v.to_string())
        .unwrap_or_else(|| "—".into());
    let packets = stats
        .packets
        .map(|v| v.to_string())
        .unwrap_or_else(|| "—".into());
    let lost = stats
        .packets_lost
        .map(|v| v.to_string())
        .unwrap_or_else(|| "—".into());
    let bitrate = rate_or_dash(
        stats.bitrate_kbps,
        if stats.outbound {
            crate::i18n::label("kbit/s out")
        } else {
            crate::i18n::label("kbit/s in")
        },
    );
    let codec_path_label = crate::i18n::text(
        if stats.outbound {
            "encoder {0}"
        } else {
            "decoder {0}"
        },
        &[implementation.to_owned()],
    );
    let efficiency_label = crate::i18n::text("power-efficient {0}", &[power_efficient.to_owned()]);
    let decoder_mode = super::screen_stats::decoder_mode(stats.codec_implementation.as_deref());
    let decoding_label = crate::i18n::text(
        "Decoding: {0}",
        &[crate::i18n::label(decoder_mode).to_owned()],
    );
    let frames_label = crate::i18n::text(
        if stats.outbound {
            "{0} frames encoded"
        } else {
            "{0} frames decoded"
        },
        &[frames],
    );
    let packets_label = crate::i18n::text(
        if stats.outbound {
            "{0} packets sent / {1} lost"
        } else {
            "{0} packets received / {1} lost"
        },
        &[packets, lost],
    );
    let capture_label = crate::i18n::text("capture {0} @ {1} fps", &[capture_size, capture_fps]);
    let encoded_label = crate::i18n::text(
        if stats.outbound {
            "encoded {0} @ {1} fps"
        } else {
            "received {0} @ {1} fps"
        },
        &[encoded_size, encoded_fps],
    );
    let target_bitrate = stats
        .target_bitrate_kbps
        .map(|v| crate::i18n::text("target {0} kbit/s", &[v.to_string()]));
    let jitter = stats
        .jitter_ms
        .map(|v| crate::i18n::text("{0} ms jitter", &[format!("{v:.1}")]));
    let processing = stats
        .capture_processing_ms
        .map(|v| crate::i18n::text("capture processing {0} ms/frame", &[format!("{v:.1}")]));
    let conversion = stats
        .capture_conversion_ms
        .map(|v| crate::i18n::text("image conversion {0} ms/frame", &[format!("{v:.1}")]));
    let copy = stats
        .capture_copy_ms
        .map(|v| crate::i18n::text("image copy {0} ms/frame", &[format!("{v:.1}")]));
    let encoding = stats
        .encode_ms
        .map(|v| crate::i18n::text("encode {0} ms/frame", &[format!("{v:.1}")]));
    let callbacks = stats
        .capture_callbacks_fps
        .map(|v| crate::i18n::text("source callbacks {0} fps", &[format!("{v:.1}")]));
    let paced_out = stats
        .capture_paced_out
        .map(|v| crate::i18n::text("capture pacing skipped {0} total", &[v.to_string()]));
    let adapter_dropped = stats
        .adapter_dropped_frames
        .map(|v| crate::i18n::text("WebRTC adapter dropped {0} total", &[v.to_string()]));
    let reception: Vec<String> = stats.reception.as_ref().map_or_else(Vec::new, |r| {
        [
            ("decode {0} ms/frame", r.decode_ms),
            ("video dropped {0}%", r.dropped_pct),
            ("display dropped {0}%", r.render_dropped_pct),
            ("recent video loss {0}%", r.loss_pct),
            ("freezes {0}", r.freezes),
            ("frozen {0} ms", r.frozen_ms),
            ("video buffer {0} ms", r.buffer_ms),
            ("frame assembly {0} ms", r.assembly_ms),
        ]
        .into_iter()
        .filter_map(|(label, value)| value.map(|v| crate::i18n::text(label, &[format!("{v:.1}")])))
        .collect()
    });

    rsx! {
        div { class: "mt-1 border-t border-[var(--border)] pt-1.5 text-[10px] font-mono",
            div { class: "text-[var(--text-muted)] mb-0.5", {crate::i18n::text("Screen share {0}", &[direction_label.to_owned()])} }
            div { class: "flex flex-wrap items-baseline gap-x-2 gap-y-0.5 text-[var(--text-dim)]",
                if stats.outbound { span { "{capture_label}" } }
                if let Some(processing) = processing { span { "{processing}" } }
                if let Some(conversion) = conversion { span { "{conversion}" } }
                if let Some(copy) = copy { span { "{copy}" } }
                if let Some(encoding) = encoding { span { "{encoding}" } }
                if let Some(callbacks) = callbacks { span { "{callbacks}" } }
                if let Some(paced_out) = paced_out {
                    span { title: crate::i18n::text("Includes intentional skips to match the selected FPS", &[]), "{paced_out}" }
                }
                if let Some(adapter_dropped) = adapter_dropped { span { "{adapter_dropped}" } }
                span { "{encoded_label}" }
                span { "{codec}" }
                span { "{bitrate}" }
                if let Some(target) = target_bitrate { span { "{target}" } }
                span { "{codec_path_label}" }
                if !stats.outbound {
                    span { "{decoding_label}" }
                }
                span { "{efficiency_label}" }
                if stats.outbound { span { {crate::i18n::text("limit {0}", &[(limitation).to_string()])} } }
                span { "{frames_label}" }
                span { "{packets_label}" }
                if let Some(jitter) = jitter { span { "{jitter}" } }
            }
            if !stats.outbound && decoder_mode == "Not reported" {
                div { class: "text-[var(--text-muted)] mt-0.5",
                    {crate::i18n::text("The browser did not identify the active decoder as hardware or software. Missing data does not mean CPU decoding.", &[])}
                }
            }
            if !reception.is_empty() {
                div { class: "text-[var(--text-dim)] mt-0.5",
                    title: crate::i18n::label("Receiver measurements for the latest sample interval; missing counters are not estimated."),
                    {reception.join(" · ")}
                }
            }
            if let Some(error) = stats.error.as_deref() {
                div { class: "text-[var(--warn)] mt-0.5", {crate::i18n::text("Stats error: {0}", &[(error).to_string()])} }
            }
        }
    }
}

fn occupant_diagnostics_tooltip(
    health: Option<crate::state::ConnectionHealth>,
    stats: Option<crate::state::TrackStats>,
    ping_ms: Option<u32>,
    is_self: bool,
) -> String {
    let mut lines = Vec::new();
    let quality_desc = match health {
        Some(crate::state::ConnectionHealth::Excellent) => {
            crate::i18n::text("Connection: Excellent", &[])
        }
        Some(crate::state::ConnectionHealth::Good) => crate::i18n::text("Connection: Good", &[]),
        Some(crate::state::ConnectionHealth::Poor) => {
            if is_self {
                crate::i18n::text("Connection: Reduced (your audio)", &[])
            } else {
                crate::i18n::text("Connection: Reduced", &[])
            }
        }
        Some(crate::state::ConnectionHealth::Lost) => {
            crate::i18n::text("Connection: Disrupted", &[])
        }
        None => crate::i18n::text("Connection: Unavailable", &[]),
    };
    lines.push(quality_desc);

    if let Some(ping) = ping_ms {
        lines.push(crate::i18n::text(
            "Server ping: {0} ms",
            &[ping.to_string()],
        ));
    }

    match stats {
        Some(crate::state::TrackStats::Inbound {
            loss_pct,
            recent_loss_pct,
            jitter_ms,
            buffer_ms,
            concealed_pct,
            ..
        }) => {
            if let Some(recent) = recent_loss_pct {
                lines.push(crate::i18n::text(
                    "Loss: {0}% recent · {1}% total",
                    &[format!("{recent:.1}"), format!("{loss_pct:.1}")],
                ));
            } else {
                lines.push(crate::i18n::text("Loss: {0}%", &[format!("{loss_pct:.1}")]));
            }
            lines.push(crate::i18n::text(
                "Jitter: {0} ms",
                &[format!("{jitter_ms:.0}")],
            ));
            if buffer_ms > 0.0 {
                lines.push(crate::i18n::text(
                    "Buffer delay: {0} ms",
                    &[format!("{buffer_ms:.0}")],
                ));
            }
            if let Some(concealed) = concealed_pct.filter(|c| *c >= 0.5) {
                lines.push(crate::i18n::text(
                    "Concealed speech: {0}%",
                    &[format!("{concealed:.1}")],
                ));
            }
        }
        Some(crate::state::TrackStats::Outbound {
            bitrate_kbps,
            packets_per_sec,
            target_kbps,
            loss_pct,
        }) => {
            if let Some(loss) = loss_pct {
                lines.push(crate::i18n::text("Loss: {0}%", &[format!("{loss:.1}")]));
            }
            if let Some(kbps) = bitrate_kbps {
                lines.push(crate::i18n::text(
                    "Send: {0} / {1} kbps",
                    &[kbps.to_string(), target_kbps.to_string()],
                ));
            }
            if let Some(pps) = packets_per_sec {
                lines.push(crate::i18n::text("Packets: {0}/s", &[pps.to_string()]));
            }
        }
        _ => {}
    }
    lines.join("\n")
}

#[component]
fn VoiceSignal(icon: &'static str, color: &'static str, tooltip: String) -> Element {
    let mut shown = use_signal(|| false);
    let tooltip_id = use_hook(|| format!("voice-tooltip-{}", uuid::Uuid::new_v4()));
    rsx! {
        span {
            class: "relative inline-flex w-3.5 h-3.5 shrink-0 items-center justify-center {color} cursor-help opacity-70 hover:opacity-100 transition-opacity",
            role: "img",
            aria_label: "{tooltip}",
            onmouseenter: move |_| shown.set(true),
            onmouseleave: move |_| shown.set(false),
            span { class: "inline-flex", dangerous_inner_html: icon }
            if shown() {
                span {
                    id: "{tooltip_id}",
                    "popover": "manual",
                    role: "tooltip",
                    class: "pointer-events-none rounded-md bg-[var(--panel-solid)] border border-[var(--border)] shadow-xl px-2.5 py-1.5 text-left font-mono text-[11px] leading-relaxed text-[var(--text)] select-none",
                    style: "position:fixed;inset:auto;margin:0;width:max-content;max-width:calc(100vw - 16px);max-height:calc(100vh - 16px);overflow:auto;overflow-wrap:anywhere;white-space:normal;",
                    onmounted: move |_| {
                        let _ = document::eval(&format!("({})({tooltip_id:?})", include_str!("../../assets/voice-tooltip.js")));
                    },
                    for line in tooltip.lines() {
                        span { class: "block", "{line}" }
                    }
                }
            }
        }
    }
}

#[component]
pub(super) fn VoiceQualitySignal(
    health: Option<crate::state::ConnectionHealth>,
    stats: Option<crate::state::TrackStats>,
    ping_ms: Option<u32>,
    is_self: bool,
) -> Element {
    let (signal_icon, signal_color) = match health {
        Some(crate::state::ConnectionHealth::Excellent | crate::state::ConnectionHealth::Good) => {
            (crate::features::icons::SIGNAL_GOOD, "text-[var(--success)]")
        }
        Some(crate::state::ConnectionHealth::Poor) => {
            (crate::features::icons::SIGNAL_POOR, "text-[var(--warn)]")
        }
        Some(crate::state::ConnectionHealth::Lost) => {
            (crate::features::icons::SIGNAL_LOST, "text-[var(--danger)]")
        }
        None => (
            crate::features::icons::SIGNAL_GOOD,
            "text-[var(--text-dim)]",
        ),
    };
    let signal_tooltip = occupant_diagnostics_tooltip(health, stats, ping_ms, is_self);
    rsx! { VoiceSignal { icon: signal_icon, color: signal_color, tooltip: signal_tooltip } }
}

#[component]
fn VoiceOccupant(
    channel_id: Id,
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
    let can_move = {
        let s = state.read();
        s.channels
            .iter()
            .find(|c| c.id == channel_id)
            .is_some_and(|c| {
                !is_self
                    && s.can(c.guild_id, Permission::MoveMembers)
                    && !s
                        .guilds
                        .iter()
                        .any(|g| g.id == c.guild_id && g.owner_pubkey == pubkey)
            })
    };

    let volume = state
        .read()
        .user_volumes
        .get(&pubkey)
        .copied()
        .unwrap_or(100);
    let locally_muted = state.read().user_muted.contains(&pubkey);
    let same_channel = state.read().connected_voice_channel() == Some(channel_id);
    let health = state
        .read()
        .voice_quality
        .get(&pubkey)
        .copied()
        .filter(|_| same_channel);
    let track_stats = state
        .read()
        .voice_stats
        .get(&pubkey)
        .copied()
        .filter(|_| same_channel);
    let ring = crate::state::talk_ring(same_channel && speaking && !locally_muted);
    // Volume, local mute and disconnect live in the right-click menu, mounted
    // at the workspace root (trap 22); the row only shows what is set.
    let open_menu = {
        let pubkey = pubkey.clone();
        let name = name.clone();
        move |e: MouseEvent| {
            if is_self {
                return;
            }
            e.prevent_default();
            e.stop_propagation();
            let c = e.client_coordinates();
            state.write().voice_menu = Some(crate::state::VoiceMenu {
                pubkey: pubkey.clone(),
                name: name.clone(),
                x: c.x,
                y: c.y,
            });
        }
    };
    let open_menu_click = open_menu.clone();
    let pk_watch = pubkey.clone();
    let pk_camera = pubkey.clone();
    let pk_card = pubkey.clone();
    let is_watching_screen = state.read().screen_viewing.contains(&pubkey);
    let is_watching_camera = state.read().cameras_watching.contains(&pubkey);

    rsx! {
        div {
            class: "px-2 py-0.5 rounded cursor-pointer select-none hover:bg-white/[0.03]",
            "data-voice-pubkey": "{pubkey}",
            "data-voice-from": "{channel_id}",
            "data-voice-name": "{name}",
            "data-voice-movable": if can_move { "true" } else { "false" },
            onclick: move |_| state.write().profile_card = Some(pk_card.clone()),
            oncontextmenu: open_menu,
            div { class: "flex items-center gap-1.5 text-xs text-[var(--text-muted)]",
                span { class: "shrink-0 flex",
                    crate::features::profiles::Avatar {
                        pubkey: pubkey.clone(),
                        name: name.clone(),
                        size: "w-5 h-5 text-[9px] {ring}",
                    }
                }
                span { class: "truncate flex-1",
                    "{name}"
                    if is_self { {crate::i18n::text(" (you)", &[])} }
                }
                if same_channel {
                    VoiceQualitySignal { health, stats: track_stats, ping_ms: state.read().voice_ping_ms, is_self }
                }
                crate::features::soundboard::SoundChip { pubkey: pubkey.clone() }
                if remote_deafened {
                    span {
                        class: "inline-flex w-4 h-4 shrink-0 items-center justify-center text-[var(--danger)]",
                        role: "img",
                        title: crate::i18n::text("deafened", &[]),
                        aria_label: crate::i18n::text("deafened", &[]),
                        dangerous_inner_html: crate::features::icons::HEADPHONES_OFF,
                    }
                } else if remote_muted {
                    span {
                        class: "inline-flex w-4 h-4 shrink-0 items-center justify-center text-[var(--danger)]",
                        role: "img",
                        title: crate::i18n::text("Microphone muted", &[]),
                        aria_label: crate::i18n::text("Microphone muted", &[]),
                        dangerous_inner_html: crate::features::icons::MIC_OFF,
                    }
                }
                if !is_self && (locally_muted || volume != 100) {
                    button {
                        class: if locally_muted {
                            "w-5 h-5 flex items-center justify-center rounded text-[var(--danger)] shrink-0"
                        } else {
                            "w-5 h-5 flex items-center justify-center rounded text-[var(--accent)] shrink-0"
                        },
                        title: if locally_muted { crate::i18n::text("Muted for you — click for options", &[]) } else { crate::i18n::text("{0}% for you — click for options", &[(volume).to_string()]) },
                        onclick: open_menu_click,
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
                            crate::i18n::text("Sharing their screen", &[])
                        } else if is_watching_screen {
                            crate::i18n::text("Stop watching their screen", &[])
                        } else {
                            crate::i18n::text("Watch their screen", &[])
                        },
                        onclick: move |e: MouseEvent| {
                            e.stop_propagation();
                            if can_watch {
                                let mut s = state.write();
                                if !s.screen_viewing.remove(&pk_watch) {
                                    s.screen_viewing.insert(pk_watch.clone());
                                }
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
                            crate::i18n::text("Their camera is on — join the channel to watch", &[])
                        } else if is_watching_camera {
                            crate::i18n::text("Hide their camera", &[])
                        } else {
                            crate::i18n::text("Watch their camera", &[])
                        },
                        onclick: move |e: MouseEvent| {
                            e.stop_propagation();
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
        let rules = s
            .selected_guild
            .map(|gid| s.leveling_of(gid))
            .unwrap_or_default();
        let progress = crate::features::leveling::progress(&rules, xp);
        (progress.label, progress.pct)
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
        let where_name = ch
            .map(|c| c.name.clone())
            .unwrap_or_else(|| crate::i18n::text("Voice", &[]));
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
        (
            where_name,
            crate::i18n::text("{0} · {1} connected", &[guild, n.to_string()]),
        )
    };

    let show_banner = !matches!(self_voice.phase, VoicePhase::Idle);
    let (dot_color, phase_text) = match self_voice.phase {
        VoicePhase::Idle => ("var(--text-dim)", "voice idle"),
        VoicePhase::Connecting => ("var(--warn)", "Connecting…"),
        VoicePhase::Connected => ("var(--success)", "voice connected"),
        VoicePhase::Error => ("var(--danger)", "voice error"),
    };
    let voice_error = self_voice.error.clone();

    let muted = self_voice.muted;
    let deafened = self_voice.deafened;
    let mute_label = if muted { "Unmute mic" } else { "Mute mic" };
    let g_for_mute = gateway.clone();
    let v_for_mute = voice.clone();
    let g_for_deafen = gateway.clone();
    let v_for_deafen = voice.clone();
    let g_for_hang = gateway.clone();
    let v_for_hang = voice.clone();
    let g_for_share = gateway.clone();
    let g_for_camera = gateway.clone();
    let voice_channel = self_voice.channel_id;
    let soundboard_open = state.read().soundboard_open;

    // The trigger lives in the title bar; this panel only owns the device
    // signals the popover needs, so the flag travels through AppState.
    let show_audio_settings = use_memo(move || state.read().audio_settings);
    {
        let v_audio = voice.clone();
        use_effect(move || {
            if show_audio_settings() {
                v_audio.send(crate::features::voice::VoiceCmd::ListDevices);
                crate::features::camera::refresh_cameras(state);
            }
        });
    }
    {
        let v_stats = voice.clone();
        use_effect(move || {
            let active = state.read().connected_voice_channel().is_some();
            v_stats.send(crate::features::voice::VoiceCmd::SetStatsPolling { enabled: active });
        });
    }

    // Either one moves the level the threshold is then judged against, and the
    // AGC moves it *up*, so the old `pre > post` test could never fire for it.

    rsx! {
        div { class: "border-t border-[var(--border)]",
            if show_banner {
                div { class: "px-3 pt-2.5 pb-2 border-b border-[var(--border)] grid grid-cols-[minmax(0,1fr)_auto] items-center gap-x-2 gap-y-1",
                    div { class: "min-w-0 flex items-center gap-2",
                        span {
                            class: "w-2 h-2 rounded-full shrink-0",
                            style: "background-color:{dot_color};",
                            title: crate::i18n::label(phase_text),
                        }
                        span {
                            class: "text-[12.5px] font-semibold truncate",
                            style: "color:{dot_color};",
                            title: "{voice_where}",
                            "{voice_where}"
                        }
                        span { class: "shrink-0 font-mono text-[10.5px] text-[var(--text-dim)]",
                            if let Some(ping) = state.read().voice_ping_ms {
                                {crate::i18n::text("{0} ms · {1} kbps", &[ping.to_string(), (voice_bitrate_kbps).to_string()])}
                            } else {
                                {crate::i18n::text("{0} kbps", &[(voice_bitrate_kbps).to_string()])}
                            }
                        }
                    }
                    div { class: "min-w-0 text-[11px] text-[var(--text-dim)] truncate", title: "{voice_sub}", "{voice_sub}" }
                    button {
                        class: "col-start-2 row-start-1 row-span-2 justify-self-end h-9 px-2.5 flex items-center justify-center gap-1.5 rounded-lg border text-[11.5px] font-semibold text-[var(--danger)] transition-colors hover:brightness-125",
                        style: "background-color: color-mix(in srgb, var(--danger) 10%, transparent); border-color: color-mix(in srgb, var(--danger) 35%, transparent);",
                        title: crate::i18n::text("Leave voice", &[]),
                        onclick: move |_| {
                            g_for_hang.send(ClientMessage::LeaveVoice);
                            v_for_hang.send(VoiceCmd::Disconnect { done: None });
                        },
                        span { class: "block w-4 h-4", dangerous_inner_html: crate::features::icons::PHONE_HANGUP }
                        {crate::i18n::text("Leave", &[])}
                    }
                }
            }
            // Always present, so you can arrive already muted; the share and
            // camera chips join it only once there is a call to point them at.
            div { class: "dxf-channel-audio px-3 py-2 flex items-center gap-1.5",
                    button {
                        class: if muted {
                            "flex-1 h-9 flex items-center justify-center rounded-lg border transition-colors border-[var(--danger)] text-[var(--danger)]"
                        } else {
                            "flex-1 h-9 flex items-center justify-center rounded-lg border transition-colors border-[var(--border)] text-[var(--text-muted)] hover:text-[var(--text)] hover:border-[var(--border-strong)]"
                        },
                        title: crate::i18n::label(mute_label),
                        onclick: move |_| {
                            let new_muted = !muted || deafened;
                            state.write().voice.muted = new_muted;
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
                        title: if deafened { crate::i18n::text("undeafen", &[]) } else { crate::i18n::text("deafen", &[]) },
                        onclick: move |_| {
                            let (next_muted, next_deafened) = state.write().voice.toggle_deafen();
                            {
                                let mut s = state.write();
                                s.voice.muted = next_muted;
                                s.voice.deafened = next_deafened;
                            }
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
                                crate::i18n::text("This build's webview has no camera support.", &[])
                            } else if camera_starting {
                                crate::i18n::text("Starting your camera…", &[])
                            } else if camera_on { crate::i18n::text("Turn your camera off", &[]) } else { crate::i18n::text("Turn your camera on", &[]) },
                            // While it is still starting, a press cancels it.
                            onclick: move |_| {
                                if camera_on || camera_starting {
                                    crate::features::camera::toggle_camera(state, settings, &g_for_camera, false);
                                } else {
                                    state.write().camera_picker = true;
                                }
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
                                crate::i18n::text("Screen capture isn't available in this webview — pressing this will explain why.", &[])
                            } else if sharing { crate::i18n::text("Stop sharing your screen", &[]) } else { crate::i18n::text("Share your screen", &[]) },
                            onclick: move |_| {
                                let now = !sharing;
                                if !now {
                                    state.write().screen_sharing = false;
                                    if let Some(cid) = voice_channel {
                                        g_for_share.send(ClientMessage::SetScreenShare { channel_id: cid, sharing: false });
                                    }
                                }

                                                            if native_capture {
                                                                if now && state.peek().screen_video_token.is_none() {
                                                                    state.write().error_toast = Some(
                                                                        "This server is too old to accept a natively \
                                                                         captured screen share.".into()
                                                                    );
                                                                } else if now {
                                                                    crate::features::screenshare::open_screen_picker(state, g_for_share.clone(), settings);
                                                                } else {
                                                                    let mut s = state.write();
                                                                    s.screen_native_audio = false;
                                                                    s.screen_share_target = None;
                                                                }
                                                            } else if now {
                                                                let quality = settings.read().screenshare_quality.clone();
                                                                let audio = settings.read().screenshare_audio;
                                                                let _ = document::eval(&crate::features::screenshare::share_js(true, &quality, audio));
                                                            } else {
                                                                let _ = document::eval(&crate::features::screenshare::share_js(false, "", true));
                                                            }
                                                        },
                            dangerous_inner_html: crate::features::icons::SCREEN,
                        }
                        button {
                            class: if soundboard_open {
                                "[&>svg]:pointer-events-none flex-1 h-9 flex items-center justify-center rounded-lg border transition-colors border-[var(--accent)] bg-[var(--accent-soft)] text-[var(--accent)]"
                            } else {
                                "[&>svg]:pointer-events-none flex-1 h-9 flex items-center justify-center rounded-lg border transition-colors border-[var(--border)] text-[var(--text-muted)] hover:text-[var(--text)] hover:border-[var(--border-strong)]"
                            },
                            id: "dxf-soundboard-toggle",
                            onpointerdown: move |e| e.stop_propagation(),
                            title: crate::i18n::text("Soundboard — right-click for its volume", &[]),
                            onclick: move |e: MouseEvent| {
                                let at = e.client_coordinates();
                                let inside = e.element_coordinates();
                                let mut s = state.write();
                                s.soundboard_anchor = (at.x, at.y - inside.y);
                                s.soundboard_open = !(s.soundboard_open && !s.soundboard_adjusting);
                                s.soundboard_adjusting = false;
                            },
                            oncontextmenu: move |e: MouseEvent| {
                                e.prevent_default();
                                let at = e.client_coordinates();
                                let inside = e.element_coordinates();
                                let mut s = state.write();
                                s.soundboard_anchor = (at.x, at.y - inside.y);
                                s.soundboard_open = !(s.soundboard_open && s.soundboard_adjusting);
                                s.soundboard_adjusting = true;
                            },
                            dangerous_inner_html: crate::features::icons::SOUNDBOARD,
                        }
                    }
            }
            if let Some(err) = voice_error {
                div { class: "px-3 pb-2 text-[10px] text-[var(--danger)] break-all",
                    {crate::i18n::message(&(err).to_string())}
                }
            }
            div { class: "dxf-channel-account h-14 px-2 flex items-center gap-1 border-t border-[var(--border)]",
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
                            span { class: "text-[9px] font-semibold text-[var(--text-dim)] shrink-0", "{level}" }
                        }
                        // The key under the name: this is the account, and the
                        // account is the key (trap 12).
                        span { class: "block font-mono text-[10px] text-[var(--text-dim)] truncate",
                            title: "{self_npub}",
                            "{self_npub_short}"
                        }
                        div { class: "mt-1 h-0.5 rounded-full overflow-hidden", style: "background-color: var(--bg2);",
                            div {
                                class: "h-full rounded-full",
                                style: "width: {xp_pct}%; background-image: linear-gradient(90deg, #8fb0ff, var(--accent));",
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
    if let Some(mut tab) = try_consume_context::<Signal<super::workspace::MobileTab>>() {
        tab.set(super::workspace::MobileTab::Chat);
    }
    let needs_fetch = {
        let mut s = state.write();
        s.selected_channel = Some(channel_id);
        s.dm_mode = false;
        crate::channel_selection::remember_current(&mut s);
        s.mark_channel_read(channel_id);
        !s.history_loaded.contains(&channel_id)
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
    if let Some(mut tab) = try_consume_context::<Signal<super::workspace::MobileTab>>() {
        tab.set(super::workspace::MobileTab::Chat);
    }
    let needs_fetch = {
        let mut s = state.write();
        s.dm_mode = true;
        s.selected_channel = Some(channel_id);
        s.request_composer_focus(channel_id);
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
#[path = "voice_drag_tests.rs"]
mod voice_drag_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn chan(name: &str, position: u32) -> Channel {
        kinded(name, position, ChannelKind::Text)
    }

    fn kinded(name: &str, position: u32, kind: ChannelKind) -> Channel {
        Channel {
            divider: Default::default(),
            id: Id::new_v4(),
            guild_id: Id::nil(),
            name: name.into(),
            kind,
            topic: None,
            read_only: false,
            position,
            access: None,
            slowmode_secs: 0,
            markdown: false,
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
    fn any_kind_can_be_dropped_among_any_other() {
        let guild = vec![
            kinded("general", 0, ChannelKind::Text),
            kinded("Lobby", 1, ChannelKind::Voice),
            kinded("Games", 2, ChannelKind::Divider),
            kinded("Raid", 3, ChannelKind::Voice),
        ];
        let updates = reorder_positions(&guild, guild[3].id, guild[0].id);
        assert_eq!(
            applied(&guild, &updates),
            ["Raid", "general", "Lobby", "Games"]
        );
        let updates = reorder_positions(&guild, guild[2].id, guild[0].id);
        assert_eq!(
            applied(&guild, &updates),
            ["Games", "general", "Lobby", "Raid"]
        );
        assert_eq!(
            drop_edge(&guild, guild[0].id, guild[1].id),
            Some(Edge::After)
        );
    }

    #[test]
    fn the_drop_line_sits_where_the_row_will_land() {
        let g = group(&[("a", 0), ("b", 1), ("c", 2), ("d", 3)]);
        for from in 0..g.len() {
            for to in 0..g.len() {
                let edge = drop_edge(&g, g[from].id, g[to].id);
                if from == to {
                    assert_eq!(edge, None);
                    continue;
                }
                let order = applied(&g, &reorder_positions(&g, g[from].id, g[to].id));
                let moved = order.iter().position(|n| *n == g[from].name).unwrap();
                let target = order.iter().position(|n| *n == g[to].name).unwrap();
                let expected = if moved < target {
                    Edge::Before
                } else {
                    Edge::After
                };
                assert_eq!(edge, Some(expected), "{} onto {}", g[from].name, g[to].name);
            }
        }
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

    #[test]
    fn occupant_diagnostics_tooltip_formats_inbound_stats() {
        let text = occupant_diagnostics_tooltip(
            Some(crate::state::ConnectionHealth::Good),
            Some(crate::state::TrackStats::Inbound {
                loss_pct: 1.5,
                recent_loss_pct: Some(0.8),
                discarded_packets: None,
                concealed_pct: Some(1.2),
                jitter_ms: 24.0,
                buffer_ms: 12.0,
                concealment_events: 0,
            }),
            Some(32),
            false,
        );
        assert!(text.contains("Connection: Good"));
        assert!(text.contains("Server ping: 32 ms"));
        assert!(text.contains("Loss: 0.8% recent · 1.5% total"));
        assert!(text.contains("Jitter: 24 ms"));
        assert!(text.contains("Buffer delay: 12 ms"));
        assert!(text.contains("Concealed speech: 1.2%"));
    }

    #[test]
    fn occupant_diagnostics_tooltip_formats_outbound_stats() {
        let text = occupant_diagnostics_tooltip(
            Some(crate::state::ConnectionHealth::Excellent),
            Some(crate::state::TrackStats::Outbound {
                bitrate_kbps: Some(64),
                packets_per_sec: Some(50),
                target_kbps: 64,
                loss_pct: Some(0.0),
            }),
            Some(18),
            true,
        );
        assert!(text.contains("Connection: Excellent"));
        assert!(text.contains("Server ping: 18 ms"));
        assert!(text.contains("Loss: 0.0%"));
        assert!(text.contains("Send: 64 / 64 kbps"));
        assert!(text.contains("Packets: 50/s"));
    }

    #[test]
    fn occupant_diagnostics_tooltip_self_reduced() {
        let text = occupant_diagnostics_tooltip(
            Some(crate::state::ConnectionHealth::Poor),
            None,
            None,
            true,
        );
        assert!(text.contains("Connection: Reduced (your audio)"));
    }

    #[test]
    fn signal_updates_leave_the_open_tooltip_outside_the_svg_leaf() {
        use dioxus::core::{AttributeValue, ElementId, Mutation};
        use std::{any::Any, cell::RefCell, rc::Rc};

        type Export = Rc<RefCell<Option<Signal<bool>>>>;
        dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
        let export: Export = Rc::new(RefCell::new(None));
        let mut dom = VirtualDom::new_with_props(
            |export: Export| {
                let lost = use_signal(|| false);
                *export.borrow_mut() = Some(lost);
                let (icon, color, tooltip) = if lost() {
                    (super::super::icons::SIGNAL_LOST, "red", "Disrupted")
                } else {
                    (super::super::icons::SIGNAL_GOOD, "green", "Healthy")
                };
                rsx! { VoiceSignal { icon, color, tooltip } }
            },
            export.clone(),
        );
        let initial = dom.rebuild_to_vec().edits;
        let hover = initial
            .iter()
            .find_map(|edit| match edit {
                Mutation::NewEventListener { name, id } if name == "mouseenter" => Some(*id),
                _ => None,
            })
            .unwrap();
        let icon = initial
            .iter()
            .find_map(|edit| match edit {
                Mutation::SetAttribute {
                    name: "dangerous_inner_html",
                    id,
                    ..
                } => Some(*id),
                _ => None,
            })
            .unwrap();
        assert_ne!(
            hover, icon,
            "rewriting the SVG must not replace the hover container's children"
        );
        fn mouse(dom: &mut VirtualDom, name: &str, id: ElementId) -> Vec<Mutation> {
            let data: Rc<dyn Any> = Rc::new(dioxus::html::PlatformEventData::new(Box::new(
                dioxus::html::SerializedMouseData::default(),
            )));
            dom.runtime()
                .handle_event(name, Event::new(data, false), id);
            dom.render_immediate_to_vec().edits
        }
        let shown = mouse(&mut dom, "mouseenter", hover);
        assert!(shown.iter().any(|edit| matches!(
            edit,
            Mutation::SetText { value, .. } | Mutation::CreateTextNode { value, .. } if value == "Healthy"
        )));
        let mut lost = export.borrow().unwrap();
        lost.set(true);
        let changed = dom.render_immediate_to_vec().edits;
        assert!(changed.iter().any(|edit| matches!(edit,
            Mutation::SetAttribute { name: "dangerous_inner_html", value: AttributeValue::Text(svg), id, .. }
                if *id == icon && *svg == super::super::icons::SIGNAL_LOST
        )));
        assert!(
            changed.iter().any(|edit| matches!(
                edit,
                Mutation::SetText { value, .. } if value == "Disrupted"
            )),
            "the mounted tooltip still receives its text update"
        );
        assert!(
            mouse(&mut dom, "mouseleave", hover)
                .iter()
                .any(|edit| matches!(edit, Mutation::Remove { .. } | Mutation::ReplaceWith { .. }))
        );
        assert!(!mouse(&mut dom, "mouseenter", hover).is_empty());
    }

    #[test]
    fn diagnostics_do_not_claim_healthy_quality_or_zero_loss_without_feedback() {
        let text = occupant_diagnostics_tooltip(
            None,
            Some(crate::state::TrackStats::Outbound {
                bitrate_kbps: None,
                packets_per_sec: None,
                target_kbps: 64,
                loss_pct: None,
            }),
            None,
            true,
        );
        assert!(text.contains("Connection: Unavailable"));
        assert!(!text.contains("Loss:"));
        assert!(!text.contains("ping"));
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
                if let Some(mut tab) = try_consume_context::<Signal<super::workspace::MobileTab>>()
                {
                    tab.set(super::workspace::MobileTab::Chat);
                }
            }
            Err(e) => error.set(Some(e)),
        }
    };

    rsx! {
        div { class: "dxf-dm-key-entry px-1 pb-2 space-y-1",
            input {
                class: "w-full bg-[var(--bg2)] border border-[var(--border)] rounded px-2 py-1 text-xs outline-none focus:border-[var(--accent)]",
                r#type: "text",
                placeholder: crate::i18n::text("npub1… or hex key", &[]),
                value: "{input}",
                oninput: move |e| { input.set(e.value()); error.set(None); },
                onkeydown: move |e| {
                    if e.key() == Key::Enter { start(); }
                },
            }
            if let Some(err) = error() {
                div { class: "px-1 text-[10px] text-[var(--danger,#f87171)]", {crate::i18n::message(&(err).to_string())} }
            }
        }
    }
}
