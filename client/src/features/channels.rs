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

    let mut chan_menu = use_signal::<Option<ChanMenu>>(|| None);
    let mut show_create = use_signal(|| false);

    // The guild banner had nowhere to render at all — uploading one changed a
    // database row and nothing else. Here is its home: a strip across the top
    // of the channel list, which is the surface that already belongs to the
    // guild you're looking at.
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
                    // Scrim so the name below stays readable over a busy image.
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
                                    // Watching needs you in the channel (the JS
                                    // screen room only connects then), and you
                                    // can't watch your own share.
                                    can_watch: is_sharing && connected && !is_self,
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A measured rate, or an em dash while there is nothing to measure against.
///
/// The outbound numbers are deltas between two readings, so the tick that opens
/// the panel has nothing to divide. A dash says "measuring"; a zero would say
/// "sending nothing", which is the opposite.
fn rate_or_dash(v: Option<u32>, unit: &str) -> String {
    match v {
        Some(n) => format!("{n} {unit}"),
        None => format!("— {unit}"),
    }
}

/// The numbers behind "it sounds bad", for whoever is in the call.
///
/// Mounting is the subscription: polling the peer connection costs a walk of
/// every track every second, so it runs only while this is on screen. Tying
/// that to mount/unmount rather than to the toggle's own handler is what makes
/// closing the whole popover — or the call ending under it — stop the poll
/// too, without every close path having to remember.
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

    // Sorted by name: a HashMap's order is arbitrary and would reshuffle the
    // rows under the reader's eyes on every tick.
    let mut rows: Vec<(String, crate::state::TrackStats)> = {
        let s = state.read();
        s.voice_stats
            .iter()
            .map(|(pk, st)| {
                let name = s
                    .user_of(pk)
                    .map(|u| u.username.clone())
                    .unwrap_or_else(|| crate::identity::truncate_pubkey(pk));
                (name, *st)
            })
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
                                // Payload only — RTP headers add a few kbit/s on
                                // top — so this says what the encoder produced,
                                // which is the question the row is for. The aim
                                // rides along because it is the only half still
                                // readable while the transmit gate is shut.
                                //
                                // And it names RED, because the measurement
                                // reading at roughly twice the aim is the
                                // tooltip's own doing otherwise: the mic track
                                // publishes with the SDK's `red` default, which
                                // carries a copy of the previous frame in every
                                // packet. Without that sentence the row looks
                                // like the bitrate setting did not take.
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

/// One participant in a voice channel's roster, with the local audio controls
/// for them: a volume slider and a local mute.
///
/// Everything here is *listener-side*. The gain is applied to their incoming
/// stream in our own playback mixer (`features::voice`), so turning someone
/// down changes only what this machine plays — it never touches their
/// microphone, is never sent to the server, and no other listener sees it.
/// That is also why local mute is kept separate from `remote_muted`, the
/// speaker's own mute state pushed by the server.
#[component]
fn VoiceOccupant(
    pubkey: String,
    name: String,
    speaking: bool,
    remote_muted: bool,
    remote_deafened: bool,
    is_self: bool,
    is_sharing: bool,
    can_watch: bool,
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
    // Only rendered when the SFU says something is wrong: a dot on every name
    // in a healthy call is noise, and trains people to ignore the one that
    // matters. `None` covers both "fine" and "no reading yet".
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
    // Push the current setting down to the mixer, reading it back through the
    // same accessor the rest of the app uses so the slider, the mute button and
    // what's actually playing can't drift apart. Call it *after* writing state.
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

    rsx! {
        div { class: "px-2 py-0.5",
            div { class: "flex items-center gap-1.5 text-xs text-[var(--text-muted)]",
                span { class: "w-1.5 h-1.5 rounded-full shrink-0 {dot}" }
                span { class: "truncate flex-1",
                    "{name}"
                    if is_self { " (you)" }
                }
                // Deafened subsumes muted — the server forces mute on with it —
                // so showing both would be permanent noise in a narrow column.
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
                // No volume control for yourself: your own voice is never
                // played back, so there'd be nothing for it to change.
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
                        class: "flex items-center gap-1 text-[9px] uppercase tracking-wider text-[var(--danger)] font-semibold disabled:opacity-70 disabled:cursor-default",
                        disabled: !can_watch,
                        title: if can_watch { "Watch screen" } else { "Sharing screen" },
                        onclick: move |_| {
                            if can_watch {
                                state.write().screen_viewing = Some(pk_watch.clone());
                            }
                        },
                        span { class: "w-1.5 h-1.5 rounded-full bg-[var(--danger)] dxf-dot-pulse", style: "color:var(--danger);" }
                        "live"
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
    // Whether a screen can be captured at all, by either path; populated by the
    // ScreenShareBridge.
    let screen_capture_available = state.read().screen_capture_available;
    // Which path captures here. On macOS the webview has no `getDisplayMedia` to
    // call, so the share is driven from Rust via `sysvideo` instead of by
    // evaluating JS in the webview — and unlike the JS path it needs no user
    // gesture, because there is no browser permission prompt in it.
    let native_capture = crate::sysvideo::supported();
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

    // UI state for audio settings popover. The popover is a freely-draggable
    // floating window (not anchored to the gear button), so we track pixel
    // position + an in-flight drag. Initial coords approximate the old
    // `right-3 top-12` anchor in a ~1280px viewport; the user moves it from
    // there. Position persists across open/close within the session.
    let mut show_audio_settings = use_signal(|| false);
    // Not persisted: reopening the app should not silently resume polling.
    let mut show_stats = use_signal(|| false);
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
    let noise_cancellation = state.read().noise_cancellation;
    let atten_lim_db = state.read().denoise_atten_lim_db;
    let mic_volume = state.read().mic_volume;
    let auto_gain_control = state.read().auto_gain_control;
    let voice_bitrate_kbps = state.read().voice_bitrate_kbps;
    // Whether the transmit gate is currently open — the very same flag the
    // publish path acts on, so this can't claim something the audio isn't doing.
    let gate_open = self_voice.speaking;

    // Clone voice sender for each closure so move into one closure doesn't
    // prevent reuse in others.
    let v_for_audio_button = voice.clone();
    let v_for_input_change = voice.clone();
    let v_for_output_change = voice.clone();
    let v_for_sensitivity = voice.clone();
    let v_for_denoise = voice.clone();
    let v_for_atten = voice.clone();
    let v_for_mic_volume = voice.clone();
    let v_for_agc = voice.clone();
    let v_for_bitrate = voice.clone();

    // Snapshot current voice phase so the popover can show reconnection state.
    let voice_phase = state.read().voice.phase;
    let reconnecting = matches!(voice_phase, VoicePhase::Connecting);
    // VU bar + threshold marker, both on a dB scale. A linear amplitude meter
    // reads 3-30% for ordinary speech and made the default threshold display as
    // "2%", which looks broken even when the audio is fine.
    let mic_level_pct = crate::features::voice::peak_to_meter_pct(mic_level);
    let threshold_pct = crate::features::voice::peak_to_meter_pct(mic_sensitivity);
    let sensitivity_display = crate::features::voice::peak_to_db_label(mic_sensitivity);
    let mic_level_display = crate::features::voice::peak_to_db_label(mic_level);
    // Screen-share preset lives only in local settings — it's a capture-side
    // choice the server never sees.
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
                        // Screen share toggle. Calls getDisplayMedia inside this
                        // click gesture (must not be deferred to an effect).
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
                                // Starting is NOT announced here. The capture
                                // may still be cancelled at the picker, and
                                // claiming to share before a track exists meant
                                // the button lit up, the self-preview mounted,
                                // and everyone in the channel saw "live" for a
                                // share that never happened. The JS reports
                                // `share-started` once a track is published;
                                // stopping is immediate and stays here.
                                if !now {
                                    state.write().screen_sharing = false;
                                    if let Some(cid) = voice_channel {
                                        g_for_share.send(ClientMessage::SetScreenShare { channel_id: cid, sharing: false });
                                    }
                                }

                                                            let q = settings.read().screenshare_quality.clone();
                                                            let a = settings.read().screenshare_audio;
                                                            if native_capture {
                                                                // Native path: the button only opens the picker, which
                                                                // owns starting the share (it is the thing that knows
                                                                // *what* to capture). Publishing itself belongs to the
                                                                // effect in `ScreenShareBridge`, so a voice-session
                                                                // restart mid-share re-issues it rather than leaving the
                                                                // share quietly dead.
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
                                                                    // Forget the surface, so the next share opens the
                                                                    // picker rather than silently reusing whatever was
                                                                    // shared last time.
                                                                    s.screen_share_target = None;
                                                                }
                                                            } else if now {
                                                                // Execute the user-gesture prompt + start helper from the
                                                                // screenshare module. This calls requestAndStartShare()
                                                                // which prompts for getDisplayMedia inside the click.
                                                                // If the feature is unavailable the button is disabled and
                                                                // this branch won't run.
                                                                let _ = document::eval(&crate::features::screenshare::share_js(true, &q, a));
                                                            } else {
                                                                // Turning off — stop immediately.
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

                // Settings button — opens a small popover, grouped by what each
                // knob acts on: playback, capture, and what goes on the wire.
                button {
                    class: "w-7 h-7 flex items-center justify-center rounded text-[var(--text-muted)] hover:text-[var(--accent)] transition-colors",
                    title: "Settings",
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
                        // The cap has to be computed, not a fixed `max-h-[80vh]`:
                        // the window is draggable, so the room it has is the
                        // distance from wherever it currently sits to the bottom
                        // of the screen. Without it the popover just grew past
                        // the edge and the last section became unreachable —
                        // there is nowhere to scroll a `fixed` box.
                        style: "left: {audio_x}px; top: {audio_y}px; max-height: calc(100vh - {audio_y}px - 12px);",
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
                            span { class: "text-[11px] font-medium text-[var(--text)] flex-1", "Settings" }
                            button {
                                class: "text-[var(--text-dim)] hover:text-[var(--text)] text-base leading-none",
                                onmousedown: move |e| e.stop_propagation(),
                                onclick: move |_| show_audio_settings.set(false),
                                "✕"
                            }
                        }

                        // Scrolls while the drag header stays put — same shape as
                        // the channel list above (`flex flex-col overflow-hidden`
                        // parent, `flex-1 overflow-y-auto` child).
                        div { class: "p-2 flex-1 overflow-y-auto",
                            // Reconnection indicator
                            if reconnecting {
                                div { class: "mb-2 flex items-center text-[12px] text-[var(--text-muted)]",
                                    span { class: "dx-spinner" }
                                    span { "Reconnecting audio…" }
                                }
                            }

                            // ---- Audio ----
                            div { class: "mt-3 mb-1.5 pb-1 border-b border-[var(--border)]",
                                span { class: "text-[10px] font-semibold uppercase tracking-wide text-[var(--text)]", "Audio" }
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
                                        v.send(crate::features::voice::VoiceCmd::SetDevices { input: None, output: s.selected_output_device.clone() });
                                    },
                                    option { value: "", "System default" }
                                    for dev in available_output_devices.iter() {
                                        option { selected: selected_output_device.as_ref().map(|n| n == dev).unwrap_or(false), value: "{dev}", "{dev}" }
                                    }
                                }
                            }
                            // Sound effects volume. Controls all synthesized UI
                            // cues (connect, disconnect, mute, peer events, etc.).
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
                                            settings.set(next.clone());
                                            crate::settings::save(&next);
                                            // Apply immediately to the live SFX engine.
                                            let v = val as f32 / 100.0;
                                            let _ = document::eval(&format!(
                                                "window.dxSfx && window.dxSfx.setVolume({v});"
                                            ));
                                        },
                                    }
                                    span { class: "text-[10px] text-[var(--text-dim)] w-8 text-right", "{settings.read().sfx_volume}%" }
                                }
                            }
                            // ---- Microphone ----
                            div { class: "mt-3 mb-1.5 pb-1 border-b border-[var(--border)]",
                                span { class: "text-[10px] font-semibold uppercase tracking-wide text-[var(--text)]", "Microphone" }
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
                                        v.send(crate::features::voice::VoiceCmd::SetDevices { input: s.selected_input_device.clone(), output: None });
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
                                        span { class: "text-[10px] text-[var(--text-dim)]", "{mic_level_display}" }
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
                            // Microphone input volume. Applied before the meter
                            // and the gate, so the VU bar above moves with it —
                            // which is the point: you set the level by watching
                            // where speech lands on the bar, then set the
                            // threshold underneath it.
                            div { class: "mb-2",
                                div { class: "flex items-center justify-between",
                                    span { class: "text-[11px] text-[var(--text-muted)]", "Microphone Input" }
                                    span { class: "text-[10px] text-[var(--text-dim)]", "{mic_volume}%" }
                                }
                                input {
                                    r#type: "range",
                                    min: "0",
                                    // Above unity so a quiet mic can be rescued
                                    // without reaching for the OS mixer.
                                    max: "200",
                                    value: "{mic_volume}",
                                    class: "w-full mt-1 accent-[var(--accent)]",
                                    oninput: move |e| {
                                        let pct: u16 = e.value().parse().unwrap_or(100).min(200);
                                        let mut next = settings.read().clone();
                                        next.mic_volume = pct;
                                        settings.set(next.clone());
                                        crate::settings::save(&next);
                                        state.write().mic_volume = pct;
                                        let v = v_for_mic_volume.clone();
                                        v.send(crate::features::voice::VoiceCmd::SetMicVolume { percent: pct });
                                    },
                                }
                                if auto_gain_control && mic_volume != 100 {
                                    span { class: "text-[10px] text-[var(--text-dim)] mt-0.5 block",
                                        "Auto gain is on and will pull this back — turn it off below to keep this level."
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
                                    span { class: "text-[10px] text-[var(--text-dim)]", "{sensitivity_display}" }
                                }
                                input {
                                    r#type: "range",
                                    // The slider moves in even dB steps, not
                                    // even amplitude steps — otherwise the
                                    // entire useful range is crammed into the
                                    // bottom few percent of its travel.
                                    min: "0",
                                    max: "100",
                                    value: "{threshold_pct}",
                                    class: "w-full mt-1 accent-[var(--accent)]",
                                    oninput: move |e| {
                                        let pct: u32 = e.value().parse().unwrap_or(40).clamp(0, 100);
                                        let val = crate::features::voice::meter_pct_to_peak(pct);
                                        // Persist to client settings
                                        let mut next = settings.read().clone();
                                        next.mic_sensitivity = val;
                                        settings.set(next.clone());
                                        crate::settings::save(&next);
                                        // Update AppState + tell voice service (takes effect
                                        // on the next 150ms speaking-detection tick).
                                        state.write().mic_sensitivity = val;
                                        let v = v_for_sensitivity.clone();
                                        v.send(crate::features::voice::VoiceCmd::SetSensitivity { threshold: val });
                                    },
                                }
                                // Live gate state. Moving the slider past the
                                // current level flips this within ~300ms, which
                                // is the quickest way to see that the control
                                // does something — and the quickest way to spot
                                // a threshold set so high you've gone silent.
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
                            // Automatic gain control (libwebrtc's own). Its own
                            // switch because it and the input slider above are
                            // two answers to the same question — left on, it
                            // walks a manual level back toward its target over
                            // a second or two.
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
                            // Noise cancellation (DeepFilterNet). Applies to the
                            // live capture immediately — the model is loaded on
                            // first enable, which takes ~200ms.
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
                            // How far that model may pull a hop down. A ceiling, not a
                            // strength dial — the model decides how much of it to use, and
                            // on ordinary speech it rarely reaches even half. Measured on
                            // one microphone, moving this from 30 to 12 changed what was
                            // actually applied by about a decibel; it is here because that
                            // was one microphone, and a noisier room is where it bites.
                            if noise_cancellation {
                                div { class: "mb-2",
                                    div { class: "flex items-center justify-between",
                                        span { class: "text-[11px] text-[var(--text-muted)]", "Suppression strength" }
                                        span { class: "text-[10px] text-[var(--text-dim)]", "{atten_lim_db} dB max" }
                                    }
                                    input {
                                        r#type: "range",
                                        min: "6",
                                        max: "60",
                                        step: "1",
                                        value: "{atten_lim_db}",
                                        class: "w-full mt-1 accent-[var(--accent)]",
                                        oninput: move |e| {
                                            let db: u32 = e.value().parse().unwrap_or(30).clamp(6, 60);
                                            let mut next = settings.read().clone();
                                            next.denoise_atten_lim_db = db;
                                            settings.set(next.clone());
                                            crate::settings::save(&next);
                                            state.write().denoise_atten_lim_db = db;
                                            // Live: the DSP thread reapplies it on the next hop, so
                                            // dragging this mid-sentence costs no model reload.
                                            v_for_atten.send(crate::features::voice::VoiceCmd::SetDenoiseAttenLim { db });
                                        },
                                    }
                                    span { class: "text-[10px] text-[var(--text-dim)] mt-0.5 block",
                                        "Lower keeps more of your voice, and more of the room with it."
                                    }
                                }
                            }
                            // ---- Transmission ----
                            div { class: "mt-3 mb-1.5 pb-1 border-b border-[var(--border)]",
                                span { class: "text-[10px] font-semibold uppercase tracking-wide text-[var(--text)]", "Transmission" }
                            }
                            // Outgoing voice quality. The encoder is configured
                            // when the mic track is published, so this can only
                            // land on the next join — said out loud below
                            // rather than letting the user wonder why the call
                            // they're in sounds the same.
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
                                // Share the machine's sound. On unless turned
                                // off — a shared video or game without its audio
                                // is the surprising outcome, not the safe one.
                                // Applies to the next share.
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
                            // Connection stats. Folded away and not persisted:
                            // it is a diagnostic for when something sounds
                            // wrong, and the poll behind it only runs while
                            // this is open.
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
                        // Mute is only ever about the mic — deafen rides along
                        // untouched instead of being forged from it.
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
