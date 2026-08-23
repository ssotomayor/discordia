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
use crate::state::{ConnectionStatus, HomeView, SessionMode, use_app_state, use_gateway};

/// Renaming and signing out, in context because the identity card now lives in
/// home's column while the identity itself is owned by `App`.
#[derive(Clone, Copy)]
pub struct IdentityActions {
    pub on_rename: EventHandler<String>,
    pub on_sign_out: EventHandler<()>,
}

/// Which explore pane home leads with: the level you are missing.
///
/// The two are not interchangeable. Communities live *inside* a host, so
/// browsing them is a question you can only ask once you have arrived at one —
/// with no gateway there is no catalog to read, and the directory of hosts is
/// the only move there is. Past that, a host with nothing to offer (nothing
/// you could join, nothing you have) leaves the same answer standing.
pub fn primary_explore(
    offline: bool,
    joined_communities: usize,
    joinable_communities: usize,
) -> HomeView {
    if offline {
        HomeView::Servers
    } else if joined_communities > 0 || joinable_communities > 0 {
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
    let offline = snapshot.status == ConnectionStatus::Offline;
    let server_label = snapshot.server_label.clone();
    drop(snapshot);

    // Offline there is no host, so there is no catalog to browse and the row
    // is omitted rather than shown dead. The other door is the whole point of
    // being here.
    let communities_first = primary_explore(offline, joined, joinable) == HomeView::Communities;

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
                    if offline { "you're not on one yet" } else { "directory · code · host your own" }
                }
            }
        }
    };

    rsx! {
        div { class: "px-1 pb-2 space-y-0.5 border-b border-[var(--border)] mb-2",
            // Which server this session is attached to, and a way off it. The
            // rail below shows the communities *inside* it, which is not the
            // same question and never answered this one.
            if let Some(label) = server_label.clone() {
                button {
                    class: "w-full flex items-center gap-2 px-2 py-1.5 mb-1 rounded border border-[var(--border)] bg-[var(--panel2)] text-left hover:border-[var(--accent)] transition-colors",
                    title: "The server this session is on. Opens the directory to change it.",
                    onclick: move |_| open_servers(),
                    span { class: "text-[9px] font-mono uppercase tracking-wider text-[var(--text-dim)]", "on" }
                    span { class: "flex-1 min-w-0 truncate text-xs text-[var(--text)]", "{label}" }
                    span { class: "shrink-0 text-[var(--text-dim)] text-[10px]", "\u{25be}" }
                }
            }
            if offline {
                {servers}
            } else if communities_first {
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
                                    onclick: {
                                        let gateway = gateway.clone();
                                        move |_| {
                                            // Same path the rail takes, and for
                                            // the same reason: setting
                                            // `selected_guild` alone lands you
                                            // in a guild with no channel open
                                            // and no history fetched.
                                            let target = {
                                                let mut s = state.write();
                                                s.dm_mode = false;
                                                s.home_view = HomeView::Dms;
                                                s.selected_guild = Some(gid);
                                                s.selected_channel = None;
                                                s.default_channel_of(gid)
                                            };
                                            if let Some(cid) = target {
                                                crate::features::channels::select_text_channel(
                                                    &mut state.clone(),
                                                    &gateway,
                                                    cid,
                                                );
                                            }
                                        }
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
    let mut settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let rendezvous = settings.read().active_rendezvous();
    let state = use_app_state();
    let snapshot = state.read();
    let offline = snapshot.status == ConnectionStatus::Offline;
    let open_host = snapshot.home_open_host;
    drop(snapshot);
    let mut code = use_signal(String::new);
    // Read once: it is a file, and it cannot change while this pane is open.
    let last_session = use_hook(|| crate::session::load().ok().flatten());

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

    // Picking a row fills the field rather than connecting, so the button has
    // to say what the pick will do — otherwise the click above it looks like it
    // did nothing at all.
    let go_label = match code().trim() {
        "" => "Connect".to_string(),
        picked => format!("Connect to {picked}"),
    };

    rsx! {
        div { class: "flex-1 overflow-y-auto p-4 space-y-4",
            div { class: "flex items-baseline gap-2",
                h2 { class: "text-base font-medium text-[var(--text)]", "Servers" }
                span { class: "text-[11px] text-[var(--text-dim)]", "{rendezvous}" }
            }

            div { class: "text-xs text-[var(--text-muted)] leading-relaxed",
                if offline {
                    "Each server is a machine somebody runs — it gives you communities, channels
                     and voice. Your direct messages do not need one and are already working."
                } else {
                    "Each server is a machine somebody runs. Joining one swaps this session for
                     that one — your direct messages come with you, they belong to your key."
                }
            }

            // Offered before the directory, because coming back to where you
            // were is the commonest reason to be looking at this pane at all,
            // and it was the one thing the old connect screen did that home
            // could not.
            if offline {
                if let Some(saved) = last_session.clone() {
                    {
                        let label = saved.mode.label();
                        rsx! {
                            button {
                                class: "w-full flex items-center gap-3 px-3 py-2.5 rounded-lg border border-[var(--border-strong)] bg-[var(--panel)] text-left hover:border-[var(--accent)] transition-colors",
                                onclick: move |_| on_switch.call(saved.mode.clone()),
                                span { class: "flex-1 min-w-0",
                                    span { class: "block text-[10px] font-mono uppercase tracking-wider text-[var(--text-dim)]",
                                        "last session"
                                    }
                                    span { class: "block truncate text-sm text-[var(--text)]", "{label}" }
                                }
                                span { class: "shrink-0 text-xs text-[var(--accent)]", "Reconnect" }
                            }
                        }
                    }
                }
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
                    "{go_label}"
                }
            }

            // Both folded: the directory and a pasted code answer this pane's
            // question almost every time, and an open form beside them reads as
            // something you are expected to fill in.
            details {
                summary { class: "cursor-pointer text-[10px] uppercase tracking-wider text-[var(--text-muted)] hover:text-[var(--text)] transition-colors",
                    "Connect by address"
                }
                div { class: "mt-2",
                    crate::features::connect::AddressForm { on_go: move |mode| on_switch.call(mode) }
                }
            }

            details {
                open: open_host,
                summary { class: "cursor-pointer text-[10px] uppercase tracking-wider text-[var(--text-muted)] hover:text-[var(--text)] transition-colors",
                    "Host your own"
                }
                div { class: "mt-2",
                    crate::features::connect::HostForm { on_go: move |mode| on_switch.call(mode) }
                }
            }

            details {
                summary { class: "cursor-pointer text-[10px] uppercase tracking-wider text-[var(--text-muted)] hover:text-[var(--text)] transition-colors",
                    "Directory settings"
                }
                div { class: "mt-2",
                    crate::features::connect::RendezvousPicker {
                        selected: rendezvous.clone(),
                        on_select: move |url: String| {
                            let mut next = settings.read().clone();
                            next.use_rendezvous(&url);
                            settings.set(next.clone());
                            crate::settings::save(&next);
                        },
                    }
                }
            }
        }
    }
}

/// What fills the main area when home has nothing open yet.
///
/// The alternative was an empty chat pane, which on a first launch says
/// nothing at all — and says it at the exact moment somebody is deciding
/// whether this app works without a server. It does, and this is where that
/// gets stated.
#[component]
pub fn HomeWelcome() -> Element {
    let mut state = use_app_state();
    let snapshot = state.read();
    let offline = snapshot.status == ConnectionStatus::Offline;
    let dms = snapshot.dms.len();
    let relays = snapshot.nostr_relays_up.len();
    drop(snapshot);

    rsx! {
        div { class: "flex-1 flex items-center justify-center p-8",
            div { class: "max-w-md space-y-4 text-center",
                div { class: "text-lg font-medium text-[var(--text)]",
                    if dms == 0 { "Nothing open yet" } else { "Pick up a conversation" }
                }
                div { class: "text-sm text-[var(--text-muted)] leading-relaxed",
                    if dms == 0 {
                        "Direct messages are Nostr events signed by your key, so they work with no
                         server at all — paste someone's npub in the column to start one."
                    } else {
                        "Your conversations are in the column. They belong to your key, not to any
                         server, so they follow you wherever you connect."
                    }
                }
                div { class: "flex items-center justify-center gap-2 text-[11px] font-mono text-[var(--text-dim)]",
                    span {
                        class: "w-1.5 h-1.5 rounded-full",
                        style: if relays > 0 { "background: var(--up);" } else { "background: var(--warn);" },
                    }
                    if relays == 1 { "1 relay connected" } else { "{relays} relays connected" }
                }
                if offline {
                    div { class: "pt-2 border-t border-[var(--border)] space-y-3",
                        div { class: "text-sm text-[var(--text-muted)] leading-relaxed",
                            "You're not on a server. One gives you communities, channels and voice
                             — everything that is shared rather than yours."
                        }
                        div { class: "flex items-center justify-center gap-2",
                            button {
                                class: "dxf-cta px-4 py-2 rounded text-sm",
                                onclick: move |_| {
                                    let mut s = state.write();
                                    s.home_view = HomeView::Servers;
                                    s.home_open_host = false;
                                },
                                "Find a server"
                            }
                            button {
                                class: "px-4 py-2 rounded text-sm border border-[var(--border)] text-[var(--text-muted)] hover:text-[var(--accent)] hover:border-[var(--accent)] transition-colors",
                                onclick: move |_| {
                                    let mut s = state.write();
                                    s.home_view = HomeView::Servers;
                                    s.home_open_host = true;
                                },
                                "Host my own"
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Who you are, at the foot of home's column.
///
/// It was the first thing on the connect screen, which is where somebody
/// looked for it exactly once. Your key is the account here, so it belongs
/// where you can see it every day rather than on a screen you passed through.
#[component]
pub fn IdentityFooter() -> Element {
    let identity = use_context::<crate::identity::Identity>();
    let actions = use_context::<IdentityActions>();
    let mut open = use_signal(|| false);

    rsx! {
        div { class: "shrink-0 border-t border-[var(--border)] px-2 py-2",
            if open() {
                div { class: "mb-2",
                    crate::features::connect::IdentityCard {
                        identity: identity.clone(),
                        on_rename: actions.on_rename,
                        on_sign_out: actions.on_sign_out,
                    }
                }
            }
            button {
                class: "w-full flex items-center gap-2 px-1 py-1 rounded text-left hover:bg-white/[0.03] transition-colors",
                onclick: move |_| open.set(!open()),
                crate::features::profiles::Avatar {
                    pubkey: identity.pubkey.clone(),
                    name: identity.display_name.clone(),
                    size: "w-7 h-7",
                    text: "text-[10px]",
                }
                span { class: "flex-1 min-w-0",
                    span { class: "block truncate text-sm text-[var(--text)]", "{identity.display_name}" }
                    span { class: "block truncate font-mono text-[9px] text-[var(--text-dim)]",
                        "{identity.npub()}"
                    }
                }
                span { class: "shrink-0 text-[10px] text-[var(--text-dim)]",
                    if open() { "\u{25b4}" } else { "\u{25be}" }
                }
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
    let offline = snapshot.status == ConnectionStatus::Offline;
    drop(snapshot);

    let communities_first =
        primary_explore(offline, joined, joinable.len()) == HomeView::Communities;
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
                    if offline {
                        "Communities live inside a server. You're not on one yet."
                    } else {
                        "You haven't joined a community on this server yet."
                    }
                }
            }

            div { class: "flex-1" }
            // Offline this would ask a server that was never dialled, so the
            // button is not offered rather than being offered and failing.
            if !offline {
                button {
                    class: "shrink-0 text-[10px] uppercase tracking-wider text-[var(--text-muted)] border border-[var(--border)] rounded px-2.5 py-1 hover:text-[var(--accent)] hover:border-[var(--accent)] transition-colors",
                    onclick: move |_| {
                        gw_fetch.send(ClientMessage::FetchCatalog { offset: 0, limit: 0 });
                        state.write().home_view = HomeView::Communities;
                    },
                    "Communities"
                }
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

    /// With no gateway there is no catalog to read, whatever a stale count
    /// might still say — this is the first-launch case, and the reason home
    /// no longer sits behind the connect screen.
    #[test]
    fn offline_can_only_lead_to_a_server() {
        assert_eq!(primary_explore(true, 0, 0), HomeView::Servers);
        assert_eq!(primary_explore(true, 4, 9), HomeView::Servers);
    }

    /// Arrived somewhere with nothing to join: the only useful next step is
    /// somebody else's server.
    #[test]
    fn a_bare_server_leads_with_the_directory() {
        assert_eq!(primary_explore(false, 0, 0), HomeView::Servers);
    }

    /// A host with a catalog is worth browsing before leaving it.
    #[test]
    fn something_to_join_here_leads_with_communities() {
        assert_eq!(primary_explore(false, 0, 3), HomeView::Communities);
    }

    /// Having joined is what "you're in" means, and it holds even once there
    /// is nothing left to join — otherwise settling in would flip home back to
    /// the directory, which reads as being asked to leave.
    #[test]
    fn joining_everything_does_not_send_you_away() {
        assert_eq!(primary_explore(false, 4, 0), HomeView::Communities);
    }
}
