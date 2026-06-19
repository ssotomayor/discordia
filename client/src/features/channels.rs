use dioxus::prelude::*;
use dioxus_grid_layout::NoDrag;

use crate::features::voice::{VoiceCmd, use_voice_tx};
use crate::identity::discriminator;
use crate::protocol::{Channel, ChannelKind, ClientMessage, DmInfo, Id, VoiceState};
use crate::state::{AppState, GatewayTx, VoicePhase, use_app_state, use_gateway};

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
            snapshot
                .channels
                .iter()
                .filter(|c| c.guild_id == gid)
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    let voice_states: Vec<VoiceState> = snapshot.voice_states.clone();
    let self_user = snapshot.self_user.clone();
    let self_voice = snapshot.voice.clone();
    drop(snapshot);

    let text_channels: Vec<&Channel> = channels
        .iter()
        .filter(|c| matches!(c.kind, ChannelKind::Text))
        .collect();
    let voice_channels: Vec<&Channel> = channels
        .iter()
        .filter(|c| matches!(c.kind, ChannelKind::Voice))
        .collect();

    rsx! {
        aside { class: PANEL,
            div { class: HEADER,
                h2 { class: "text-sm text-[var(--accent)] truncate font-medium",
                    if dm_mode {
                        "Direct Messages"
                    } else {
                        {guild.as_ref().map(|g| g.name.clone()).unwrap_or_else(|| "No server".into())}
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
                                rsx! {
                                    button {
                                        key: "{cid}",
                                        class: "w-full flex items-center gap-1.5 px-2 py-1 rounded text-left text-sm transition-colors {cls}",
                                        onclick: move |_| select_text_channel(&mut state, &g2, cid),
                                        span { class: "text-[var(--text-dim)]", "#" }
                                        span { class: "truncate", "{ch.name}" }
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
                                rsx! {
                                    VoiceChannelRow {
                                        key: "{cid}",
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

            UserPanel { self_voice: self_voice, self_username: self_user.map(|u| u.username) }
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
    let self_pubkey = state.read().self_user.as_ref().map(|u| u.pubkey.clone());
    let sharing = state.read().screen_sharing;
    let name = self_username.clone().unwrap_or_else(|| "—".into());

    let show_banner = !matches!(self_voice.phase, VoicePhase::Idle);
    let phase_label = match self_voice.phase {
        VoicePhase::Idle => None,
        VoicePhase::Connecting => Some(("text-[var(--warn)]", "connecting…")),
        VoicePhase::Connected => Some(("text-[var(--success)]", "connected")),
        VoicePhase::Error => Some(("text-[var(--danger)]", "error")),
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

    rsx! {
        div { class: "border-t border-[var(--border)]",
            if show_banner {
                div { class: "px-3 py-2 border-b border-[var(--border)]",
                    div { class: "flex items-center gap-2",
                        span { class: "text-[10px] text-[var(--accent)] font-semibold uppercase tracking-wider", "Voice" }
                        if let Some((cls, label)) = phase_label {
                            span { class: "text-[10px] {cls}", "{label}" }
                        }
                        div { class: "flex-1" }
                        // Screen share toggle. Calls getDisplayMedia inside this
                        // click gesture (must not be deferred to an effect).
                        button {
                            class: if sharing {
                                "flex items-center gap-1 text-[10px] text-[var(--accent)] font-medium uppercase tracking-wider"
                            } else {
                                "flex items-center gap-1 text-[10px] text-[var(--text-muted)] hover:text-[var(--accent)] font-medium uppercase tracking-wider"
                            },
                            title: if sharing { "Stop sharing your screen" } else { "Share your screen" },
                            onclick: move |_| {
                                let now = !sharing;
                                state.write().screen_sharing = now;
                                let _ = document::eval(&crate::features::screenshare::share_js(now));
                                if let Some(cid) = voice_channel {
                                    g_for_share.send(ClientMessage::SetScreenShare { channel_id: cid, sharing: now });
                                }
                            },
                            span { dangerous_inner_html: crate::features::icons::SCREEN }
                            if sharing { "stop" } else { "share" }
                        }
                        button {
                            class: "text-[10px] text-[var(--danger)] hover:text-[var(--accent-strong)] font-medium uppercase tracking-wider",
                            onclick: move |_| {
                                g_for_hang.send(ClientMessage::LeaveVoice);
                                v_for_hang.send(VoiceCmd::Disconnect);
                            },
                            "disconnect"
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
                    div { class: "text-sm text-[var(--text)] truncate", "{name}" }
                }
                crate::features::profiles::ProfileEditor {}
                crate::features::appearance::AppearanceButton {}
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
        });
    }
}

