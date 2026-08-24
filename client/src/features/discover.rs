//! The rendezvous directory of public *hosts*.
//!
//! Separate from `features::home`, which mounts it, because it is the one part
//! of finding a server that talks to the rendezvous over plain HTTP rather than
//! to a gateway — and because it outlived the connect screen it was written
//! for. `/discover` is read here and nowhere else.

use dioxus::prelude::*;

use crate::protocol::rendezvous::DiscoverEntry;
use crate::state::{SessionMode, host_of};

/// Whether a row is the host this session is already attached to.
///
/// Not cosmetic. "Enter" swaps the session, and the workspace is keyed on the
/// session so it is torn down and rebuilt — for a row you are already on that
/// costs you the live gateway to gain the same one back, and when the row *is*
/// this machine it stops the embedded server the row is advertising. So the
/// directory has to be able to recognise itself, and the answer is not one
/// comparison: how you got here decides what identifies the host.
///
/// - Self-hosting: the code the rendezvous handed us. `None` when this host
///   registered with no rendezvous, and then no row can be us — we are not
///   listed.
/// - By code: the code *is* the join key, for a claimed name as much as for a
///   random shortcode — the registry is keyed by it either way.
/// - By address: the host:port we dialled against the one the row advertises.
///   Best-effort by nature — a host reachable under two addresses is two
///   strings — so a miss here degrades to the old behaviour rather than to a
///   wrong claim.
///
/// `directory_url` is the rendezvous whose listing is being read, and the two
/// code cases require it to be the one that issued the code. A shortcode is
/// only unique within one registry — `adjective-animal-NN` is a small space and
/// every relay draws from it independently — so without this a stranger's host
/// on another directory could inherit the badge, and its Enter button with it.
pub fn is_current_host(
    entry: &DiscoverEntry,
    directory_url: &str,
    mode: Option<&SessionMode>,
    own_shortcode: Option<&str>,
) -> bool {
    let same_directory = |theirs: &str| {
        let a = host_of(theirs.trim());
        !a.is_empty() && a.eq_ignore_ascii_case(&host_of(directory_url.trim()))
    };
    let same_code = |code: &str| {
        let code = code.trim();
        !code.is_empty() && code.eq_ignore_ascii_case(&entry.shortcode)
    };
    match mode {
        Some(SessionMode::SelfHost { rendezvous_url, .. }) => {
            rendezvous_url.as_deref().is_some_and(same_directory)
                && own_shortcode.is_some_and(same_code)
        }
        Some(SessionMode::ByCode {
            rendezvous_url,
            code,
        }) => same_directory(rendezvous_url) && same_code(code),
        // No directory check: an address was dialled without one, and the row
        // advertising that address is that host on whichever relay lists it.
        Some(SessionMode::Remote { server_url }) => entry.endpoint.as_deref().is_some_and(|ep| {
            let ours = host_of(server_url);
            !ours.is_empty() && ours.eq_ignore_ascii_case(&host_of(ep))
        }),
        None => false,
    }
}

/// How quiet a host has to be before the list stops presenting it as
/// reachable. The rendezvous pings every 20s and unregisters at 60s, so a host
/// past this mark has already missed at least one beat and is on its way out.
pub const HOST_STALE_AFTER_SECS: u64 = 45;

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

/// Whether a typed search asks for this host.
///
/// Name, code and description, because all three are ways somebody refers to a
/// server — and the code especially, since pasting one that happens to be
/// listed should find it rather than come back empty.
fn matches_query(entry: &DiscoverEntry, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let name = entry.name.as_deref().unwrap_or_default().to_lowercase();
    let desc = entry
        .description
        .as_deref()
        .unwrap_or_default()
        .to_lowercase();
    name.contains(needle)
        || entry.shortcode.to_lowercase().contains(needle)
        || desc.contains(needle)
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
    let mut query = use_signal(String::new);
    // Read here rather than taken as a prop: every mount of this list wants the
    // same answer, and it is one the app state already holds.
    let state = crate::state::use_app_state();
    let (session_mode, own_shortcode) = {
        let s = state.read();
        (
            s.session_mode.clone(),
            s.host_info.as_ref().and_then(|h| h.shortcode.clone()),
        )
    };
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
                input {
                    class: "flex-1 min-w-[180px] bg-[var(--bg)] border border-[var(--border)] focus:border-[var(--accent)] rounded px-2.5 py-1.5 text-xs text-[var(--text)] outline-none transition-colors",
                    r#type: "text",
                    placeholder: "Search by name, or paste a code\u{2026}",
                    value: "{query}",
                    oninput: move |e| query.set(e.value()),
                }
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
                        let needle = query().trim().to_lowercase();
                        let shown: Vec<DiscoverEntry> = list
                            .iter()
                            .filter(|e| filter().keeps(e))
                            .filter(|e| matches_query(e, &needle))
                            .cloned()
                            .collect();
                        rsx! {
                            if shown.is_empty() {
                                div { class: "text-xs text-[var(--text-dim)] px-3 py-4 text-center border border-dashed border-[var(--border)] rounded",
                                    "No server matches that filter right now."
                                }
                            }
                            for entry in shown {
                                {
                                    let sc = entry.shortcode.clone();
                                    let here = is_current_host(
                                        &entry,
                                        &rendezvous_url,
                                        session_mode.as_ref(),
                                        own_shortcode.as_deref(),
                                    );
                                    let selected = picked_shortcode == sc && !here;
                                    let named = entry.name.is_some();
                                    let title = entry.name.clone().unwrap_or_else(|| sc.clone());
                                    let (fresh_label, fresh_ok) = freshness(entry.idle_secs);
                                    let direct = entry.endpoint.is_some();
                                    let row_cls = if here {
                                        "border-[var(--border-strong)] bg-[var(--panel2)]"
                                    } else if selected {
                                        "border-[var(--accent)] bg-[var(--accent-soft)]"
                                    } else {
                                        "border-[var(--border)] hover:border-[var(--border-strong)]"
                                    };
                                    // The stripe says at a glance which rows
                                    // can be reached without a relay in the
                                    // middle, which is the one property of a
                                    // host worth reading before its name.
                                    let stripe = if direct { "var(--blue, #8fb0ff)" } else { "var(--violet)" };
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
                                            class: "flex items-center gap-3 pl-2.5 pr-3 py-2.5 rounded-lg border transition-colors {row_cls}",
                                            style: "border-left: 3px solid {stripe};",
                                            button {
                                                r#type: "button",
                                                class: "flex-1 min-w-0 flex items-center gap-3 text-left",
                                                style: if here { "cursor: default;" } else { "" },
                                                onclick: move |_| if !here { on_pick.call(for_pick.clone()) },
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
                                                }
                                            }
                                            // Pills sit beside the button, not
                                            // under the description: they are
                                            // what you compare rows by, and a
                                            // column of them reads at a glance.
                                            span { class: "shrink-0 hidden lg:flex items-center gap-1.5",
                                                span {
                                                    class: "px-1.5 py-0.5 rounded-full border text-[9px] font-mono whitespace-nowrap",
                                                    style: if fresh_ok {
                                                        "color: var(--up); border-color: color-mix(in srgb, var(--up) 40%, transparent);"
                                                    } else {
                                                        "color: var(--warn); border-color: color-mix(in srgb, var(--warn) 40%, transparent);"
                                                    },
                                                    "{fresh_label}"
                                                }
                                                span {
                                                    class: "px-1.5 py-0.5 rounded-full border border-[var(--border)] text-[9px] font-mono text-[var(--text-dim)] whitespace-nowrap",
                                                    title: if direct {
                                                        "This host published an address, so the connection can skip the relay"
                                                    } else {
                                                        "No address published — the rendezvous relays every frame, and can read them"
                                                    },
                                                    if direct { "direct" } else { "relayed" }
                                                }
                                            }
                                            // No Enter on the host you are on:
                                            // pressing it would drop the live
                                            // session to dial the same one
                                            // again.
                                            if here {
                                                span {
                                                    class: "shrink-0 px-2.5 py-1 rounded-full border text-[10px] whitespace-nowrap",
                                                    style: "color: var(--up); border-color: color-mix(in srgb, var(--up) 40%, transparent);",
                                                    title: "This session is already on this server",
                                                    "You're here"
                                                }
                                            } else {
                                                button {
                                                    r#type: "button",
                                                    class: "dxf-cta shrink-0 px-3 py-1.5 rounded text-[11px]",
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

    fn entry(name: Option<&str>, shortcode: &str, description: Option<&str>) -> DiscoverEntry {
        DiscoverEntry {
            shortcode: shortcode.into(),
            name: name.map(Into::into),
            description: description.map(Into::into),
            idle_secs: 0,
            endpoint: None,
            transport_key: None,
            transport_addrs: Vec::new(),
            relay_url: None,
        }
    }

    /// An empty box asks for everything, or typing would start by hiding the
    /// list it is meant to narrow.
    #[test]
    fn an_empty_search_keeps_every_host() {
        assert!(matches_query(
            &entry(Some("rust-sur"), "rust-sur", None),
            ""
        ));
    }

    /// Pasting a code that happens to be listed should find its row, not come
    /// back empty because the search only looked at names.
    #[test]
    fn a_pasted_code_finds_its_row() {
        let e = entry(Some("hormiguero"), "viento-tapir-04", None);
        assert!(matches_query(&e, "viento-tapir"));
    }

    /// The description is where the reason to join is written, so it is worth
    /// searching even though it is not an identifier.
    #[test]
    fn the_description_is_searchable() {
        let e = entry(Some("nexo"), "nexo", Some("Rust en espanol"));
        assert!(matches_query(&e, "rust"));
        assert!(!matches_query(&e, "haskell"));
    }

    const RZ: &str = "wss://rz.example";

    fn by_code(code: &str) -> SessionMode {
        SessionMode::ByCode {
            rendezvous_url: RZ.into(),
            code: code.into(),
        }
    }

    fn self_host(rendezvous: Option<&str>) -> SessionMode {
        SessionMode::SelfHost {
            allow_lan: true,
            rendezvous_url: rendezvous.map(Into::into),
            publish_name: None,
            description: None,
            publish_public: true,
        }
    }

    /// The case that was reported: the row you arrived through is still in the
    /// list, and entering it again costs the session you already have.
    #[test]
    fn the_code_you_arrived_by_marks_its_own_row() {
        let e = entry(Some("hormiguero"), "viento-tapir-04", None);
        assert!(is_current_host(
            &e,
            RZ,
            Some(&by_code("viento-tapir-04")),
            None
        ));
        assert!(!is_current_host(
            &e,
            RZ,
            Some(&by_code("otro-codigo-11")),
            None
        ));
    }

    /// A claimed name is the join code, so it matches the same way — and
    /// codes are lowercase by convention rather than by rule.
    #[test]
    fn a_named_host_matches_on_its_name() {
        let e = entry(Some("Hormiguero"), "hormiguero", None);
        assert!(is_current_host(
            &e,
            "https://rz.example/",
            Some(&by_code(" Hormiguero ")),
            None
        ));
    }

    /// A shortcode is unique to one registry, so the same one on another
    /// directory is a stranger — and must keep its Enter button.
    #[test]
    fn the_same_code_on_another_directory_is_someone_else() {
        let e = entry(None, "viento-tapir-04", None);
        assert!(!is_current_host(
            &e,
            "wss://otra.example",
            Some(&by_code("viento-tapir-04")),
            None
        ));
    }

    /// Your own listed host is the worst version of the bug: entering it stops
    /// the server the row is advertising.
    #[test]
    fn your_own_host_knows_itself() {
        let e = entry(None, "purple-fox-42", None);
        let mine = self_host(Some(RZ));
        assert!(is_current_host(&e, RZ, Some(&mine), Some("purple-fox-42")));
        assert!(!is_current_host(&e, RZ, Some(&mine), Some("verde-oso-07")));
    }

    /// Hosting without a rendezvous means being in no directory at all, so no
    /// row may claim to be us.
    #[test]
    fn an_unregistered_host_matches_nothing() {
        let e = entry(None, "purple-fox-42", None);
        assert!(!is_current_host(
            &e,
            RZ,
            Some(&self_host(None)),
            Some("purple-fox-42")
        ));
        assert!(!is_current_host(&e, RZ, Some(&self_host(Some(RZ))), None));
        assert!(!is_current_host(
            &e,
            RZ,
            Some(&self_host(Some(RZ))),
            Some("")
        ));
    }

    /// Typed an address, then found the same host listed: the endpoint is the
    /// only thing the two have in common, and it holds on any directory.
    #[test]
    fn an_address_session_matches_the_advertised_endpoint() {
        let mut e = entry(Some("nexo"), "nexo", None);
        e.endpoint = Some("ws://203.0.113.9:9000".into());
        let mine = SessionMode::Remote {
            server_url: "ws://203.0.113.9:9000/gateway".into(),
        };
        assert!(is_current_host(&e, "wss://otra.example", Some(&mine), None));
        let other = SessionMode::Remote {
            server_url: "ws://198.51.100.4:9000/gateway".into(),
        };
        assert!(!is_current_host(&e, RZ, Some(&other), None));
    }

    /// A relay-only row publishes no address, so an address session has
    /// nothing to compare and must not guess.
    #[test]
    fn a_relayed_row_cannot_be_matched_by_address() {
        let e = entry(Some("nexo"), "nexo", None);
        let mine = SessionMode::Remote {
            server_url: "ws://203.0.113.9:9000/gateway".into(),
        };
        assert!(!is_current_host(&e, RZ, Some(&mine), None));
    }

    /// Offline every row is somewhere to go.
    #[test]
    fn with_no_session_no_row_is_current() {
        let e = entry(Some("nexo"), "nexo", None);
        assert!(!is_current_host(&e, RZ, None, Some("nexo")));
    }
}
