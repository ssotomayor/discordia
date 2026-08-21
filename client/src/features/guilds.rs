use dioxus::prelude::*;
use dioxus_grid_layout::NoDrag;

use crate::protocol::{ClientMessage, GuildSummary, Id, Permission};
use crate::state::{use_app_state, use_gateway};

const HEADER: &str =
    "h-11 px-2 flex items-center justify-center border-b border-[var(--border)] shrink-0";

/// Right-click menu anchored over a guild the current user can manage (or at
/// least leave).
#[derive(Clone, PartialEq)]
struct GuildMenu {
    guild_id: Id,
    name: String,
    x: f64,
    y: f64,
    /// Inline confirm step for a destructive action.
    confirming: Option<ConfirmAction>,
}

#[derive(Clone, Copy, PartialEq)]
enum ConfirmAction {
    Delete,
    Leave,
    Transfer,
}

#[component]
pub fn GuildsSidebar() -> Element {
    let mut state = use_app_state();
    let gateway = use_gateway();
    // Present only under `HomeView`; absent in the workspace. See `HomeChrome`.
    let home_chrome = try_consume_context::<crate::features::home::HomeChrome>();

    let snapshot = state.read();
    // Owned guilds first: they are the ones you administer, and grouping them
    // makes the cog affordance read as a property of the group.
    let guilds = {
        let mut v = snapshot.guilds.clone();
        v.sort_by_key(|g| !snapshot.is_owner(g.id));
        v
    };
    // `is_owner` covers system guilds (empty owner) for operators, matching
    // the rest of the app.
    let owned_count = guilds.iter().filter(|g| snapshot.is_owner(g.id)).count();
    let selected = snapshot.selected_guild;
    let dm_mode = snapshot.dm_mode;
    let dm_unread = snapshot.dm_unread_total() as usize;
    let is_operator = snapshot.is_operator;
    let available: Vec<GuildSummary> = snapshot
        .catalog
        .iter()
        .filter(|c| !snapshot.guilds.iter().any(|g| g.id == c.id))
        .cloned()
        .collect();
    let catalog_len = snapshot.catalog.len();
    let catalog_total = snapshot.catalog_total as usize;
    drop(snapshot);

    let mut menu = use_signal::<Option<GuildMenu>>(|| None);
    let mut show_browse = use_signal(|| false);

    rsx! {
        nav { class: "panel-hover w-full h-full bg-[var(--panel)] border border-[var(--border)] rounded-lg flex flex-col overflow-hidden",
            div { class: HEADER,
                span { class: "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)]",
                    // Not "Guilds" at home, where there are none and cannot be:
                    // a guild is a community *inside* a server, so the header
                    // named a thing the panel did not contain while the only
                    // entry in it offered a different noun entirely.
                    if home_chrome.is_some() { "Home" } else { "Guilds" }
                }
            }

            NoDrag {
            div { class: "flex-1 overflow-y-auto flex flex-col items-center py-3 gap-2",
                DmHomeButton {
                    active: dm_mode,
                    count: dm_unread,
                    onclick: move |_| {
                        let mut s = state.write();
                        s.dm_mode = true;
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

                div { class: "w-6 h-px bg-[var(--border)] my-1" }

                for (idx, guild) in guilds.iter().cloned().enumerate() {
                    {
                        // System guilds (empty owner) are only manageable by
                        // operators.
                        let has_menu = !guild.owner_pubkey.is_empty() || is_operator;
                        // Visible cog for owners; right-click is the only way
                        // to reach "Leave" on non-owned guilds.
                        let is_mine = idx < owned_count;
                        let gname = guild.name.clone();
                        rsx! {
                            GuildIcon {
                                key: "{guild.id}",
                                id: guild.id,
                                label: guild.icon.clone().unwrap_or_else(|| initials(&guild.name)),
                                image: guild.icon_image.clone(),
                                name: guild.name.clone(),
                                selected: !dm_mode && selected == Some(guild.id),
                                has_menu,
                                is_mine,
                                on_select: {
                                    let gateway = gateway.clone();
                                    move |gid: Id| {
                                        // Always land on the default text
                                        // channel; never leave the previous
                                        // guild's channel or land on a voice
                                        // channel.
                                        let target = {
                                            let mut s = state.write();
                                            s.dm_mode = false;
                                            s.selected_guild = Some(gid);
                                            s.selected_channel = None;
                                            s.default_channel_of(gid)
                                        };
                                        // Route through the shared selector so
                                        // the channel's history is fetched if we
                                        // haven't loaded it yet; setting
                                        // selected_channel directly left the
                                        // view empty on first visit.
                                        if let Some(cid) = target {
                                            crate::features::channels::select_text_channel(
                                                &mut state.clone(),
                                                &gateway,
                                                cid,
                                            );
                                        }
                                    }
                                },
                                on_menu: move |(gid, x, y): (Id, f64, f64)| {
                                    menu.set(Some(GuildMenu {
                                        guild_id: gid,
                                        name: gname.clone(),
                                        x,
                                        y,
                                        confirming: None,
                                    }));
                                },
                            }
                        }
                    }
                }

                // Creating and browsing guilds are things a connected server
                // does. At home there is none, so the rail offers the one
                // action that changes that instead.
                if let Some(home) = home_chrome {
                    ConnectEntry { show_connect: home.show_connect }
                }

                if home_chrome.is_none() {
                    CreateGuild {}

                    button {
                        class: "relative w-10 h-10 rounded-md border border-dashed border-[var(--border)] text-[var(--text-muted)] hover:text-[var(--accent)] hover:border-[var(--accent)] flex items-center justify-center text-base leading-none transition-colors",
                        title: "Browse guilds to join",
                        onclick: {
                            let gateway = gateway.clone();
                            move |_| {
                                // Pull the latest directory on open — the server no
                                // longer pushes catalog updates to everyone.
                                gateway.send(ClientMessage::FetchCatalog { offset: 0, limit: 0 });
                                show_browse.set(true);
                            }
                        },
                        "🔍"
                        if !available.is_empty() {
                            span { class: "dxf-pop absolute -top-1 -right-1 min-w-4 h-4 px-1 rounded-full bg-[var(--accent)] text-[var(--bg)] text-[9px] font-bold flex items-center justify-center",
                                "{available.len()}"
                            }
                        }
                    }
                }
            }
            }

            if let Some(m) = menu() {
                div {
                    class: "fixed inset-0 z-50",
                    onclick: move |_| menu.set(None),
                    oncontextmenu: move |e| { e.prevent_default(); menu.set(None); },
                    div {
                        class: "dxf-pop-in absolute min-w-44 bg-[var(--panel-solid)] border border-[var(--border)] rounded-md shadow-lg p-1 text-sm",
                        style: "left: {m.x}px; top: {m.y}px;",
                        onclick: move |e| e.stop_propagation(),
                        {
                            // Per-entry permission gates (server re-checks all
                            // of these — the menu just hides dead ends).
                            let s = state.read();
                            let gid = m.guild_id;
                            let is_owner = s.is_owner(gid);
                            let can_manage = s.can(gid, Permission::ManageGuild);
                            let can_roles = s.can(gid, Permission::ManageRoles);
                            // System guilds (the Lobby) can't be deleted, left,
                            // or transferred — even by an operator — so those
                            // entries are hidden there.
                            let is_system = s
                                .guilds
                                .iter()
                                .find(|g| g.id == gid)
                                .map(|g| g.owner_pubkey.is_empty())
                                .unwrap_or(false);
                            let cur_accent = s
                                .guilds
                                .iter()
                                .find(|g| g.id == gid)
                                .and_then(|g| g.accent.clone());
                            drop(s);
                            let gw_set = gateway.clone();
                            let gw_clear = gateway.clone();
                            rsx! {
                                match m.confirming {
                                    None => rsx! {
                                        if can_manage {
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
                                            button {
                                                class: "w-full text-left px-3 py-1.5 rounded text-[var(--text)] hover:bg-white/[0.04] transition-colors",
                                                onclick: move |_| {
                                                    state.write().guild_dialog =
                                                        Some(crate::state::GuildDialog::Settings(gid));
                                                    menu.set(None);
                                                },
                                                "Guild settings"
                                            }
                                            button {
                                                class: "w-full text-left px-3 py-1.5 rounded text-[var(--text)] hover:bg-white/[0.04] transition-colors",
                                                onclick: move |_| {
                                                    state.write().guild_dialog =
                                                        Some(crate::state::GuildDialog::Integrations(gid));
                                                    menu.set(None);
                                                },
                                                "Integrations"
                                            }
                                        }
                                        if can_roles {
                                            button {
                                                class: "w-full text-left px-3 py-1.5 rounded text-[var(--text)] hover:bg-white/[0.04] transition-colors",
                                                onclick: move |_| {
                                                    state.write().guild_dialog =
                                                        Some(crate::state::GuildDialog::Roles(gid));
                                                    menu.set(None);
                                                },
                                                "Roles"
                                            }
                                        }
                                        if !is_system {
                                            if is_owner {
                                                button {
                                                    class: "w-full text-left px-3 py-1.5 rounded text-[var(--warn)] hover:bg-[var(--warn)]/10 transition-colors",
                                                    onclick: move |_| {
                                                        if let Some(cur) = menu.write().as_mut() {
                                                            cur.confirming = Some(ConfirmAction::Transfer);
                                                        }
                                                    },
                                                    "Transfer ownership"
                                                }
                                                button {
                                                    class: "w-full text-left px-3 py-1.5 rounded text-[var(--danger)] hover:bg-[var(--danger)]/10 transition-colors",
                                                    onclick: move |_| {
                                                        if let Some(cur) = menu.write().as_mut() {
                                                            cur.confirming = Some(ConfirmAction::Delete);
                                                        }
                                                    },
                                                    "Delete guild"
                                                }
                                            } else {
                                                button {
                                                    class: "w-full text-left px-3 py-1.5 rounded text-[var(--danger)] hover:bg-[var(--danger)]/10 transition-colors",
                                                    onclick: move |_| {
                                                        if let Some(cur) = menu.write().as_mut() {
                                                            cur.confirming = Some(ConfirmAction::Leave);
                                                        }
                                                    },
                                                    "Leave guild"
                                                }
                                            }
                                        }
                                    },
                                    Some(ConfirmAction::Delete) => rsx! {
                                        div { class: "px-3 py-1.5 text-xs text-[var(--text-muted)]",
                                            "Delete \"{m.name}\"? This can't be undone."
                                        }
                                        div { class: "flex gap-1 px-1 pb-0.5",
                                            button {
                                                class: "flex-1 px-2 py-1 rounded text-xs uppercase tracking-wider text-[var(--danger)] border border-[var(--danger)]/40 hover:bg-[var(--danger)]/10 transition-colors",
                                                onclick: {
                                                    let gw = gateway.clone();
                                                    move |_| {
                                                        gw.send(ClientMessage::DeleteGuild { guild_id: gid });
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
                                    },
                                    Some(ConfirmAction::Leave) => rsx! {
                                        div { class: "px-3 py-1.5 text-xs text-[var(--text-muted)]",
                                            "Leave \"{m.name}\"? You can rejoin later if it's public (or with an invite)."
                                        }
                                        div { class: "flex gap-1 px-1 pb-0.5",
                                            button {
                                                class: "flex-1 px-2 py-1 rounded text-xs uppercase tracking-wider text-[var(--danger)] border border-[var(--danger)]/40 hover:bg-[var(--danger)]/10 transition-colors",
                                                onclick: {
                                                    let gw = gateway.clone();
                                                    move |_| {
                                                        gw.send(ClientMessage::LeaveGuild { guild_id: gid });
                                                        menu.set(None);
                                                    }
                                                },
                                                "Leave"
                                            }
                                            button {
                                                class: "flex-1 px-2 py-1 rounded text-xs uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--text-muted)] transition-colors",
                                                onclick: move |_| menu.set(None),
                                                "Cancel"
                                            }
                                        }
                                    },
                                    Some(ConfirmAction::Transfer) => rsx! {
                                        TransferPicker {
                                            guild_id: gid,
                                            on_done: move |_| menu.set(None),
                                        }
                                    },
                                }
                            }
                        }
                    }
                }
            }

            // The management dialogs are rendered at the workspace root (see
            // `GuildDialogHost`), not here. A modal inside this panel would be
            // inside this panel's stacking context and could be covered by any
            // panel stacked above it.

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
                                                onclick: move |_| gw.send(ClientMessage::JoinGuild { guild_id: gid, accept: false, pow_nonce: None }),
                                                "Join"
                                            }
                                        }
                                    }
                                }
                            }
                            if catalog_len < catalog_total {
                                {
                                    let gw = gateway.clone();
                                    rsx! {
                                        div { class: "flex justify-center py-2",
                                            button {
                                                class: "text-[11px] uppercase tracking-wider text-[var(--text-muted)] border border-[var(--border)] rounded px-3 py-1 hover:text-[var(--accent)] hover:border-[var(--accent)] transition-colors",
                                                onclick: move |_| {
                                                    gw.send(ClientMessage::FetchCatalog {
                                                        offset: catalog_len as u32,
                                                        limit: 0,
                                                    });
                                                },
                                                "Load more"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        InviteJoinRow { on_joined: move |_| show_browse.set(false) }
                    }
                }
            }
        }
    }
}

/// "Have an invite code?" input at the bottom of the browse modal. The server
/// replies `GuildJoined` (which auto-selects the guild) or an `Error` toast.
#[component]
fn InviteJoinRow(on_joined: EventHandler<()>) -> Element {
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
        on_joined.call(());
    };

    rsx! {
        form {
            class: "border-t border-[var(--border)] p-2 flex items-center gap-2",
            onsubmit: move |_| submit(),
            input {
                class: "flex-1 bg-transparent border border-[var(--border)] focus:border-[var(--accent)] rounded px-2 py-1 text-xs font-mono text-[var(--text)] outline-none transition-colors",
                placeholder: "Have an invite code?",
                value: "{code}",
                oninput: move |e| code.set(e.value()),
            }
            button {
                r#type: "submit",
                class: "px-3 py-1 rounded text-[10px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors",
                "Join"
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
/// The rail's way out of the home surface, in the slot the workspace uses for
/// creating and browsing guilds.
#[component]
fn ConnectEntry(mut show_connect: Signal<bool>) -> Element {
    rsx! {
        button {
            r#type: "button",
            class: "w-10 h-10 rounded-md border border-dashed border-[var(--border)] text-[var(--text-muted)] hover:text-[var(--accent)] hover:border-[var(--accent)] flex items-center justify-center text-base leading-none transition-colors",
            title: "Connect to a server",
            onclick: move |_| show_connect.set(true),
            "+"
        }
    }
}

#[component]
fn CreateGuild() -> Element {
    let gateway = use_gateway();
    let mut open = use_signal(|| false);
    let mut name = use_signal(String::new);

    let mut submit = move || {
        let trimmed = name().trim().to_string();
        if !trimmed.is_empty() {
            gateway.send(ClientMessage::CreateGuild {
                name: trimmed,
                template: None,
            });
        }
        name.set(String::new());
        open.set(false);
    };

    if !open() {
        return rsx! {
            button {
                class: "w-10 h-10 rounded-md border border-dashed border-[var(--border)] text-[var(--text-muted)] hover:text-[var(--accent)] hover:border-[var(--accent)] flex items-center justify-center text-lg leading-none transition-colors",
                title: "Create a guild",
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
    /// Uploaded icon (http(s) or data URL). Falls back to `label` when absent —
    /// which, until now, was the only thing ever drawn: an uploaded guild icon
    /// was stored and round-tripped but never rendered anywhere.
    image: Option<String>,
    name: String,
    selected: bool,
    /// Whether right-clicking opens the management/leave menu (false only for
    /// system guilds, which can be neither managed nor left).
    has_menu: bool,
    /// Whether the viewer owns this guild. Owners get a visible cog, because
    /// "right-click the icon" is not a thing anyone discovers on their own.
    is_mine: bool,
    on_select: EventHandler<Id>,
    /// Fired with (guild_id, x, y) when the menu should open — right-click on
    /// anything with a menu, or the cog on a guild you own.
    on_menu: EventHandler<(Id, f64, f64)>,
) -> Element {
    let cls = if selected {
        "border-[var(--accent)] text-[var(--accent)]"
    } else {
        "border-[var(--border)] text-[var(--text-muted)] hover:text-[var(--text)] hover:border-[var(--border-strong)]"
    };

    rsx! {
        // relative so the cog overlays the tile corner; group so it brightens
        // with the tile.
        div { class: "relative group",
            button {
                class: "w-10 h-10 rounded-md border flex items-center justify-center text-xs font-medium transition-colors overflow-hidden {cls}",
                title: if is_mine {
                    "{name} — yours, click the cog for settings"
                } else if has_menu {
                    "{name} (right-click for options)"
                } else {
                    "{name}"
                },
                onclick: move |_| on_select.call(id),
                oncontextmenu: move |e: MouseEvent| {
                    if has_menu {
                        e.prevent_default();
                        let c = e.client_coordinates();
                        on_menu.call((id, c.x, c.y));
                    }
                },
                if let Some(src) = image {
                    img { class: "w-full h-full object-cover", src: "{src}", alt: "{name}" }
                } else {
                    "{label}"
                }
            }
            if is_mine {
                button {
                    class: "absolute -right-1 -bottom-1 w-4 h-4 flex items-center justify-center rounded-full border border-[var(--border)] bg-[var(--panel-solid)] text-[var(--text-muted)] opacity-70 group-hover:opacity-100 hover:text-[var(--accent)] hover:border-[var(--accent)] transition-all",
                    title: "Guild settings",
                    // Open at the cog, not under the cursor, so the menu lands
                    // in the same place however the pointer arrived.
                    onclick: move |e: MouseEvent| {
                        e.stop_propagation();
                        let c = e.client_coordinates();
                        on_menu.call((id, c.x, c.y));
                    },
                    span {
                        class: "block w-2.5 h-2.5",
                        dangerous_inner_html: crate::features::icons::GEAR,
                    }
                }
            }
        }
    }
}

/// Inline member picker for the "Transfer ownership" confirm step. Lists the
/// guild's human members (excluding yourself); clicking one arms a final
/// confirm button.
#[component]
fn TransferPicker(guild_id: Id, on_done: EventHandler<()>) -> Element {
    let state = use_app_state();
    let gateway = use_gateway();
    let mut chosen = use_signal(|| None::<(String, String)>); // (pubkey, name)

    let candidates: Vec<(String, String)> = {
        let s = state.read();
        let me = s
            .self_user
            .as_ref()
            .map(|u| u.pubkey.clone())
            .unwrap_or_default();
        s.members
            .iter()
            .filter(|m| m.guild_id == guild_id && !m.bot && m.user.pubkey != me)
            .map(|m| (m.user.pubkey.clone(), m.user.username.clone()))
            .collect()
    };

    rsx! {
        div { class: "px-3 py-1.5 text-xs text-[var(--text-muted)]",
            "Hand this guild to… (you keep membership, they get the crown)"
        }
        if candidates.is_empty() {
            div { class: "px-3 pb-1.5 text-xs text-[var(--text-dim)]", "No other members yet." }
        }
        div { class: "max-h-40 overflow-y-auto",
            for (pk, name) in candidates {
                {
                    let selected = chosen().map(|(c, _)| c == pk).unwrap_or(false);
                    let pk2 = pk.clone();
                    let name2 = name.clone();
                    let row_cls = if selected { "text-[var(--warn)]" } else { "text-[var(--text)]" };
                    rsx! {
                        button {
                            key: "{pk}",
                            class: "w-full text-left px-3 py-1 rounded {row_cls} hover:bg-white/[0.04] transition-colors text-xs truncate",
                            onclick: move |_| chosen.set(Some((pk2.clone(), name2.clone()))),
                            "{name}"
                        }
                    }
                }
            }
        }
        if let Some((pk, name)) = chosen() {
            div { class: "flex gap-1 px-1 py-1 border-t border-[var(--border)] mt-1",
                button {
                    class: "flex-1 px-2 py-1 rounded text-xs uppercase tracking-wider text-[var(--warn)] border border-[var(--warn)]/40 hover:bg-[var(--warn)]/10 transition-colors",
                    onclick: {
                        let gw = gateway.clone();
                        move |_| {
                            gw.send(ClientMessage::TransferOwnership {
                                guild_id,
                                new_owner_pubkey: pk.clone(),
                            });
                            on_done.call(());
                        }
                    },
                    "Transfer to {name}"
                }
                button {
                    class: "px-2 py-1 rounded text-xs uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--text-muted)] transition-colors",
                    onclick: move |_| on_done.call(()),
                    "Cancel"
                }
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
