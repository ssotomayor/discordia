use dioxus::prelude::*;
use dioxus_grid_layout::NoDrag;

use crate::protocol::{ClientMessage, Id, Member, Permission, VoiceState};
use crate::state::{use_app_state, use_gateway};

/// Right-click moderation menu over a member row.
#[derive(Clone, PartialEq)]
struct MemberMenu {
    guild_id: Id,
    pubkey: String,
    username: String,
    x: f64,
    y: f64,
    /// Armed destructive action awaiting inline confirm.
    confirming: Option<ModAction>,
}

#[derive(Clone, Copy, PartialEq)]
enum ModAction {
    Kick,
    Ban,
}

#[component]
pub fn MembersPanel() -> Element {
    let state = use_app_state();
    let snapshot = state.read();

    let guild_id = snapshot.selected_guild;
    let members: Vec<Member> = guild_id
        .map(|gid| {
            snapshot
                .members_of(gid)
                .into_iter()
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let voice_states: Vec<VoiceState> = snapshot.voice_states.clone();
    // Moderation affordances for the current guild (server re-checks; this
    // just decides whether right-click opens a menu at all).
    let (can_kick, can_ban, can_roles, owner_pk, self_pk) = guild_id
        .map(|gid| {
            (
                snapshot.can(gid, Permission::KickMembers),
                snapshot.can(gid, Permission::BanMembers),
                snapshot.can(gid, Permission::ManageRoles),
                snapshot
                    .guilds
                    .iter()
                    .find(|g| g.id == gid)
                    .map(|g| g.owner_pubkey.clone())
                    .unwrap_or_default(),
                snapshot
                    .self_user
                    .as_ref()
                    .map(|u| u.pubkey.clone())
                    .unwrap_or_default(),
            )
        })
        .unwrap_or((false, false, false, String::new(), String::new()));
    drop(snapshot);

    let can_moderate = can_kick || can_ban || can_roles;
    let mut menu = use_signal::<Option<MemberMenu>>(|| None);

    let online_count = members.iter().filter(|m| m.online).count();
    let offline_count = members.len() - online_count;

    // A row is context-menu-able if we hold ANY moderation power and the
    // target isn't ourselves, a bot (uninstall via Integrations), or the owner.
    let on_context = {
        let owner_pk = owner_pk.clone();
        let self_pk = self_pk.clone();
        move |(member, x, y): (Member, f64, f64)| {
            if !can_moderate
                || member.bot
                || member.user.pubkey == self_pk
                || member.user.pubkey == owner_pk
            {
                return;
            }
            menu.set(Some(MemberMenu {
                guild_id: member.guild_id,
                pubkey: member.user.pubkey.clone(),
                username: member.user.username.clone(),
                x,
                y,
                confirming: None,
            }));
        }
    };

    rsx! {
        aside { class: "panel-hover w-full h-full bg-[var(--panel)] border border-[var(--border)] rounded-lg flex flex-col overflow-hidden",
            // Header — drag surface in edit mode.
            div { class: "h-11 px-3 flex items-center border-b border-[var(--border)]",
                h2 { class: "text-sm text-[var(--accent)] truncate font-medium", "Members" }
                span { class: "ml-auto text-[10px] text-[var(--text-dim)] uppercase tracking-wider",
                    "{online_count} online"
                }
            }
            NoDrag {
                div { class: "flex-1 overflow-y-auto py-3 space-y-3",
                    if online_count > 0 {
                        Section {
                            label: format!("Online — {online_count}"),
                            members: members.iter().filter(|m| m.online).cloned().collect::<Vec<_>>(),
                            voice_states: voice_states.clone(),
                            on_context: on_context.clone(),
                        }
                    }
                    if offline_count > 0 {
                        Section {
                            label: format!("Offline — {offline_count}"),
                            members: members.iter().filter(|m| !m.online).cloned().collect::<Vec<_>>(),
                            voice_states: Vec::new(),
                            on_context: on_context.clone(),
                        }
                    }
                    if members.is_empty() {
                        div { class: "px-4 text-xs text-[var(--text-dim)]", "No members" }
                    }
                }
            }

            if let Some(m) = menu() {
                MemberMenuPopover {
                    menu: m,
                    can_kick,
                    can_ban,
                    can_roles,
                    on_close: move |_| menu.set(None),
                    on_confirm: move |action: ModAction| {
                        if let Some(cur) = menu.write().as_mut() {
                            cur.confirming = Some(action);
                        }
                    },
                }
            }
        }
    }
}

/// The floating moderation popover (assign roles, kick, ban).
#[component]
fn MemberMenuPopover(
    menu: MemberMenu,
    can_kick: bool,
    can_ban: bool,
    can_roles: bool,
    on_close: EventHandler<()>,
    on_confirm: EventHandler<ModAction>,
) -> Element {
    let state = use_app_state();
    let gateway = use_gateway();

    let gid = menu.guild_id;
    let target_pk = menu.pubkey.clone();
    // The target's current role assignment + the guild's role list.
    let (target_roles, guild_roles) = {
        let s = state.read();
        let assigned = s
            .members
            .iter()
            .find(|m| m.guild_id == gid && m.user.pubkey == target_pk)
            .map(|m| m.roles.clone())
            .unwrap_or_default();
        (assigned, s.roles_of(gid).to_vec())
    };

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
                    "{menu.username}"
                }
                match menu.confirming {
                    None => rsx! {
                        if can_roles && !guild_roles.is_empty() {
                            div { class: "px-3 py-1 text-[10px] uppercase tracking-wider text-[var(--text-dim)]", "Roles" }
                            for role in guild_roles.iter().cloned() {
                                {
                                    let has = target_roles.contains(&role.id);
                                    let gw = gateway.clone();
                                    let pk = menu.pubkey.clone();
                                    let rid = role.id;
                                    rsx! {
                                        label {
                                            key: "{rid}",
                                            class: "flex items-center gap-2 px-3 py-1 text-xs text-[var(--text)] cursor-pointer select-none hover:bg-white/[0.04] rounded",
                                            input {
                                                r#type: "checkbox",
                                                checked: has,
                                                onchange: move |_| {
                                                    let msg = if has {
                                                        ClientMessage::UnassignRole { guild_id: gid, role_id: rid, user_pubkey: pk.clone() }
                                                    } else {
                                                        ClientMessage::AssignRole { guild_id: gid, role_id: rid, user_pubkey: pk.clone() }
                                                    };
                                                    gw.send(msg);
                                                },
                                            }
                                            span {
                                                style: role.color.as_deref().map(|c| format!("color: {c};")).unwrap_or_default(),
                                                "{role.name}"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        if can_kick {
                            button {
                                class: "w-full text-left px-3 py-1.5 rounded text-[var(--danger)] hover:bg-[var(--danger)]/10 transition-colors",
                                onclick: move |_| on_confirm.call(ModAction::Kick),
                                "Kick"
                            }
                        }
                        if can_ban {
                            button {
                                class: "w-full text-left px-3 py-1.5 rounded text-[var(--danger)] hover:bg-[var(--danger)]/10 transition-colors",
                                onclick: move |_| on_confirm.call(ModAction::Ban),
                                "Ban"
                            }
                        }
                    },
                    Some(action) => {
                        let (verb, msg_hint) = match action {
                            ModAction::Kick => ("Kick", "They can rejoin later."),
                            ModAction::Ban => ("Ban", "They won't be able to rejoin, even by invite."),
                        };
                        let gw = gateway.clone();
                        let pk = menu.pubkey.clone();
                        rsx! {
                            div { class: "px-3 py-1.5 text-xs text-[var(--text-muted)]",
                                "{verb} {menu.username}? {msg_hint}"
                            }
                            div { class: "flex gap-1 px-1 pb-0.5",
                                button {
                                    class: "flex-1 px-2 py-1 rounded text-xs uppercase tracking-wider text-[var(--danger)] border border-[var(--danger)]/40 hover:bg-[var(--danger)]/10 transition-colors",
                                    onclick: move |_| {
                                        let msg = match action {
                                            ModAction::Kick => ClientMessage::KickMember { guild_id: gid, user_pubkey: pk.clone() },
                                            ModAction::Ban => ClientMessage::BanMember { guild_id: gid, user_pubkey: pk.clone() },
                                        };
                                        gw.send(msg);
                                        on_close.call(());
                                    },
                                    "{verb}"
                                }
                                button {
                                    class: "flex-1 px-2 py-1 rounded text-xs uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--text-muted)] transition-colors",
                                    onclick: move |_| on_close.call(()),
                                    "Cancel"
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
fn Section(
    label: String,
    members: Vec<Member>,
    voice_states: Vec<VoiceState>,
    on_context: EventHandler<(Member, f64, f64)>,
) -> Element {
    rsx! {
        div {
            div { class: "px-3 mb-1 text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)]",
                "{label}"
            }
            for m in members.iter() {
                {
                    let vs = voice_states
                        .iter()
                        .find(|v| v.user_pubkey == m.user.pubkey)
                        .cloned();
                    rsx! {
                        MemberRow {
                            key: "{m.user.pubkey}",
                            member: m.clone(),
                            voice: vs,
                            on_context,
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn MemberRow(
    member: Member,
    voice: Option<VoiceState>,
    on_context: EventHandler<(Member, f64, f64)>,
) -> Element {
    let mut state = use_app_state();
    let is_self = state
        .read()
        .self_user
        .as_ref()
        .map(|u| u.pubkey == member.user.pubkey)
        .unwrap_or(false);

    let name_class = if member.online {
        "text-[var(--text)]"
    } else {
        "text-[var(--text-dim)]"
    };
    let speaking = voice.as_ref().map(|v| v.speaking).unwrap_or(false);
    let speaking_ring = if speaking {
        "ring-1 ring-[var(--accent)]"
    } else {
        ""
    };

    let dim = if member.online { "" } else { "opacity-60" };
    let card_pubkey = member.user.pubkey.clone();

    // Presence: connection state trumps the self-set status (see
    // `AppState::presence_of`); only actually-online users pulse.
    let (dot_color, pulse) = {
        let status = state.read().presence_of(&member.user.pubkey).to_string();
        let pulse = if status == "online" {
            "dxf-dot-pulse"
        } else {
            ""
        };
        (crate::features::profiles::status_color(&status), pulse)
    };

    // Level from the member's per-guild XP; status subtitle from the profile.
    let level = crate::protocol::level_progress(member.xp).0;
    let subtitle = state
        .read()
        .profile_of(&member.user.pubkey)
        .and_then(|p| p.custom_status.clone());

    let ctx_member = member.clone();
    rsx! {
        div {
            class: "flex items-center gap-2.5 px-3 py-1.5 rounded-lg hover:bg-white/[0.03] cursor-pointer",
            title: if is_self { "Click to view your profile" } else { "Click to view profile" },
            // A single click opens the profile card; DMs are started from the
            // card's "Send Message" button. Right-click opens the moderation
            // menu (when the viewer holds any moderation permission).
            onclick: move |_| state.write().profile_card = Some(card_pubkey.clone()),
            oncontextmenu: move |e: MouseEvent| {
                e.prevent_default();
                let c = e.client_coordinates();
                on_context.call((ctx_member.clone(), c.x, c.y));
            },
            div { class: "relative shrink-0",
                crate::features::profiles::Avatar {
                    pubkey: member.user.pubkey.clone(),
                    name: member.user.username.clone(),
                    size: "w-7 h-7 {dim} {speaking_ring}",
                }
                span {
                    class: "absolute -bottom-0.5 -right-0.5 w-2.5 h-2.5 rounded-full border-2 border-[var(--panel-solid)] {pulse}",
                    style: "background:{dot_color}; color:{dot_color};",
                }
            }
            // Name (+ verified/bot) with an optional status subtitle beneath.
            div { class: "flex flex-col min-w-0 flex-1",
                span {
                    class: "text-sm truncate flex items-center gap-1 {name_class}",
                    title: "{member.user.pubkey}",
                    "{member.user.username}"
                    if member.bot {
                        span {
                            class: "dxf-pop px-1 py-px rounded bg-[var(--accent-soft)] text-[var(--accent)] text-[8px] font-bold uppercase tracking-wider",
                            title: "Installed bot",
                            "Bot"
                        }
                    } else {
                        span { class: "text-[var(--up)] text-xs", title: "Key verified", "✓" }
                    }
                }
                if let Some(sub) = subtitle {
                    span { class: "text-[10px] text-[var(--text-dim)] truncate", "{sub}" }
                }
            }
            if let Some(vs) = voice {
                VoiceBadges { vs: vs }
            }
            span { class: "text-[10px] font-semibold text-[var(--text-dim)] shrink-0", "Lv{level}" }
        }
    }
}

#[component]
fn VoiceBadges(vs: VoiceState) -> Element {
    rsx! {
        div { class: "flex items-center gap-1 text-[10px] uppercase tracking-wider",
            if vs.channel_id.is_some() {
                span { class: "text-[var(--accent)]", title: "In voice", "v" }
            }
            // Deafened implies muted server-side, so the two never both show.
            if vs.deafened {
                span { class: "text-[var(--text-dim)]", title: "Deafened", "d" }
            } else if vs.muted {
                span { class: "text-[var(--text-dim)]", title: "Muted", "m" }
            }
        }
    }
}
