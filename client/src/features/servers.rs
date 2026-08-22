//! The server bar: everywhere this key can go, across the top.
//!
//! Above both views on purpose. Servers are not something you find *inside* a
//! screen — they are the thing you switch between — so the bar sits over the
//! home and the workspace alike rather than belonging to either.
//!
//! **A guild is not a server and the rail is not this.** A server is a host you
//! connect to; a guild is a community inside one. They were sharing a strip of
//! chrome, which is why the rail could read "Guilds" while the only thing in it
//! offered to connect to a server. Now each has its own edge of the window —
//! and the same surface, border and tile size, so they read as two views of one
//! idea rather than two unrelated objects.
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
        div { class: "shrink-0 px-2 pt-2",
            div {
                class: "dxf-drag-region panel-hover w-full bg-[var(--panel)] border border-[var(--border)] rounded-lg flex items-center gap-2 px-3 py-2",
                onmousedown: move |_| crate::app::start_window_drag(),

                span { class: "shrink-0 text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)] pr-1",
                    "Servers"
                }

                div { class: "flex-1 min-w-0 flex items-center gap-2 overflow-x-auto",
                    for saved in servers().into_iter() {
                        {
                            let active = current.as_ref() == Some(&saved.mode);
                            let full = session::label(&saved);
                            let short = short_label(&saved.mode);
                            let initial = short
                                .chars()
                                .next()
                                .unwrap_or('?')
                                .to_ascii_uppercase()
                                .to_string();
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
                                    key: "{full}",
                                    class: "group shrink-0 flex items-center h-10 pl-1 pr-1 rounded-md border transition-colors {cls}",
                                    title: "{full}",
                                    button {
                                        r#type: "button",
                                        class: "flex items-center gap-2 h-full",
                                        // mousedown, not click: the bar around
                                        // this starts a native window drag,
                                        // which swallows the mouseup and with
                                        // it the click.
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
                                        div {
                                            class: "w-8 h-8 shrink-0 rounded-md border border-[var(--edge)] flex items-center justify-center text-sm font-semibold",
                                            style: "background: var(--bg2);",
                                            "{initial}"
                                        }
                                        span { class: "max-w-[180px] truncate text-xs pr-1", "{short}" }
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

                    if servers().is_empty() {
                        span { class: "text-[11px] text-[var(--text-dim)]",
                            "None yet — messages work without one."
                        }
                    }
                }

                button {
                    r#type: "button",
                    class: "shrink-0 w-10 h-10 rounded-md border border-dashed border-[var(--border)] text-[var(--text-muted)] hover:text-[var(--accent)] hover:border-[var(--accent)] flex items-center justify-center text-base leading-none transition-colors",
                    title: "Connect to another server",
                    onmousedown: move |e| {
                        e.stop_propagation();
                        on_add.call(());
                    },
                    "+"
                }
            }
        }
    }
}

/// The part of a server's label worth reading in a tile.
///
/// `session::label` prefixes the kind — "Remote · ws://host:9000" — which is
/// right in a one-line reconnect pill and wrong in a row of them, where every
/// tile would open with the same word and the part that differs is the part
/// that gets truncated away.
fn short_label(mode: &SessionMode) -> String {
    match mode {
        SessionMode::Remote { server_url } => server_url
            .trim_start_matches("ws://")
            .trim_start_matches("wss://")
            .to_string(),
        SessionMode::SelfHost { .. } => "This machine".to_string(),
        SessionMode::ByCode { code, .. } => code.clone(),
    }
}
