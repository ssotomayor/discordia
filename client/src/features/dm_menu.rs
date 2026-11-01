use dioxus::prelude::*;

use crate::protocol::Id;
use crate::state::use_app_state;

#[derive(Clone, Copy, PartialEq)]
pub(crate) struct DmMenu {
    pub channel_id: Id,
    pub x: f64,
    pub y: f64,
}

impl DmMenu {
    /// Right click on desktop; Android's WebView sends the same event for a long press.
    pub(crate) fn at(channel_id: Id, event: &MouseEvent) -> Self {
        let c = event.client_coordinates();
        Self {
            channel_id,
            x: c.x,
            y: c.y,
        }
    }
}

#[component]
pub(crate) fn DmMenuPopover(menu: DmMenu, on_close: EventHandler<()>) -> Element {
    let state = use_app_state();
    let settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let cid = menu.channel_id;
    let snapshot = state.read();
    let name = snapshot
        .dm_of(cid)
        .map(|dm| snapshot.display_name(&dm.other_pubkey))
        .unwrap_or_default();
    let muted = snapshot.channel_muted(cid);
    drop(snapshot);

    rsx! {
        div {
            class: "fixed inset-0 z-50",
            onclick: move |_| on_close.call(()),
            oncontextmenu: move |e| { e.prevent_default(); on_close.call(()); },
            div {
                class: "dxf-pop-in absolute min-w-48 bg-[var(--panel-solid)] border border-[var(--border)] rounded-md shadow-lg p-1 text-sm",
                style: "left: {menu.x}px; top: {menu.y}px;",
                onclick: move |e| e.stop_propagation(),
                div { class: "px-3 py-1.5 text-xs text-[var(--text-muted)] border-b border-[var(--border)] mb-1 truncate",
                    "{name}"
                }
                button {
                    class: "w-full text-left px-3 py-1.5 rounded text-[var(--text)] hover:bg-white/[0.04] transition-colors",
                    onclick: move |_| {
                        crate::state::set_dm_muted(state, settings, cid, !muted);
                        on_close.call(());
                    },
                    if muted { {crate::i18n::text("Unmute this conversation", &[])} } else { {crate::i18n::text("Mute this conversation", &[])} }
                }
            }
        }
    }
}
