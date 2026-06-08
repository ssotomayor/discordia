use dioxus::prelude::*;

use crate::features::voice::{VoiceCmd, use_voice_tx};
use crate::protocol::{Channel, ChannelKind, ClientMessage, Id, VoiceState};
use crate::state::{AppState, GatewayTx, VoicePhase, use_app_state, use_gateway};

#[component]
pub fn ChannelsColumn() -> Element {
    let mut state = use_app_state();
    let gateway = use_gateway();
    let voice = use_voice_tx();

    let snapshot = state.read();
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
        aside { class: "w-60 shrink-0 bg-[#2b2d31] flex flex-col border-r border-black/20",
            div { class: "h-12 px-4 flex items-center border-b border-black/30 shadow-sm",
                h2 { class: "font-bold text-white truncate",
                    {guild.as_ref().map(|g| g.name.clone()).unwrap_or_else(|| "No server".into())}
                }
            }

            div { class: "flex-1 overflow-y-auto px-2 py-3 space-y-3",
                if !text_channels.is_empty() {
                    div {
                        div { class: "px-2 py-1 text-xs font-bold uppercase tracking-wide text-gray-400",
                            "Text channels"
                        }
                        for channel in text_channels.iter() {
                            {
                                let ch = (*channel).clone();
                                let cid = ch.id;
                                let active = selected_channel == Some(cid);
                                let cls = if active {
                                    "bg-white/10 text-white"
                                } else {
                                    "text-gray-400 hover:bg-white/5 hover:text-gray-200"
                                };
                                let g2 = gateway.clone();
                                rsx! {
                                    button {
                                        key: "{cid}",
                                        class: "w-full flex items-center gap-1.5 px-2 py-1.5 rounded text-left text-sm transition-colors {cls}",
                                        onclick: move |_| select_text_channel(&mut state, &g2, cid),
                                        span { class: "text-gray-500", "#" }
                                        span { class: "truncate", "{ch.name}" }
                                    }
                                }
                            }
                        }
                    }
                }

                if !voice_channels.is_empty() {
                    div {
                        div { class: "px-2 py-1 text-xs font-bold uppercase tracking-wide text-gray-400",
                            "Voice channels"
                        }
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
                                        self_user_id: self_user.as_ref().map(|u| u.id),
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

            // User panel with voice controls.
            UserPanel { self_voice: self_voice, self_username: self_user.map(|u| u.username) }
        }
    }
}

#[component]
fn VoiceChannelRow(
    channel: Channel,
    connected: bool,
    occupants: Vec<VoiceState>,
    self_user_id: Option<Id>,
    on_join: EventHandler<()>,
    on_leave: EventHandler<()>,
) -> Element {
    let row_cls = if connected {
        "text-white bg-white/5"
    } else {
        "text-gray-400 hover:bg-white/5 hover:text-gray-200"
    };
    let state = use_app_state();
    let users_by_id = state.read();

    rsx! {
        div { class: "rounded",
            button {
                class: "w-full flex items-center gap-1.5 px-2 py-1.5 rounded text-left text-sm transition-colors {row_cls}",
                onclick: move |_| {
                    if connected { on_leave.call(()) } else { on_join.call(()) }
                },
                span { class: "text-gray-500", "🔊" }
                span { class: "truncate flex-1", "{channel.name}" }
                if connected {
                    span { class: "text-[10px] text-emerald-400 font-semibold uppercase", "live" }
                }
            }
            if !occupants.is_empty() {
                div { class: "ml-5 mt-0.5 space-y-0.5",
                    for vs in occupants.iter() {
                        {
                            let name = users_by_id
                                .user_of(vs.user_id)
                                .map(|u| u.username.clone())
                                .unwrap_or_else(|| short_id(vs.user_id));
                            let is_self = self_user_id == Some(vs.user_id);
                            let dot = if vs.speaking { "bg-emerald-400" } else { "bg-white/30" };
                            let mute_badge = if vs.muted { Some("🔇") } else { None };
                            rsx! {
                                div {
                                    key: "{vs.user_id}",
                                    class: "flex items-center gap-1.5 px-2 py-0.5 text-xs text-gray-300",
                                    span { class: "w-1.5 h-1.5 rounded-full {dot}" }
                                    span { class: "truncate flex-1",
                                        "{name}"
                                        if is_self { " (you)" }
                                    }
                                    if let Some(badge) = mute_badge {
                                        span { "{badge}" }
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
    let initial = self_username
        .as_deref()
        .map(|n| n.chars().next().unwrap_or('?').to_ascii_uppercase().to_string())
        .unwrap_or_else(|| "?".into());
    let name = self_username.clone().unwrap_or_else(|| "—".into());

    let show_banner = !matches!(self_voice.phase, VoicePhase::Idle);
    let phase_label = match self_voice.phase {
        VoicePhase::Idle => None,
        VoicePhase::Connecting => Some(("text-yellow-300", "connecting…")),
        VoicePhase::Connected => Some(("text-emerald-400", "voice connected")),
        VoicePhase::Error => Some(("text-red-300", "voice error")),
    };
    let voice_error = self_voice.error.clone();

    let muted = self_voice.muted;
    let mute_icon = if muted { "🔇" } else { "🎙" };
    let g_for_mute = gateway.clone();
    let v_for_mute = voice.clone();
    let g_for_hang = gateway.clone();
    let v_for_hang = voice.clone();

    rsx! {
        div { class: "border-t border-black/30",
            if show_banner {
                div { class: "px-3 py-2 bg-[#1e1f22] border-b border-black/20",
                    div { class: "flex items-center gap-2",
                        span { class: "text-emerald-400 text-xs font-bold uppercase", "Voice" }
                        if let Some((cls, label)) = phase_label {
                            span { class: "text-xs {cls}", "{label}" }
                        }
                        div { class: "flex-1" }
                        button {
                            class: "text-red-300 hover:text-red-400 text-xs font-semibold",
                            onclick: move |_| {
                                g_for_hang.send(ClientMessage::LeaveVoice);
                                v_for_hang.send(VoiceCmd::Disconnect);
                            },
                            "Disconnect"
                        }
                    }
                    if let Some(err) = voice_error {
                        div { class: "text-[11px] text-red-300/90 mt-1 break-all",
                            "{err}"
                        }
                    }
                }
            }
            div { class: "h-14 bg-[#232428] px-2 flex items-center gap-2",
                div { class: "w-8 h-8 rounded-full bg-indigo-500 flex items-center justify-center text-xs font-bold text-white",
                    "{initial}"
                }
                div { class: "flex-1 min-w-0",
                    div { class: "text-sm font-semibold text-white truncate", "{name}" }
                    div { class: "text-xs text-gray-400", "Online" }
                }
                button {
                    class: "w-8 h-8 rounded hover:bg-white/10 flex items-center justify-center text-sm",
                    title: if muted { "Unmute" } else { "Mute" },
                    onclick: move |_| {
                        let new_muted = !muted;
                        g_for_mute.send(ClientMessage::SetVoiceMute { muted: new_muted, deafened: new_muted });
                        v_for_mute.send(VoiceCmd::SetMute { muted: new_muted });
                    },
                    "{mute_icon}"
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

fn short_id(id: Id) -> String {
    id.to_string().chars().take(6).collect()
}
