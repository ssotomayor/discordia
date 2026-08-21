//! The surface you land on with a key and no server.
//!
//! Direct messages are NIP-17 gift wraps and the contact list is a NIP-02
//! kind:3 event, so both belong to your key and neither has ever needed a
//! gateway. Until `IdentityHost` existed they were unreachable without one
//! anyway, because `AppState` was born inside `WorkspaceView`. This is the
//! other half of that change: the same panels, mounted without a session.
//!
//! **The components are the ones the workspace uses**, not copies. A second
//! DM list would be a second thing to keep correct, and the two would drift
//! the first time either was touched. What differs is which panels are
//! mounted, not what they are.

use dioxus::prelude::*;
use tokio::sync::mpsc::unbounded_channel;

use crate::features::{channels::ChannelsColumn, chat::ChatView, guilds::GuildsSidebar};
use crate::protocol::ClientMessage;
use crate::state::{GatewayTx, use_app_state};

/// Provided by `HomeView` and by nothing else.
///
/// `GuildsSidebar` is shared with the workspace, so it asks for this with
/// `try_consume_context` and draws the "connect to a server" entry only where
/// it is present. That keeps the rail one component instead of two, and keeps
/// the workspace from growing a button that means nothing there.
#[derive(Clone, Copy)]
pub struct HomeChrome {
    pub show_connect: Signal<bool>,
}

#[component]
pub fn HomeView(show_connect: Signal<bool>) -> Element {
    let state = use_app_state();
    provide_context(HomeChrome { show_connect });

    // `use_gateway()` is `use_context`, which panics when the context is
    // absent, and 29 call sites reach for it. The panels mounted here send
    // nothing through it — that is what `dm: stop asking the gateway about
    // channels it does not have` established, and it is the only reason this
    // is safe. So this is not a licence to ignore sends: anything arriving
    // here is a bug that the DM path grew back, and it says so on stderr
    // rather than vanishing. Same split `version::check_for_update` uses —
    // nothing to tell the user, plenty to tell whoever reads the log.
    let gateway_tx = use_hook(|| {
        let (tx, mut rx) = unbounded_channel::<ClientMessage>();
        spawn(async move {
            while let Some(msg) = rx.recv().await {
                eprintln!("[home] no server is connected; dropped {msg:?}");
            }
        });
        GatewayTx(tx)
    });
    provide_context(gateway_tx);

    // `ChannelsColumn` shows conversations only in DM mode, and the flag
    // survives a disconnect because it is a preference rather than server data.
    // Here it is not a preference: there is no guild to be in.
    use_effect(move || {
        let mut app = state;
        if !app.read().dm_mode {
            app.write().dm_mode = true;
        }
    });

    let mac_top_pad = if cfg!(target_os = "macos") {
        "pt-5"
    } else {
        ""
    };
    let has_conversations = !state.read().dms.is_empty();

    rsx! {
        div { class: "h-full w-full flex flex-col bg-[var(--bg)] p-2 gap-2 {mac_top_pad}",
            crate::features::profiles::ProfileCard {}
            crate::features::chat::ImageViewer {}

            div {
                class: "dxf-drag-region h-6 shrink-0",
                onmousedown: move |_| crate::app::start_window_drag(),
            }

            div { class: "flex-1 flex gap-2 min-h-0",
                div { class: "w-14 shrink-0",
                    GuildsSidebar {}
                }
                div { class: "w-60 shrink-0",
                    ChannelsColumn {}
                }
                div { class: "flex-1 min-w-0",
                    if has_conversations {
                        ChatView {}
                    } else {
                        NoConversationsYet { show_connect }
                    }
                }
            }
        }
    }
}

/// What the middle column says before anyone has written to you.
///
/// It names the two things that are true here and nowhere else in the app:
/// these messages do not belong to a server, and you can use them without
/// joining one. Otherwise an empty home reads as a broken one.
#[component]
fn NoConversationsYet(mut show_connect: Signal<bool>) -> Element {
    rsx! {
        div { class: "panel-hover h-full w-full bg-[var(--panel)] border border-[var(--border)] rounded-lg flex flex-col items-center justify-center gap-4 px-10 text-center",
            div { class: "text-base font-semibold text-[var(--text)]",
                "No conversations yet"
            }
            p { class: "text-sm text-[var(--text-muted)] max-w-[420px] leading-relaxed",
                "Direct messages travel on Nostr relays, signed by your key — not by a server. Paste someone's npub on the left to start one, and it will follow you to any machine you sign in on."
            }
            button {
                r#type: "button",
                class: "text-xs px-3 py-1.5 rounded border border-[var(--border)] text-[var(--text-muted)] hover:text-[var(--accent)] hover:border-[var(--accent)] transition-colors",
                onclick: move |_| show_connect.set(true),
                "Connect to a server"
            }
        }
    }
}
