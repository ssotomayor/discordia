//! The rendezvous directory of public *hosts*.
//!
//! Separate from `features::home`, which mounts it, because it is the one part
//! of finding a server that talks to the rendezvous over plain HTTP rather than
//! to a gateway — and because it outlived the connect screen it was written
//! for. `/discover` is read here and nowhere else.

use dioxus::prelude::*;

use crate::protocol::rendezvous::DiscoverEntry;

/// How quiet a host has to be before the list stops presenting it as
/// reachable. The rendezvous pings every 20s and unregisters at 60s, so a host
/// past this mark has already missed at least one beat and is on its way out.
pub const HOST_STALE_AFTER_SECS: u64 = 45;

const LABEL: &str = "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)]";

/// How a host's last heartbeat reads on its row.
///
/// The rendezvous pings every 20s and drops a host at 60s, so there is a
/// window where an entry is still listed and already gone. Saying *when* it
/// last answered is the honest version of a green dot: the reader can decide
/// whether two minutes of silence matters to them.
pub fn freshness(idle_secs: u64) -> (String, bool) {
    match idle_secs {
        0..=25 => ("active now".to_string(), true),
        s if s < HOST_STALE_AFTER_SECS => (format!("active {s}s ago"), true),
        s if s < 3600 => (format!("quiet {}m", s / 60), false),
        s => (format!("quiet {}h", s / 3600), false),
    }
}

/// Which hosts a filter chip lets through.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    Active,
    Named,
}

impl Filter {
    fn keeps(self, entry: &DiscoverEntry) -> bool {
        match self {
            Self::All => true,
            Self::Active => entry.idle_secs < HOST_STALE_AFTER_SECS,
            Self::Named => entry.name.is_some(),
        }
    }
}

/// Public hosts on one rendezvous.
///
/// `on_pick` fills the caller's code field (and highlights the row); `on_enter`
/// is the row's own button, because a directory whose every row needs a second
/// control below it to act on is a list, not a directory.
#[component]
pub fn ServerDirectory(
    rendezvous_url: String,
    on_pick: EventHandler<DiscoverEntry>,
    on_enter: EventHandler<DiscoverEntry>,
    picked_shortcode: String,
    /// Height of the scroll area, so a narrow panel and a full pane can share
    /// the list without one of them growing unbounded.
    #[props(default = "max-h-64".to_string())]
    list_height: String,
) -> Element {
    let mut refresh_tick = use_signal(|| 0u32);
    let mut filter = use_signal(|| Filter::All);
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

    let chip = |f: Filter, label: &'static str, current: Filter| {
        let on = f == current;
        let cls = if on {
            "px-2 py-0.5 rounded-full border border-[var(--border-strong)] bg-[var(--accent-soft)] text-[10px] text-[var(--accent)]"
        } else {
            "px-2 py-0.5 rounded-full border border-[var(--border)] text-[10px] text-[var(--text-dim)] hover:text-[var(--text)] transition-colors"
        };
        (cls, label)
    };
    let (all_cls, all_label) = chip(Filter::All, "All", filter());
    let (active_cls, active_label) = chip(Filter::Active, "Active now", filter());
    let (named_cls, named_label) = chip(Filter::Named, "Named", filter());

    rsx! {
        div { class: "space-y-2",
            div { class: "flex items-center gap-2 flex-wrap",
                span { class: "{LABEL}", "Public servers" }
                button {
                    r#type: "button",
                    class: "{all_cls}",
                    onclick: move |_| filter.set(Filter::All),
                    "{all_label}"
                }
                button {
                    r#type: "button",
                    class: "{active_cls}",
                    onclick: move |_| filter.set(Filter::Active),
                    "{active_label}"
                }
                button {
                    r#type: "button",
                    class: "{named_cls}",
                    onclick: move |_| filter.set(Filter::Named),
                    "{named_label}"
                }
                span { class: "flex-1" }
                button {
                    r#type: "button",
                    class: "text-[10px] text-[var(--accent)] hover:text-[var(--accent-strong)] transition-colors",
                    onclick: move |_| refresh_tick.set(refresh_tick() + 1),
                    "\u{21bb} Refresh"
                }
            }

            div { class: "{list_height} overflow-y-auto space-y-1.5 pr-0.5",
                match &*entries.read_unchecked() {
                    None => rsx! {
                        div { class: "text-xs text-[var(--text-dim)] px-3 py-4 text-center border border-[var(--border)] rounded", "Loading\u{2026}" }
                    },
                    Some(Err(e)) => rsx! {
                        div { class: "text-xs px-3 py-4 space-y-1 border border-[var(--border)] rounded",
                            div { class: "text-[var(--danger)]",
                                "Couldn't reach the server directory."
                            }
                            div { class: "text-[var(--text-dim)]",
                                "A code from a friend still works, and hosting your own needs no directory at all. ({e})"
                            }
                        }
                    },
                    Some(Ok(list)) if list.is_empty() => rsx! {
                        div { class: "text-xs text-[var(--text-dim)] px-3 py-4 text-center border border-dashed border-[var(--border)] rounded",
                            "Nobody has listed a public server on this directory yet. A code from \
                             a friend works without one, or host your own below."
                        }
                    },
                    Some(Ok(list)) => {
                        let shown: Vec<DiscoverEntry> =
                            list.iter().filter(|e| filter().keeps(e)).cloned().collect();
                        rsx! {
                            if shown.is_empty() {
                                div { class: "text-xs text-[var(--text-dim)] px-3 py-4 text-center border border-dashed border-[var(--border)] rounded",
                                    "No server matches that filter right now."
                                }
                            }
                            for entry in shown {
                                {
                                    let sc = entry.shortcode.clone();
                                    let selected = picked_shortcode == sc;
                                    let named = entry.name.is_some();
                                    let title = entry.name.clone().unwrap_or_else(|| sc.clone());
                                    let (fresh_label, fresh_ok) = freshness(entry.idle_secs);
                                    let direct = entry.endpoint.is_some();
                                    let row_cls = if selected {
                                        "border-[var(--accent)] bg-[var(--accent-soft)]"
                                    } else {
                                        "border-[var(--border)] hover:border-[var(--border-strong)]"
                                    };
                                    let initials: String = title
                                        .split(|c: char| !c.is_alphanumeric())
                                        .filter(|w| !w.is_empty())
                                        .filter_map(|w| w.chars().next())
                                        .take(2)
                                        .collect::<String>()
                                        .to_uppercase();
                                    let for_pick = entry.clone();
                                    let for_enter = entry.clone();
                                    rsx! {
                                        div {
                                            key: "{sc}",
                                            class: "flex items-center gap-3 px-3 py-2.5 rounded-lg border transition-colors {row_cls}",
                                            button {
                                                r#type: "button",
                                                class: "flex-1 min-w-0 flex items-center gap-3 text-left",
                                                onclick: move |_| on_pick.call(for_pick.clone()),
                                                span { class: "w-9 h-9 shrink-0 rounded-lg border border-[var(--border)] flex items-center justify-center text-xs text-[var(--text-muted)]",
                                                    "{initials}"
                                                }
                                                span { class: "flex-1 min-w-0",
                                                    span { class: "flex items-baseline gap-2",
                                                        span { class: "truncate text-sm text-[var(--text)]", "{title}" }
                                                        if named {
                                                            span {
                                                                class: "shrink-0 text-[9px] text-[var(--text-dim)]",
                                                                title: "A reserved name, proved with the host's key",
                                                                "\u{1f511}"
                                                            }
                                                        }
                                                    }
                                                    span { class: "block truncate font-mono text-[10px] text-[var(--text-dim)]", "{sc}" }
                                                    if let Some(d) = entry.description.clone() {
                                                        span { class: "block truncate text-[11px] text-[var(--text-muted)] mt-0.5", "{d}" }
                                                    }
                                                    span { class: "flex items-center gap-1.5 mt-1",
                                                        span {
                                                            class: "px-1.5 py-0.5 rounded-full border text-[9px] font-mono",
                                                            style: if fresh_ok {
                                                                "color: var(--up); border-color: color-mix(in srgb, var(--up) 40%, transparent);"
                                                            } else {
                                                                "color: var(--warn); border-color: color-mix(in srgb, var(--warn) 40%, transparent);"
                                                            },
                                                            "{fresh_label}"
                                                        }
                                                        span {
                                                            class: "px-1.5 py-0.5 rounded-full border border-[var(--border)] text-[9px] font-mono text-[var(--text-dim)]",
                                                            title: if direct {
                                                                "This host published an address, so the connection can skip the relay"
                                                            } else {
                                                                "No address published — the rendezvous relays every frame, and can read them"
                                                            },
                                                            if direct { "direct" } else { "relayed" }
                                                        }
                                                    }
                                                }
                                            }
                                            button {
                                                r#type: "button",
                                                class: "shrink-0 px-3 py-1.5 rounded text-[10px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors",
                                                onclick: move |_| on_enter.call(for_enter.clone()),
                                                "Enter"
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A host that answered within the ping interval is simply up, and saying
    /// "active 3s ago" about it is noise.
    #[test]
    fn a_fresh_host_reads_as_up() {
        assert_eq!(freshness(3), ("active now".to_string(), true));
    }

    /// Between one missed beat and the drop, the number is the useful part.
    #[test]
    fn a_late_host_shows_how_late() {
        let (label, ok) = freshness(40);
        assert_eq!(label, "active 40s ago");
        assert!(ok);
    }

    /// Past the stale mark it stops claiming the host is reachable, which is
    /// the whole reason the field is on the wire.
    #[test]
    fn a_stale_host_stops_claiming_it_is_up() {
        let (label, ok) = freshness(600);
        assert_eq!(label, "quiet 10m");
        assert!(!ok);
    }

    /// Hours, because "quiet 180m" is not a duration anybody reads.
    #[test]
    fn a_long_silence_is_counted_in_hours() {
        assert_eq!(freshness(7_400).0, "quiet 2h");
    }
}
