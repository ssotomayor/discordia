use dioxus::prelude::*;
use dioxus_grid_layout::NoDrag;

use crate::protocol::{ClientMessage, GuildSummary, Id};
use crate::state::{use_app_state, use_gateway};

const HEADER: &str = "h-11 px-2 flex items-center justify-center border-b border-[var(--border)] shrink-0";

/// Right-click menu anchored over a guild the current user owns.
#[derive(Clone, PartialEq)]
struct GuildMenu {
    guild_id: Id,
    name: String,
    x: f64,
    y: f64,
    /// Once the user picks "Delete", we show an inline confirm step.
    confirming: bool,
}

#[component]
pub fn GuildsSidebar() -> Element {
    let mut state = use_app_state();
    let gateway = use_gateway();

    let snapshot = state.read();
    let guilds = snapshot.guilds.clone();
    let selected = snapshot.selected_guild;
    let dm_mode = snapshot.dm_mode;
    let dm_unread = snapshot.dm_unread_total() as usize;
    let self_pubkey = snapshot.self_user.as_ref().map(|u| u.pubkey.clone());
    // Guilds in the directory we haven't joined yet.
    let available: Vec<GuildSummary> = snapshot
        .catalog
        .iter()
        .filter(|c| !snapshot.guilds.iter().any(|g| g.id == c.id))
        .cloned()
        .collect();
    drop(snapshot);

    let mut menu = use_signal::<Option<GuildMenu>>(|| None);
    let mut show_browse = use_signal(|| false);

    rsx! {
        nav { class: "panel-hover w-full h-full bg-[var(--panel)] border border-[var(--border)] rounded-lg flex flex-col overflow-hidden",
            div { class: HEADER,
                span { class: "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)]",
                    "Guilds"
                }
            }

            NoDrag {
            div { class: "flex-1 overflow-y-auto flex flex-col items-center py-3 gap-2",
                // DM "home" button, pinned above the servers.
                DmHomeButton {
                    active: dm_mode,
                    count: dm_unread,
                    onclick: move |_| {
                        let mut s = state.write();
                        s.dm_mode = true;
                        // Land on the first conversation if we aren't already
                        // viewing one, and clear its unread badge.
                        let on_dm = s
                            .selected_channel
                            .map(|cid| s.dm_of(cid).is_some())
                            .unwrap_or(false);
                        if !on_dm {
                            let first = s.dms.first().map(|d| d.channel_id);
                            s.selected_channel = first;
                            if let Some(cid) = first {
                                s.dm_unread.remove(&cid);
                            }
                        }
                    },
                }

                // Divider between DMs and servers.
                div { class: "w-6 h-px bg-[var(--border)] my-1" }

                for guild in guilds.iter().cloned() {
                    {
                        let owned = self_pubkey
                            .as_deref()
                            .map(|pk| !guild.owner_pubkey.is_empty() && guild.owner_pubkey == pk)
                            .unwrap_or(false);
                        let gname = guild.name.clone();
                        rsx! {
                            GuildIcon {
                                key: "{guild.id}",
                                id: guild.id,
                                label: guild.icon.clone().unwrap_or_else(|| initials(&guild.name)),
                                name: guild.name.clone(),
                                selected: !dm_mode && selected == Some(guild.id),
                                owned,
                                on_select: move |gid: Id| {
                                    let mut s = state.write();
                                    s.dm_mode = false;
                                    s.selected_guild = Some(gid);
                                    s.selected_channel = s
                                        .channels
                                        .iter()
                                        .find(|c| c.guild_id == gid)
                                        .map(|c| c.id);
                                },
                                on_context: move |(gid, x, y): (Id, f64, f64)| {
                                    menu.set(Some(GuildMenu {
                                        guild_id: gid,
                                        name: gname.clone(),
                                        x,
                                        y,
                                        confirming: false,
                                    }));
                                },
                            }
                        }
                    }
                }

                CreateGuild {}

                // Browse & join other guilds on this host.
                button {
                    class: "relative w-10 h-10 rounded-md border border-dashed border-[var(--border)] text-[var(--text-muted)] hover:text-[var(--accent)] hover:border-[var(--accent)] flex items-center justify-center text-base leading-none transition-colors",
                    title: "Browse guilds to join",
                    onclick: move |_| show_browse.set(true),
                    "🔍"
                    if !available.is_empty() {
                        span { class: "dxf-pop absolute -top-1 -right-1 min-w-4 h-4 px-1 rounded-full bg-[var(--accent)] text-[var(--bg)] text-[9px] font-bold flex items-center justify-center",
                            "{available.len()}"
                        }
                    }
                }
            }
            }

            // Context menu overlay. A transparent backdrop closes it on any
            // outside click; the menu itself floats at the cursor.
            if let Some(m) = menu() {
                div {
                    class: "fixed inset-0 z-50",
                    onclick: move |_| menu.set(None),
                    oncontextmenu: move |e| { e.prevent_default(); menu.set(None); },
                    div {
                        class: "dxf-pop-in absolute min-w-44 bg-[var(--panel-solid)] border border-[var(--border)] rounded-md shadow-lg p-1 text-sm",
                        style: "left: {m.x}px; top: {m.y}px;",
                        onclick: move |e| e.stop_propagation(),
                        if !m.confirming {
                            // Accent control (owner restyles the guild live).
                            {
                                let cur_accent = state
                                    .read()
                                    .guilds
                                    .iter()
                                    .find(|g| g.id == m.guild_id)
                                    .and_then(|g| g.accent.clone());
                                let gid = m.guild_id;
                                let gw_set = gateway.clone();
                                let gw_clear = gateway.clone();
                                rsx! {
                                    div { class: "flex items-center gap-2 px-3 py-1.5",
                                        span { class: "text-xs text-[var(--text-muted)] flex-1", "Guild accent" }
                                        input {
                                            r#type: "color",
                                            class: "w-7 h-7 rounded border border-[var(--border)] bg-transparent cursor-pointer",
                                            value: "{cur_accent.clone().unwrap_or_else(|| \"#e0a06a\".into())}",
                                            oninput: move |e| {
                                                gw_set.send(ClientMessage::SetGuildAccent { guild_id: gid, accent: Some(e.value()) });
                                            },
                                        }
                                        if cur_accent.is_some() {
                                            button {
                                                class: "text-[10px] uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--text-muted)]",
                                                onclick: move |_| gw_clear.send(ClientMessage::SetGuildAccent { guild_id: gid, accent: None }),
                                                "clear"
                                            }
                                        }
                                    }
                                }
                            }
                            button {
                                class: "w-full text-left px-3 py-1.5 rounded text-[var(--danger)] hover:bg-[var(--danger)]/10 transition-colors",
                                onclick: move |_| {
                                    if let Some(cur) = menu.write().as_mut() {
                                        cur.confirming = true;
                                    }
                                },
                                "Delete server"
                            }
                        } else {
                            div { class: "px-3 py-1.5 text-xs text-[var(--text-muted)]",
                                "Delete \"{m.name}\"? This can't be undone."
                            }
                            div { class: "flex gap-1 px-1 pb-0.5",
                                button {
                                    class: "flex-1 px-2 py-1 rounded text-xs uppercase tracking-wider text-[var(--danger)] border border-[var(--danger)]/40 hover:bg-[var(--danger)]/10 transition-colors",
                                    onclick: {
                                        let gw = gateway.clone();
                                        move |_| {
                                            gw.send(ClientMessage::DeleteGuild { guild_id: m.guild_id });
                                            menu.set(None);
                                        }
                                    },
                                    "Delete"
                                }
                                button {
                                    class: "flex-1 px-2 py-1 rounded text-xs uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--text-muted)] transition-colors",
                                    onclick: move |_| menu.set(None),
                                    "Cancel"
                                }
                            }
                        }
                    }
                }
            }

            // Browse-and-join modal.
            if show_browse() {
                div {
                    class: "dxf-backdrop-in fixed inset-0 z-50 flex items-center justify-center bg-black/50",
                    onclick: move |_| show_browse.set(false),
                    div {
                        class: "dxf-modal-in w-80 max-h-[70vh] flex flex-col bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg shadow-xl overflow-hidden",
                        onclick: move |e| e.stop_propagation(),
                        div { class: "px-4 py-3 border-b border-[var(--border)] flex items-center",
                            h3 { class: "text-sm font-medium text-[var(--accent)] flex-1", "Browse guilds" }
                            button {
                                class: "text-[var(--text-dim)] hover:text-[var(--text)] text-lg leading-none",
                                onclick: move |_| show_browse.set(false),
                                "✕"
                            }
                        }
                        div { class: "flex-1 overflow-y-auto p-2 space-y-1",
                            if available.is_empty() {
                                div { class: "px-2 py-6 text-center text-xs text-[var(--text-dim)]",
                                    "You've joined every guild here. Create one with +."
                                }
                            }
                            for g in available.iter().cloned() {
                                {
                                    let gid = g.id;
                                    let gw = gateway.clone();
                                    let label = g.icon.clone().unwrap_or_else(|| initials(&g.name));
                                    rsx! {
                                        div {
                                            key: "{gid}",
                                            class: "flex items-center gap-2 px-2 py-1.5 rounded hover:bg-white/[0.03]",
                                            span { class: "w-8 h-8 rounded-md border border-[var(--border)] flex items-center justify-center text-xs text-[var(--text-muted)] shrink-0",
                                                "{label}"
                                            }
                                            div { class: "flex-1 min-w-0",
                                                div { class: "text-sm text-[var(--text)] truncate", "{g.name}" }
                                                div { class: "text-[10px] text-[var(--text-dim)]",
                                                    if g.member_count == 1 { "1 member" } else { "{g.member_count} members" }
                                                }
                                            }
                                            button {
                                                class: "px-3 py-1 rounded text-[10px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors",
                                                onclick: move |_| gw.send(ClientMessage::JoinGuild { guild_id: gid }),
                                                "Join"
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

#[component]
fn DmHomeButton(active: bool, count: usize, onclick: EventHandler<()>) -> Element {
    let cls = if active {
        "border-[var(--accent)] text-[var(--accent)] bg-[var(--accent-soft)]"
    } else {
        "border-[var(--border)] text-[var(--text-muted)] hover:text-[var(--text)] hover:border-[var(--border-strong)]"
    };
    rsx! {
        button {
            class: "relative w-10 h-10 rounded-md border flex items-center justify-center text-xs font-semibold tracking-wide transition-colors {cls}",
            title: "Direct messages",
            onclick: move |_| onclick.call(()),
            "DM"
            if count > 0 {
                span { class: "dxf-pop absolute -top-1 -right-1 min-w-4 h-4 px-1 rounded-full bg-[var(--accent)] text-[var(--bg)] text-[9px] font-bold flex items-center justify-center",
                    "{count}"
                }
            }
        }
    }
}

/// "+" button that expands into a tiny name field. Submitting sends a
/// `CreateGuild` to the server; the new guild arrives back over the socket
/// (see `ServerMessage::GuildJoined`, delivered only to the creator) and is
/// selected automatically.
#[component]
fn CreateGuild() -> Element {
    let gateway = use_gateway();
    let mut open = use_signal(|| false);
    let mut name = use_signal(String::new);

    let mut submit = move || {
        let trimmed = name().trim().to_string();
        if !trimmed.is_empty() {
            gateway.send(ClientMessage::CreateGuild { name: trimmed });
        }
        name.set(String::new());
        open.set(false);
    };

    if !open() {
        return rsx! {
            button {
                class: "w-10 h-10 rounded-md border border-dashed border-[var(--border)] text-[var(--text-muted)] hover:text-[var(--accent)] hover:border-[var(--accent)] flex items-center justify-center text-lg leading-none transition-colors",
                title: "Create a server",
                onclick: move |_| open.set(true),
                "+"
            }
        };
    }

    rsx! {
        form {
            class: "w-full px-1.5 flex flex-col gap-1",
            onsubmit: move |_| submit(),
            input {
                class: "w-full bg-transparent border border-[var(--border)] focus:border-[var(--accent)] rounded px-1.5 py-1 text-[11px] text-[var(--text)] outline-none transition-colors",
                placeholder: "Name…",
                value: "{name}",
                autofocus: true,
                maxlength: 64,
                oninput: move |e| name.set(e.value()),
                onkeydown: move |e| {
                    if e.key() == Key::Escape {
                        name.set(String::new());
                        open.set(false);
                    }
                },
            }
            div { class: "flex gap-1",
                button {
                    r#type: "submit",
                    class: "flex-1 rounded px-1 py-0.5 text-[10px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors",
                    "Add"
                }
                button {
                    r#type: "button",
                    class: "rounded px-1 py-0.5 text-[10px] uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--text-muted)] transition-colors",
                    onclick: move |_| {
                        name.set(String::new());
                        open.set(false);
                    },
                    "✕"
                }
            }
        }
    }
}

#[component]
fn GuildIcon(
    id: Id,
    label: String,
    name: String,
    selected: bool,
    /// Whether the current user owns this guild (and so may delete it).
    owned: bool,
    on_select: EventHandler<Id>,
    /// Fired on right-click for owned guilds, with (guild_id, x, y).
    on_context: EventHandler<(Id, f64, f64)>,
) -> Element {
    let cls = if selected {
        "border-[var(--accent)] text-[var(--accent)]"
    } else {
        "border-[var(--border)] text-[var(--text-muted)] hover:text-[var(--text)] hover:border-[var(--border-strong)]"
    };

    rsx! {
        button {
            class: "w-10 h-10 rounded-md border flex items-center justify-center text-xs font-medium transition-colors {cls}",
            title: if owned { "{name} (right-click for options)" } else { "{name}" },
            onclick: move |_| on_select.call(id),
            oncontextmenu: move |e: MouseEvent| {
                // Only owners get a menu; otherwise let the default through.
                if owned {
                    e.prevent_default();
                    let c = e.client_coordinates();
                    on_context.call((id, c.x, c.y));
                }
            },
            "{label}"
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
