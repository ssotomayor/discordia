//! The right-click menu on someone in a voice channel: their volume for you,
//! a local mute, and the moderator's disconnect. Mounted at the workspace root
//! because the channel list is a grid panel (trap 22).

use dioxus::prelude::*;

use crate::features::voice::{VoiceCmd, use_voice_tx};
use crate::protocol::{ClientMessage, Permission};
use crate::state::{VoiceMenu, use_app_state, use_gateway};

const MENU_W: f64 = 232.0;
const MENU_H: f64 = 190.0;

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

    let (volume, locally_muted, disconnect_in) = {
        let s = state.read();
        let volume = s.user_volumes.get(&pubkey).copied().unwrap_or(100);
        let muted = s.user_muted.contains(&pubkey);
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
        (volume, muted, disconnect_in)
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
                div { class: "px-3 py-1.5 text-xs text-[var(--text-muted)] border-b border-[var(--border)] mb-1 truncate",
                    "{menu.name}"
                }
                div { class: "px-3 pt-1 pb-0.5 text-[10px] uppercase tracking-wider text-[var(--text-dim)]",
                    "Volume for you"
                }
                div { class: "flex items-center gap-2 px-3 pb-1.5",
                    input {
                        r#type: "range",
                        min: "0",
                        max: "200",
                        value: "{volume}",
                        disabled: locally_muted,
                        class: "flex-1 accent-[var(--accent)] disabled:opacity-40",
                        aria_label: "Volume for you",
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
                    title: "Only changes what you hear",
                    onclick: move |_| {
                        let now = !locally_muted;
                        {
                            let mut s = state.write();
                            if now { s.user_muted.insert(pk_mute.clone()); } else { s.user_muted.remove(&pk_mute); }
                        }
                        apply();
                    },
                    if locally_muted { "Unmute for you" } else { "Mute for you" }
                }
                button {
                    class: "w-full text-left px-3 py-1.5 rounded text-xs text-[var(--text)] hover:bg-white/[0.04] transition-colors",
                    onclick: move |_| {
                        state.write().profile_card = Some(pk_profile.clone());
                        close();
                    },
                    "View profile"
                }
                if let Some(gid) = disconnect_in {
                    div { class: "border-t border-[var(--border)] my-1" }
                    button {
                        class: "w-full text-left px-3 py-1.5 rounded text-xs text-[var(--warn)] hover:bg-[var(--warn)]/10 transition-colors",
                        title: "Drops their voice, screen share and camera for everyone. They can join again.",
                        onclick: move |_| {
                            gateway.send(ClientMessage::DisconnectVoice {
                                guild_id: gid,
                                user_pubkey: pk_disconnect.clone(),
                            });
                            close();
                        },
                        "Disconnect from voice"
                    }
                }
            }
        }
    }
}
