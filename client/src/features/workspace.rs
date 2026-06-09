use dioxus::prelude::*;

use crate::features::{
    channels::ChannelsColumn, chat::ChatView, guilds::GuildsSidebar, members::MembersPanel,
    voice::spawn_voice_service,
};
use crate::net::spawn_gateway;
use crate::state::{AppState, ConnectionStatus, SessionParams, VoicePhase, use_app_state};

const CONNECT_SOUND: Asset = asset!("/assets/connect.mp3");

#[component]
pub fn WorkspaceView(params: SessionParams, on_disconnect: EventHandler<String>) -> Element {
    let state = use_signal(AppState::empty);

    let (gateway_tx, voice_tx) = use_hook(|| {
        let voice_tx = spawn_voice_service(state);
        let gateway_tx = spawn_gateway(params.clone(), state, voice_tx.clone(), move |reason| {
            on_disconnect.call(reason);
        });
        (gateway_tx, voice_tx)
    });
    provide_context(gateway_tx.clone());
    provide_context(crate::features::voice::VoiceTx(voice_tx.clone()));
    provide_context(state);

    let status = state.read().status;

    rsx! {
        div { class: "h-full w-full flex flex-col bg-[var(--bg)] p-2 gap-2",
            VoiceSounds {}
            HostBanner {}
            div { class: "flex-1 flex min-h-0 gap-2",
                GuildsSidebar {}
                ChannelsColumn {}
                div { class: "flex-1 flex flex-col min-w-0 bg-[var(--panel)] border border-[var(--border)] rounded-lg overflow-hidden",
                    if status == ConnectionStatus::Connecting {
                        div { class: "flex-1 flex items-center justify-center text-[var(--text-muted)] text-sm",
                            "Connecting…"
                        }
                    } else {
                        ChatView {}
                    }
                }
                MembersPanel {}
            }
        }
    }
}

/// Hidden `<audio>` element + voice-phase watcher.
#[component]
fn VoiceSounds() -> Element {
    let state = use_app_state();
    let phase = use_memo(move || state.read().voice.phase);
    let mut last_phase = use_signal(|| VoicePhase::Idle);

    use_effect(move || {
        let now = phase();
        let prev = *last_phase.peek();
        if now == VoicePhase::Connected && prev != VoicePhase::Connected {
            let _ = document::eval(
                "const a = document.getElementById('voice-connect-sound'); \
                 if (a) { a.currentTime = 0; a.play().catch(() => {}); }",
            );
        }
        last_phase.set(now);
    });

    rsx! {
        audio {
            id: "voice-connect-sound",
            src: CONNECT_SOUND,
            preload: "auto",
            style: "display:none",
        }
    }
}

#[component]
fn HostBanner() -> Element {
    let state = use_app_state();
    let snapshot = state.read();
    let Some(info) = snapshot.host_info.clone() else {
        return rsx! { Fragment {} };
    };
    drop(snapshot);

    let lan_text = info.lan_url.clone();
    let shortcode = info.shortcode.clone();
    let (voice_label, voice_color) = if info.voice_bundled {
        ("voice ready", "text-[var(--success)]")
    } else {
        ("voice unavailable", "text-[var(--warn)]")
    };

    rsx! {
        div { class: "shrink-0 px-3 py-2 bg-[var(--panel)] border border-[var(--border)] rounded-lg flex items-center gap-3 text-xs flex-wrap",
            span { class: "text-[var(--accent)] font-medium tracking-wide", "Self-hosting" }
            if let Some(code) = shortcode {
                span { class: "text-[var(--text-dim)]", "·" }
                span { class: "text-[var(--text-muted)]", "Code:" }
                code { class: "text-[var(--text)] select-all font-medium",
                    "{code}"
                }
            }
            span { class: "text-[var(--text-dim)]", "·" }
            span { class: "text-[var(--text-muted)]", "LAN:" }
            code { class: "text-[var(--text)] select-all",
                "{lan_text}"
            }
            span { class: "flex-1" }
            span { class: "{voice_color}", "● {voice_label}" }
        }
    }
}
