//! Home — the surface that belongs to you rather than to a guild.
//!
//! It carries two jobs at once. **Talking** is the constant: DMs are NIP-17
//! events on Nostr relays, keyed to your identity rather than to this gateway,
//! so they are reachable here without entering anything. **Finding** is the
//! one that changes shape, because the thing worth finding depends on where
//! you already are — see `primary_explore`.

use dioxus::prelude::*;

use crate::protocol::rendezvous::DiscoverEntry;
use crate::protocol::{ClientMessage, GuildSummary};
use crate::state::{HomeView, SessionMode, use_app_state, use_gateway};

/// Which explore pane home leads with.
///
/// The two are not interchangeable: communities live *inside* a host, so
/// browsing them is only a question you can ask once you have arrived at one.
/// A host with nothing to offer — no community you could join, none you have —
/// leaves the directory of *other hosts* as the only useful next step, which is
/// exactly the case a newcomer to a bare server lands in.
///
/// Note this deliberately does not read "am I connected to a server", which in
/// this client is always true past the connect screen: the workspace only
/// exists inside a session. Joining a community is the closest thing to the
/// arrival the rule is about.
pub fn primary_explore(joined_communities: usize, joinable_communities: usize) -> HomeView {
    if joined_communities > 0 || joinable_communities > 0 {
        HomeView::Communities
    } else {
        HomeView::Servers
    }
}

const ROW: &str = "w-full flex items-center gap-2 px-2 py-1.5 rounded text-left transition-colors";
const ROW_IDLE: &str = "text-[var(--text-muted)] hover:text-[var(--text)] hover:bg-white/[0.03]";
const ROW_ON: &str = "text-[var(--accent)] bg-[var(--accent-soft)]";
const GLYPH: &str = "w-7 h-7 shrink-0 rounded-md border border-[var(--border)] flex items-center justify-center text-xs";
const LABEL: &str = "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-dim)]";

/// The explore rows that sit above the DM list in home's column.
///
/// Both levels are always offered; only their order changes. Hiding one would
/// make "how do I get to another server" unanswerable from the place people
/// spend their time, which is the problem the redesign started from.
#[component]
pub fn HomeNav() -> Element {
    let mut state = use_app_state();
    let gateway = use_gateway();

    let snapshot = state.read();
    let view = snapshot.home_view;
    let joinable = snapshot.joinable_communities().len();
    let joined = snapshot.joined_communities();
    drop(snapshot);

    let communities_first = primary_explore(joined, joinable) == HomeView::Communities;

    let gw = gateway.clone();
    let mut open_communities = move || {
        // Pull the latest directory on open — the server no longer pushes
        // catalog updates to everyone.
        gw.send(ClientMessage::FetchCatalog {
            offset: 0,
            limit: 0,
        });
        let mut s = state.write();
        s.dm_mode = true;
        s.home_view = HomeView::Communities;
    };
    let mut open_servers = move || {
        let mut s = state.write();
        s.dm_mode = true;
        s.home_view = HomeView::Servers;
    };

    let cls = |on: bool| format!("{ROW} {}", if on { ROW_ON } else { ROW_IDLE });
    let cls_communities = cls(view == HomeView::Communities);
    let cls_servers = cls(view == HomeView::Servers);
    let cls_dms = cls(view == HomeView::Dms);

    let communities = rsx! {
        button {
            class: "{cls_communities}",
            onclick: move |_| open_communities(),
            span { class: GLYPH, "◎" }
            span { class: "flex-1 min-w-0",
                span { class: "block text-sm truncate", "Browse communities" }
                span { class: "block text-[10px] text-[var(--text-dim)]",
                    if joinable == 1 { "1 you haven't joined" } else { "{joinable} you haven't joined" }
                }
            }
        }
    };
    let servers = rsx! {
        button {
            class: "{cls_servers}",
            onclick: move |_| open_servers(),
            span { class: GLYPH, "⇥" }
            span { class: "flex-1 min-w-0",
                span { class: "block text-sm truncate",
                    if communities_first { "Another server" } else { "Find a server" }
                }
                span { class: "block text-[10px] text-[var(--text-dim)]",
                    "directory · code · host your own"
                }
            }
        }
    };

    rsx! {
        div { class: "px-1 pb-2 space-y-0.5 border-b border-[var(--border)] mb-2",
            if communities_first {
                {communities}
                {servers}
            } else {
                {servers}
                {communities}
            }
            button {
                class: "{cls_dms}",
                onclick: move |_| {
                    let mut s = state.write();
                    s.dm_mode = true;
                    s.home_view = HomeView::Dms;
                },
                span { class: GLYPH, "✉" }
                span { class: "flex-1 min-w-0 text-sm truncate", "Direct messages" }
            }
        }
    }
}

/// What fills home's main area. `Dms` is not handled here — that is the chat
/// view, which the workspace keeps rendering.
#[component]
pub fn HomePane(on_switch: EventHandler<SessionMode>) -> Element {
    let view = use_app_state().read().home_view;
    rsx! {
        match view {
            HomeView::Communities => rsx! { CommunitiesPane {} },
            HomeView::Servers => rsx! { ServersPane { on_switch } },
            HomeView::Dms => rsx! { Fragment {} },
        }
    }
}

/// This host's public guild catalog: the level-2 answer.
#[component]
fn CommunitiesPane() -> Element {
    let mut state = use_app_state();
    let gateway = use_gateway();

    let snapshot = state.read();
    let joinable: Vec<GuildSummary> = snapshot.joinable_communities();
    let mine: Vec<(crate::protocol::Id, String, Option<String>)> = snapshot
        .guilds
        .iter()
        .filter(|g| !g.owner_pubkey.is_empty())
        .map(|g| (g.id, g.name.clone(), g.icon.clone()))
        .collect();
    let fetched = snapshot.catalog.len();
    let total = snapshot.catalog_total as usize;
    drop(snapshot);

    rsx! {
        div { class: "flex-1 overflow-y-auto p-4 space-y-4",
            div { class: "flex items-baseline gap-2",
                h2 { class: "text-base font-medium text-[var(--text)]", "Communities here" }
                span { class: "text-[11px] text-[var(--text-dim)]",
                    "on the server you're connected to"
                }
            }

            if joinable.is_empty() {
                div { class: "border border-dashed border-[var(--border)] rounded-lg px-4 py-6 text-center space-y-1",
                    div { class: "text-sm text-[var(--text-muted)]",
                        "Nothing left to join on this server."
                    }
                    div { class: "text-xs text-[var(--text-dim)]",
                        "Create one with + in the guild rail, or look for another server."
                    }
                }
            } else {
                div { class: LABEL, "You haven't joined" }
                div { class: "space-y-2",
                    for g in joinable.iter().cloned() {
                        {
                            let gid = g.id;
                            let gw = gateway.clone();
                            let label = g.icon.clone().unwrap_or_else(|| initials(&g.name));
                            rsx! {
                                div {
                                    key: "{gid}",
                                    class: "flex items-center gap-3 px-3 py-2.5 rounded-lg bg-[var(--panel)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors",
                                    span { class: "w-10 h-10 shrink-0 rounded-lg border border-[var(--border)] flex items-center justify-center text-sm text-[var(--text-muted)]",
                                        "{label}"
                                    }
                                    div { class: "flex-1 min-w-0",
                                        div { class: "text-sm text-[var(--text)] truncate", "{g.name}" }
                                        div { class: "text-[11px] text-[var(--text-dim)]",
                                            if g.member_count == 1 { "1 member" } else { "{g.member_count} members" }
                                        }
                                    }
                                    button {
                                        class: "px-3 py-1 rounded text-[10px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors",
                                        onclick: move |_| gw.send(ClientMessage::JoinGuild {
                                            guild_id: gid,
                                            accept: false,
                                            pow_nonce: None,
                                        }),
                                        "Join"
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // The server answers `FetchCatalog` a page at a time, so a host
            // with a long directory needs this to reach the rest of it.
            if fetched < total {
                {
                    let gw = gateway.clone();
                    rsx! {
                        div { class: "flex justify-center",
                            button {
                                class: "text-[11px] uppercase tracking-wider text-[var(--text-muted)] border border-[var(--border)] rounded px-3 py-1 hover:text-[var(--accent)] hover:border-[var(--accent)] transition-colors",
                                onclick: move |_| gw.send(ClientMessage::FetchCatalog {
                                    offset: fetched as u32,
                                    limit: 0,
                                }),
                                "Load more"
                            }
                        }
                    }
                }
            }

            if !mine.is_empty() {
                div { class: LABEL, "You're in" }
                div { class: "flex flex-wrap gap-2",
                    for (gid, name, icon) in mine.iter().cloned() {
                        {
                            let label = icon.unwrap_or_else(|| initials(&name));
                            rsx! {
                                button {
                                    key: "{gid}",
                                    class: "flex items-center gap-2 px-2.5 py-1.5 rounded-lg bg-[var(--panel)] border border-[var(--border)] hover:border-[var(--accent)] text-left transition-colors",
                                    onclick: move |_| {
                                        let mut s = state.write();
                                        s.dm_mode = false;
                                        s.home_view = HomeView::Dms;
                                        s.selected_guild = Some(gid);
                                        s.selected_channel = None;
                                    },
                                    span { class: "w-6 h-6 shrink-0 rounded border border-[var(--border)] flex items-center justify-center text-[10px] text-[var(--text-muted)]",
                                        "{label}"
                                    }
                                    span { class: "text-xs text-[var(--text)]", "{name}" }
                                }
                            }
                        }
                    }
                }
            }

            InviteJoinRow {}
        }
    }
}

/// The rendezvous directory of other hosts: the level-1 answer.
#[component]
fn ServersPane(on_switch: EventHandler<SessionMode>) -> Element {
    let settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let rendezvous = settings.read().active_rendezvous();
    let mut code = use_signal(String::new);

    let rz_for_go = rendezvous.clone();
    let go = move || {
        let c = code().trim().to_string();
        if c.is_empty() {
            return;
        }
        // Same mode the connect screen submits for a picked entry: `ByCode`
        // resolves the host's advertised address first and only falls back to
        // the relay, so this is the direct path when there is one.
        on_switch.call(SessionMode::ByCode {
            rendezvous_url: rz_for_go.clone(),
            code: c,
        });
    };

    rsx! {
        div { class: "flex-1 overflow-y-auto p-4 space-y-4",
            div { class: "flex items-baseline gap-2",
                h2 { class: "text-base font-medium text-[var(--text)]", "Servers" }
                span { class: "text-[11px] text-[var(--text-dim)]", "{rendezvous}" }
            }

            div { class: "text-xs text-[var(--text-muted)] leading-relaxed",
                "Each server is a machine somebody runs. Joining one swaps this session for
                 that one — your direct messages come with you, they belong to your key."
            }

            crate::features::discover::ServerDirectory {
                on_pick: move |entry: DiscoverEntry| code.set(entry.shortcode),
                picked_shortcode: code(),
                rendezvous_url: rendezvous.clone(),
                list_height: "max-h-72".to_string(),
            }

            form {
                class: "flex items-center gap-2",
                onsubmit: move |_| go(),
                input {
                    class: "flex-1 bg-transparent border border-[var(--border)] focus:border-[var(--accent)] rounded px-3 py-2 text-sm font-mono text-[var(--text)] outline-none transition-colors lowercase",
                    placeholder: "purple-fox-42 or a server name",
                    value: "{code}",
                    oninput: move |e| code.set(e.value()),
                }
                button {
                    r#type: "submit",
                    class: "dxf-cta px-4 py-2 rounded text-sm disabled:opacity-40",
                    disabled: code().trim().is_empty(),
                    "Go"
                }
            }

            div { class: "text-[11px] text-[var(--text-dim)] leading-relaxed",
                "To host your own, or to reach a server by address, disconnect with the plug
                 button — those need a session that hasn't started yet."
            }
        }
    }
}

/// The strip that keeps the other level visible while you're reading a DM.
///
/// Without it the two jobs take turns: opening a conversation would hide every
/// way to reach a community or another server until you navigated back.
#[component]
pub fn ExploreStrip() -> Element {
    let mut state = use_app_state();
    let gateway = use_gateway();

    let snapshot = state.read();
    let joinable = snapshot.joinable_communities();
    let joined = snapshot.joined_communities();
    drop(snapshot);

    let communities_first = primary_explore(joined, joinable.len()) == HomeView::Communities;
    // Four is what fits beside the two buttons at the narrowest the chat panel
    // is usable at; the rest are one click away in the pane.
    let shown: Vec<GuildSummary> = joinable.iter().take(4).cloned().collect();
    let gw_fetch = gateway.clone();

    rsx! {
        div { class: "shrink-0 border-t border-[var(--border)] bg-[var(--panel)] px-3 py-2 flex items-center gap-2 overflow-x-auto",
            span { class: "{LABEL} shrink-0",
                if communities_first { "Communities" } else { "Servers" }
            }
            if communities_first {
                for g in shown.iter().cloned() {
                    {
                        let gid = g.id;
                        let gw = gateway.clone();
                        let label = g.icon.clone().unwrap_or_else(|| initials(&g.name));
                        rsx! {
                            div {
                                key: "{gid}",
                                class: "shrink-0 flex items-center gap-2 px-2 py-1 rounded-lg bg-[var(--panel2)] border border-[var(--border)]",
                                span { class: "w-5 h-5 rounded border border-[var(--border)] flex items-center justify-center text-[9px] text-[var(--text-muted)]",
                                    "{label}"
                                }
                                span { class: "text-xs text-[var(--text)] whitespace-nowrap", "{g.name}" }
                                button {
                                    class: "text-[10px] uppercase tracking-wider text-[var(--accent)] hover:text-[var(--accent-strong)] transition-colors",
                                    onclick: move |_| gw.send(ClientMessage::JoinGuild {
                                        guild_id: gid,
                                        accept: false,
                                        pow_nonce: None,
                                    }),
                                    "Join"
                                }
                            }
                        }
                    }
                }
                if shown.is_empty() {
                    span { class: "text-[11px] text-[var(--text-dim)] whitespace-nowrap",
                        "Nothing left to join here."
                    }
                }
            } else {
                span { class: "text-[11px] text-[var(--text-dim)] whitespace-nowrap",
                    "You haven't joined a community on this server yet."
                }
            }

            div { class: "flex-1" }
            button {
                class: "shrink-0 text-[10px] uppercase tracking-wider text-[var(--text-muted)] border border-[var(--border)] rounded px-2.5 py-1 hover:text-[var(--accent)] hover:border-[var(--accent)] transition-colors",
                onclick: move |_| {
                    gw_fetch.send(ClientMessage::FetchCatalog { offset: 0, limit: 0 });
                    state.write().home_view = HomeView::Communities;
                },
                "Communities"
            }
            button {
                class: "shrink-0 text-[10px] uppercase tracking-wider text-[var(--text-muted)] border border-[var(--border)] rounded px-2.5 py-1 hover:text-[var(--accent)] hover:border-[var(--accent)] transition-colors",
                onclick: move |_| state.write().home_view = HomeView::Servers,
                "Servers"
            }
        }
    }
}

/// "Have an invite code?" — how you reach a community that isn't listed. The
/// server replies `GuildJoined` (which auto-selects the guild) or an `Error`
/// toast.
#[component]
fn InviteJoinRow() -> Element {
    let gateway = use_gateway();
    let mut code = use_signal(String::new);

    let mut submit = move || {
        let c = code().trim().to_string();
        if c.is_empty() {
            return;
        }
        gateway.send(ClientMessage::JoinByInvite {
            code: c,
            accept: false,
            pow_nonce: None,
        });
        code.set(String::new());
    };

    rsx! {
        form {
            class: "border-t border-[var(--border)] pt-3 flex items-center gap-2",
            onsubmit: move |_| submit(),
            input {
                class: "flex-1 bg-transparent border border-[var(--border)] focus:border-[var(--accent)] rounded px-2 py-1.5 text-xs font-mono text-[var(--text)] outline-none transition-colors",
                placeholder: "Have an invite code?",
                value: "{code}",
                oninput: move |e| code.set(e.value()),
            }
            button {
                r#type: "submit",
                class: "px-3 py-1.5 rounded text-[10px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors disabled:opacity-40",
                disabled: code().trim().is_empty(),
                "Join"
            }
        }
    }
}

fn initials(name: &str) -> String {
    name.split_whitespace()
        .filter_map(|w| w.chars().next())
        .take(2)
        .collect::<String>()
        .to_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The case the rule exists for: you arrived somewhere with nothing to
    /// join, so the only useful next step is somebody else's server.
    #[test]
    fn a_bare_server_leads_with_the_directory() {
        assert_eq!(primary_explore(0, 0), HomeView::Servers);
    }

    /// A host with a catalog is worth browsing before leaving it.
    #[test]
    fn something_to_join_here_leads_with_communities() {
        assert_eq!(primary_explore(0, 3), HomeView::Communities);
    }

    /// Having joined is what "you're in" means, and it holds even once there
    /// is nothing left to join — otherwise settling in would flip home back to
    /// the directory, which reads as being asked to leave.
    #[test]
    fn joining_everything_does_not_send_you_away() {
        assert_eq!(primary_explore(4, 0), HomeView::Communities);
    }
}
