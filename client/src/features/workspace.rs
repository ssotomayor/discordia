use dioxus::prelude::*;
use dioxus_grid_layout::{GridItem, GridLayout, GridPosition, use_layout_store};

use crate::features::{
    channels::ChannelsColumn, chat::ChatView, guilds::GuildsSidebar, members::MembersPanel,
    voice::spawn_voice_service, wallet::WalletControls,
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

    // Initial 4-panel dashboard layout. 12 cols × 24px row height; total
    // height ≈ 720px which fits a default desktop window. Users can drag
    // and resize via the corner grip.
    let layout = use_layout_store(|| {
        vec![
            ("guilds".into(), GridPosition::new(0, 0, 1, 30)),
            ("channels".into(), GridPosition::new(1, 0, 2, 30)),
            ("chat".into(), GridPosition::new(3, 0, 7, 30)),
            ("members".into(), GridPosition::new(10, 0, 2, 30)),
        ]
    });

    let mut edit_mode = use_signal(|| false);
    let status = state.read().status;

    // Pad above the top row on macOS so the traffic lights (which float
    // over our content thanks to fullsize content view) don't collide
    // with the host banner / brand / wallet buttons. Reserve enough
    // horizontal space at the left to fully clear them.
    let mac_top_pad = if cfg!(target_os = "macos") { "pt-7 pl-20" } else { "" };

    rsx! {
        div { class: "h-full w-full flex flex-col bg-[var(--bg)] p-2 gap-2 {mac_top_pad}",
            VoiceSounds {}

            // Top row: host banner (only renders when self-hosting) grows
            // to push the brand mark + wallet button to the right. The
            // whole row is a drag region so the empty space between
            // elements lets the user move the window; the interactive
            // children opt out with .dxf-no-drag.
            div { class: "dxf-drag-region flex items-stretch gap-2",
                HostBanner {}
                div { class: "shrink-0 flex items-center px-2",
                    img {
                        src: crate::app::DISCORDIA_LOGO,
                        alt: "Discordia",
                        class: "dxf-logo w-7 h-7",
                        title: "Discordia",
                    }
                }
                div { class: "dxf-no-drag",
                    WalletControls { identity: params.identity.clone() }
                }
            }

            div { class: "flex-1 overflow-auto min-h-0",
                GridLayout {
                    cols: 12, row_height: 24.0, gap: 8.0,
                    store: layout, editable: edit_mode(),
                    GridItem { id: "guilds", x: 0, y: 0, w: 1, h: 30, min_w: 1, min_h: 10,
                        GuildsSidebar {}
                    }
                    GridItem { id: "channels", x: 1, y: 0, w: 2, h: 30, min_w: 2, min_h: 10,
                        ChannelsColumn {}
                    }
                    GridItem { id: "chat", x: 3, y: 0, w: 7, h: 30, min_w: 3, min_h: 10,
                        div { class: "panel-hover w-full h-full flex flex-col bg-[var(--panel)] border border-[var(--border)] rounded-lg overflow-hidden",
                            if status == ConnectionStatus::Connecting {
                                div { class: "flex-1 flex items-center justify-center text-[var(--text-muted)] text-sm",
                                    "Connecting…"
                                }
                            } else {
                                ChatView {}
                            }
                        }
                    }
                    GridItem { id: "members", x: 10, y: 0, w: 2, h: 30, min_w: 2, min_h: 10,
                        MembersPanel {}
                    }
                }
            }

            // Floating edit-mode toggle. Always visible, subtle.
            button {
                class: "fixed bottom-3 right-3 z-40 border border-[var(--border)] rounded px-3 py-1 text-[10px] uppercase tracking-wider bg-[var(--panel)] hover:border-[var(--accent)] text-[var(--text-muted)] hover:text-[var(--accent)] transition-colors",
                onclick: move |_| edit_mode.set(!edit_mode()),
                if edit_mode() { "Done" } else { "Edit layout" }
            }
        }
    }
}

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
        div { class: "panel-hover flex-1 min-w-0 px-3 py-2 bg-[var(--panel)] border border-[var(--border)] rounded-lg flex items-center gap-3 text-xs flex-wrap",
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
