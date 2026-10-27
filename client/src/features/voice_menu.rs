//! The right-click menu on someone in a voice channel: their volume for you,
//! a local mute, and the moderator's disconnect. Mounted at the workspace root
//! because the channel list is a grid panel (trap 22).

use dioxus::prelude::*;

use crate::features::voice::{VoiceCmd, use_voice_tx};
use crate::protocol::{ClientMessage, Permission};
use crate::state::{VoiceMenu, use_app_state, use_gateway};

const MENU_W: f64 = 232.0;
const MENU_H: f64 = 220.0;

#[component]
pub fn VoiceMenuHost() -> Element {
    let mut state = use_app_state();
    let menu = use_memo(move || state.read().voice_menu.clone());
    // Whoever it was opened on may leave the call, or we may; the menu goes with them.
    let gone = use_memo(move || {
        let s = state.read();
        s.voice_menu.as_ref().is_some_and(|m| {
            !s.voice_states
                .iter()
                .any(|v| v.user_pubkey == m.pubkey && v.channel_id.is_some())
        })
    });
    use_effect(move || {
        if gone() {
            state.write().voice_menu = None;
        }
    });
    rsx! {
        for m in menu().filter(|_| !gone()) {
            VoiceMenuPopover { key: "{m.pubkey}", menu: m }
        }
    }
}

#[component]
fn VoiceMenuPopover(menu: VoiceMenu) -> Element {
    let mut state = use_app_state();
    let gateway = use_gateway();
    let voice = use_voice_tx();
    let pubkey = menu.pubkey.clone();

    let (volume, locally_muted, disconnect_in, track_stats, health, ping_ms) = {
        let s = state.read();
        let volume = s.user_volumes.get(&pubkey).copied().unwrap_or(100);
        let muted = s.user_muted.contains(&pubkey);
        let track_stats = s.voice_stats.get(&pubkey).copied();
        let health = s.voice_quality.get(&pubkey).copied();
        let ping_ms = s.voice_ping_ms;
        // The server re-checks; this only hides a button that would be refused.
        let disconnect_in = s
            .voice_states
            .iter()
            .find(|v| v.user_pubkey == pubkey)
            .map(|v| v.guild_id)
            .filter(|gid| {
                s.can(*gid, Permission::DisconnectMembers)
                    && !s
                        .guilds
                        .iter()
                        .any(|g| g.id == *gid && g.owner_pubkey == pubkey)
            });
        (volume, muted, disconnect_in, track_stats, health, ping_ms)
    };

    let apply = {
        let pubkey = pubkey.clone();
        move || {
            let gain = state.read().voice_gain_of(&pubkey);
            voice.send(VoiceCmd::SetUserVolume {
                pubkey: pubkey.clone(),
                gain,
            });
        }
    };
    let apply_slider = apply.clone();
    let pk_slider = pubkey.clone();
    let pk_mute = pubkey.clone();
    let pk_profile = pubkey.clone();
    let pk_disconnect = pubkey.clone();
    let mut close = move || state.write().voice_menu = None;

    let style = format!(
        "left:clamp(8px, {:.0}px, calc(100vw - {:.0}px)); top:min({:.0}px, calc(100vh - {MENU_H:.0}px)); width:{MENU_W:.0}px;",
        menu.x,
        MENU_W + 8.0,
        menu.y
    );

    rsx! {
        div {
            class: "fixed inset-0 z-[70]",
            onclick: move |_| close(),
            oncontextmenu: move |e| { e.prevent_default(); close(); },
            div {
                class: "dxf-pop-in fixed bg-[var(--panel-solid)] border border-[var(--border)] rounded-md shadow-lg p-1 text-sm select-none",
                style: "{style}",
                onclick: move |e| e.stop_propagation(),
                div { class: "px-3 py-1.5 text-xs text-[var(--text-muted)] border-b border-[var(--border)] mb-1 flex items-center justify-between gap-1",
                    span { class: "truncate font-medium", "{menu.name}" }
                    if let Some(h) = health {
                        span {
                            class: match h {
                                crate::state::ConnectionHealth::Excellent | crate::state::ConnectionHealth::Good => "text-[10px] text-[var(--success)] shrink-0",
                                crate::state::ConnectionHealth::Poor => "text-[10px] text-[var(--warn)] shrink-0",
                                crate::state::ConnectionHealth::Lost => "text-[10px] text-[var(--danger)] shrink-0",
                            },
                            match h {
                                crate::state::ConnectionHealth::Excellent => crate::i18n::text("Excellent", &[]),
                                crate::state::ConnectionHealth::Good => crate::i18n::text("Good", &[]),
                                crate::state::ConnectionHealth::Poor => crate::i18n::text("Reduced", &[]),
                                crate::state::ConnectionHealth::Lost => crate::i18n::text("Disrupted", &[]),
                            }
                        }
                    }
                }
                if let Some(stats) = track_stats {
                    match stats {
                        crate::state::TrackStats::Inbound { loss_pct, recent_loss_pct, jitter_ms, .. } => rsx! {
                            div { class: "px-3 py-1 text-[11px] text-[var(--text-dim)] border-b border-[var(--border)] mb-1 flex items-center justify-between font-mono gap-1.5",
                                span {
                                    class: if recent_loss_pct.is_some_and(|l| l >= 1.0) { "text-[var(--warn)]" } else { "" },
                                    {crate::i18n::text("Loss: {0}%", &[format!("{:.1}", recent_loss_pct.unwrap_or(loss_pct))])}
                                }
                                span { {crate::i18n::text("Jit: {0}ms", &[format!("{jitter_ms:.0}")])} }
                                if let Some(ping) = ping_ms {
                                    span { {crate::i18n::text("Ping: {0}ms", &[ping.to_string()])} }
                                }
                            }
                        },
                        crate::state::TrackStats::Outbound { bitrate_kbps, packets_per_sec, loss_pct, .. } => rsx! {
                            div { class: "px-3 py-1 text-[11px] text-[var(--text-dim)] border-b border-[var(--border)] mb-1 flex items-center justify-between font-mono gap-1.5",
                                if let Some(loss) = loss_pct {
                                    span { {crate::i18n::text("Loss: {0}%", &[format!("{loss:.1}")])} }
                                } else {
                                    span { {crate::i18n::text("Out: {0} kbps", &[bitrate_kbps.unwrap_or(0).to_string()])} }
                                }
                                span { {crate::i18n::text("{0} pkt/s", &[packets_per_sec.unwrap_or(0).to_string()])} }
                                if let Some(ping) = ping_ms {
                                    span { {crate::i18n::text("Ping: {0}ms", &[ping.to_string()])} }
                                }
                            }
                        },
                        _ => rsx! {},
                    }
                }
                div { class: "px-3 pt-1 pb-0.5 text-[10px] uppercase tracking-wider text-[var(--text-dim)]",
                    {crate::i18n::text("Volume for you", &[])}
                }
                div { class: "flex items-center gap-2 px-3 pb-1.5",
                    input {
                        r#type: "range",
                        min: "0",
                        max: "200",
                        value: "{volume}",
                        disabled: locally_muted,
                        class: "flex-1 accent-[var(--accent)] disabled:opacity-40",
                        aria_label: crate::i18n::text("Volume for you", &[]),
                        oninput: move |e| {
                            let val: u32 = e.value().parse().unwrap_or(100).clamp(0, 200);
                            state.write().user_volumes.insert(pk_slider.clone(), val);
                            apply_slider();
                        },
                    }
                    span { class: "text-[10px] text-[var(--text-dim)] w-9 text-right shrink-0", "{volume}%" }
                }
                button {
                    class: if locally_muted {
                        "w-full text-left px-3 py-1.5 rounded text-xs text-[var(--danger)] hover:bg-white/[0.04] transition-colors"
                    } else {
                        "w-full text-left px-3 py-1.5 rounded text-xs text-[var(--text)] hover:bg-white/[0.04] transition-colors"
                    },
                    title: crate::i18n::text("Only changes what you hear", &[]),
                    onclick: move |_| {
                        let now = !locally_muted;
                        {
                            let mut s = state.write();
                            if now { s.user_muted.insert(pk_mute.clone()); } else { s.user_muted.remove(&pk_mute); }
                        }
                        apply();
                    },
                    if locally_muted { {crate::i18n::text("Unmute for you", &[])} } else { {crate::i18n::text("Mute for you", &[])} }
                }
                button {
                    class: "w-full text-left px-3 py-1.5 rounded text-xs text-[var(--text)] hover:bg-white/[0.04] transition-colors",
                    onclick: move |_| {
                        state.write().profile_card = Some(pk_profile.clone());
                        close();
                    },
                    {crate::i18n::text("View profile", &[])}
                }
                if let Some(gid) = disconnect_in {
                    div { class: "border-t border-[var(--border)] my-1" }
                    button {
                        class: "w-full text-left px-3 py-1.5 rounded text-xs text-[var(--warn)] hover:bg-[var(--warn)]/10 transition-colors",
                        title: crate::i18n::text("Drops their voice, screen share and camera for everyone. They can join again.", &[]),
                        onclick: move |_| {
                            gateway.send(ClientMessage::DisconnectVoice {
                                guild_id: gid,
                                user_pubkey: pk_disconnect.clone(),
                            });
                            close();
                        },
                        {crate::i18n::text("Disconnect from voice", &[])}
                    }
                }
            }
        }
    }
}
