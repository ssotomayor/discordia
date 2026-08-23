//! The rendezvous directory of public *hosts* — one list, two callers.
//!
//! It was written for the connect screen, where it answers "where do I go?".
//! Home asks the same question of the same endpoint once you are already
//! somewhere, so the list lives here rather than being reimplemented against
//! `/discover` a second time and drifting.

use dioxus::prelude::*;

use crate::protocol::rendezvous::DiscoverEntry;

/// How quiet a host has to be before the list stops presenting it as
/// reachable. The rendezvous pings every 20s and unregisters at 60s, so a host
/// past this mark has already missed at least one beat and is on its way out.
pub const HOST_STALE_AFTER_SECS: u64 = 45;

const LABEL: &str = "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)]";

/// Public hosts on one rendezvous. `on_pick` hands back the whole entry
/// because a caller needs more than the shortcode: the connect screen fills a
/// field with it, home dials the address it advertised.
#[component]
pub fn ServerDirectory(
    rendezvous_url: String,
    on_pick: EventHandler<DiscoverEntry>,
    picked_shortcode: String,
    /// Height of the scroll area, so a narrow panel and a full pane can share
    /// the list without one of them growing unbounded.
    #[props(default = "max-h-64".to_string())]
    list_height: String,
) -> Element {
    let mut refresh_tick = use_signal(|| 0u32);
    let url_for_fetch = rendezvous_url.clone();
    let entries = use_resource(move || {
        let _ = refresh_tick();
        let base = ws_to_http(&url_for_fetch);
        async move {
            let url = format!("{base}/discover");
            reqwest::Client::new()
                .get(&url)
                .send()
                .await
                .map_err(|e| format!("fetch: {e}"))?
                .json::<Vec<DiscoverEntry>>()
                .await
                .map_err(|e| format!("decode: {e}"))
        }
    });

    rsx! {
        div { class: "space-y-2",
            div { class: "flex items-center gap-2",
                span { class: "{LABEL} flex-1", "Public servers" }
                button {
                    r#type: "button",
                    class: "text-[10px] text-[var(--accent)] hover:text-[var(--accent-strong)] transition-colors",
                    onclick: move |_| refresh_tick.set(refresh_tick() + 1),
                    "↻ Refresh"
                }
            }

            div { class: "{list_height} overflow-y-auto border border-[var(--border)] rounded",
                match &*entries.read_unchecked() {
                    None => rsx! {
                        div { class: "text-xs text-[var(--text-dim)] px-3 py-4 text-center", "Loading…" }
                    },
                    Some(Err(e)) => rsx! {
                        div { class: "text-xs px-3 py-4 space-y-1",
                            div { class: "text-[var(--danger)]",
                                "Couldn't reach the server directory."
                            }
                            div { class: "text-[var(--text-dim)]",
                                "A code from a friend still works, or create your own server. ({e})"
                            }
                        }
                    },
                    Some(Ok(list)) if list.is_empty() => rsx! {
                        div { class: "text-xs text-[var(--text-dim)] px-3 py-4 text-center",
                            "Nobody has listed a public server on this directory yet. A code from \
                             a friend works without one, or make your own from the Create tab."
                        }
                    },
                    Some(Ok(list)) => rsx! {
                        for entry in list.iter().cloned() {
                            {
                                let sc = entry.shortcode.clone();
                                let selected = picked_shortcode == sc;
                                let row_cls = if selected {
                                    "bg-[var(--accent-soft)] border-l-2 border-[var(--accent)]"
                                } else {
                                    "border-l-2 border-transparent hover:bg-white/[0.02]"
                                };
                                let entry_for_pick = entry.clone();
                                rsx! {
                                    button {
                                        key: "{sc}",
                                        r#type: "button",
                                        class: "w-full text-left px-3 py-2 {row_cls} transition-colors",
                                        onclick: move |_| on_pick.call(entry_for_pick.clone()),
                                        div { class: "flex items-baseline gap-2",
                                            // Rendezvous drops stale hosts up
                                            // to a minute late; this dot shows
                                            // the gap immediately.
                                            span {
                                                class: "w-1.5 h-1.5 rounded-full shrink-0 self-center",
                                                style: if entry.idle_secs >= HOST_STALE_AFTER_SECS {
                                                    "background: var(--warn);"
                                                } else {
                                                    "background: var(--up);"
                                                },
                                                title: if entry.idle_secs >= HOST_STALE_AFTER_SECS {
                                                    "Not responding — this host may already be offline"
                                                } else {
                                                    "Online"
                                                },
                                            }
                                            span { class: "text-sm font-medium text-[var(--text)]",
                                                {entry.name.clone().unwrap_or_else(|| entry.shortcode.clone())}
                                            }
                                            span { class: "text-[10px] text-[var(--text-dim)]", "{entry.shortcode}" }
                                            if entry.idle_secs >= HOST_STALE_AFTER_SECS {
                                                span { class: "text-[10px] text-[var(--warn)]", "not responding" }
                                            }
                                        }
                                        if let Some(d) = entry.description.clone() {
                                            div { class: "text-xs text-[var(--text-muted)] mt-0.5", "{d}" }
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

pub fn ws_to_http(url: &str) -> String {
    if let Some(rest) = url.strip_prefix("wss://") {
        format!("https://{rest}")
    } else if let Some(rest) = url.strip_prefix("ws://") {
        format!("http://{rest}")
    } else {
        url.to_string()
    }
}
