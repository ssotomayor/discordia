//! The server bar: everywhere this key can go, across the top.
//!
//! Above both views on purpose. Servers are not something you find *inside* a
//! screen — they are the thing you switch between — so the bar sits over the
//! home and the workspace alike rather than belonging to either.
//!
//! **A guild is not a server and the rail is not this.** A server is a host you
//! connect to; a guild is a community inside one. They were sharing a strip of
//! chrome, which is why the rail could read "Guilds" while the only thing in it
//! offered to connect to a server. Now each has its own edge of the window.
//!
//! Selecting one connects, which means dropping the current connection: the
//! session model is genuinely one server at a time, and the bar does not
//! pretend otherwise.

use dioxus::prelude::*;

use crate::identity::Identity;
use crate::session;
use crate::state::{SessionMode, SessionParams};

#[component]
pub fn ServerBar(
    identity: Identity,
    current: Option<SessionMode>,
    on_connect: EventHandler<SessionParams>,
    on_add: EventHandler<()>,
) -> Element {
    // Re-read on every change rather than holding a copy: the list is written
    // by `session::save` from the connect flow, which does not go through here.
    let mut revision = use_signal(|| 0u32);
    let servers = use_memo(move || {
        let _ = revision();
        session::load_all()
    });

    rsx! {
        div {
            class: "dxf-drag-region shrink-0 flex items-center gap-1 px-2 h-9 border-b border-[var(--border)]",
            onmousedown: move |_| crate::app::start_window_drag(),

            span { class: "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-dim)] pr-1",
                "Servers"
            }

            for saved in servers().into_iter() {
                {
                    let active = current.as_ref() == Some(&saved.mode);
                    let label = session::label(&saved);
                    let connect = saved.clone();
                    let identity = identity.clone();
                    let drop_mode = saved.mode.clone();
                    let cls = if active {
                        "border-[var(--accent)] bg-[var(--accent-soft)] text-[var(--accent)]"
                    } else {
                        "border-[var(--border)] text-[var(--text-muted)] hover:text-[var(--text)] hover:border-[var(--border-strong)]"
                    };
                    rsx! {
                        div {
                            key: "{label}",
                            class: "group flex items-center rounded border transition-colors {cls}",
                            button {
                                r#type: "button",
                                class: "max-w-[220px] truncate px-2 py-0.5 text-xs",
                                title: "{label}",
                                // mousedown, not click: the bar around this
                                // starts a native window drag, which swallows
                                // the mouseup and with it the click.
                                onmousedown: move |e| {
                                    e.stop_propagation();
                                    if active {
                                        return;
                                    }
                                    on_connect.call(SessionParams {
                                        mode: connect.mode.clone(),
                                        username: connect.username.clone(),
                                        identity: identity.clone(),
                                    });
                                },
                                "{label}"
                            }
                            button {
                                r#type: "button",
                                class: "shrink-0 px-1 text-[10px] opacity-0 group-hover:opacity-100 text-[var(--text-dim)] hover:text-[var(--danger)] transition-all",
                                title: "Forget this server",
                                onmousedown: move |e| {
                                    e.stop_propagation();
                                    let _ = session::forget(&drop_mode);
                                    revision += 1;
                                },
                                "✕"
                            }
                        }
                    }
                }
            }

            button {
                r#type: "button",
                class: "shrink-0 w-6 h-6 rounded border border-dashed border-[var(--border)] text-[var(--text-muted)] hover:text-[var(--accent)] hover:border-[var(--accent)] flex items-center justify-center text-xs leading-none transition-colors",
                title: "Connect to another server",
                onmousedown: move |e| {
                    e.stop_propagation();
                    on_add.call(());
                },
                "+"
            }

            if servers().is_empty() {
                span { class: "text-[10px] text-[var(--text-dim)] pl-1",
                    "None yet — messages work without one."
                }
            }
        }
    }
}
