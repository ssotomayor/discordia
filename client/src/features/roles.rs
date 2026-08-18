//! "Roles" dialog: create/edit/delete a guild's roles and their permission
//! grants. Requires `ManageRoles` (the server enforces the grant-subset rule:
//! you can only hand out permissions you hold, and roles carrying
//! `ManageRoles`/`ManageGuild` are owner-touch-only — those two are shown
//! disabled for non-owners).
//!
//! Assigning roles to people happens in the member list's context menu.

use dioxus::prelude::*;

use crate::protocol::{ClientMessage, Id, Permission, Role};
use crate::state::{use_app_state, use_gateway};

#[component]
pub fn RolesDialog(guild_id: Id, on_close: EventHandler<()>) -> Element {
    let state = use_app_state();
    let gateway = use_gateway();

    // Live role list — arrives in Ready/GuildJoined and stays fresh via
    // GuildRoles pushes; no fetch-on-open needed.
    let roles: Vec<Role> = use_memo(move || state.read().roles_of(guild_id).to_vec())();
    let is_owner = state.read().is_owner(guild_id);

    // Editing state: None = creating a new role.
    let mut editing = use_signal(|| None::<Id>);
    let mut name = use_signal(String::new);
    let mut color = use_signal(|| None::<String>);
    let mut perms = use_signal(Vec::<Permission>::new);

    let mut reset_form = move || {
        editing.set(None);
        name.set(String::new());
        color.set(None);
        perms.set(Vec::new());
    };

    let mut submit = {
        let gateway = gateway.clone();
        move || {
            let n = name().trim().to_string();
            if n.is_empty() {
                return;
            }
            match editing() {
                Some(role_id) => gateway.send(ClientMessage::UpdateRole {
                    guild_id,
                    role_id,
                    name: n,
                    color: color(),
                    permissions: perms(),
                }),
                None => gateway.send(ClientMessage::CreateRole {
                    guild_id,
                    name: n,
                    color: color(),
                    permissions: perms(),
                }),
            }
            reset_form();
        }
    };

    rsx! {
        div {
            class: "dxf-backdrop-in fixed inset-0 z-50 flex items-center justify-center bg-black/50",
            onclick: move |_| on_close.call(()),
            div {
                class: "dxf-modal-in w-[26rem] max-h-[80vh] flex flex-col bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg shadow-xl overflow-hidden",
                onclick: move |e| e.stop_propagation(),
                div { class: "px-4 py-3 border-b border-[var(--border)] flex items-center",
                    h3 { class: "text-sm font-medium text-[var(--accent)] flex-1", "Roles" }
                    button {
                        class: "text-[var(--text-dim)] hover:text-[var(--text)] text-lg leading-none",
                        onclick: move |_| on_close.call(()),
                        "✕"
                    }
                }
                div { class: "flex-1 overflow-y-auto p-3 space-y-4",
                    div {
                        div { class: "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)] mb-1.5",
                            "Defined roles"
                        }
                        if roles.is_empty() {
                            div { class: "text-xs text-[var(--text-dim)] py-2",
                                "No roles yet. Create one below, then assign it from the member list (right-click a member)."
                            }
                        }
                        for role in roles.iter().cloned() {
                            {
                                let gw_del = gateway.clone();
                                let rid = role.id;
                                let r_name = role.name.clone();
                                let r_color = role.color.clone();
                                let r_perms = role.permissions.clone();
                                let selected = editing() == Some(rid);
                                let row_cls = if selected {
                                    "border-[var(--accent)]"
                                } else {
                                    "border-[var(--border)] hover:border-[var(--border-strong)]"
                                };
                                rsx! {
                                    div {
                                        key: "{rid}",
                                        class: "border {row_cls} rounded-md p-2.5 flex flex-col gap-1.5 cursor-pointer transition-colors",
                                        onclick: move |_| {
                                            editing.set(Some(rid));
                                            name.set(r_name.clone());
                                            color.set(r_color.clone());
                                            perms.set(r_perms.clone());
                                        },
                                        div { class: "flex items-center gap-2",
                                            span {
                                                class: "w-3 h-3 rounded-full shrink-0 border border-[var(--border)]",
                                                style: format!(
                                                    "background: {};",
                                                    role.color.as_deref().unwrap_or("transparent")
                                                ),
                                            }
                                            span { class: "text-sm text-[var(--text)] font-medium truncate flex-1",
                                                "{role.name}"
                                            }
                                            button {
                                                class: "text-[10px] uppercase tracking-wider text-[var(--danger)] border border-[var(--danger)]/40 rounded px-2 py-0.5 hover:bg-[var(--danger)]/10 transition-colors",
                                                onclick: move |e| {
                                                    e.stop_propagation();
                                                    gw_del.send(ClientMessage::DeleteRole { guild_id, role_id: rid });
                                                },
                                                "Delete"
                                            }
                                        }
                                        div { class: "flex flex-wrap gap-1",
                                            for p in role.permissions.iter().copied() {
                                                span { class: "text-[9px] px-1.5 py-px rounded bg-white/[0.04] text-[var(--text-muted)]",
                                                    "{p.label()}"
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    div { class: "border-t border-[var(--border)] pt-3",
                        div { class: "flex items-center mb-1.5",
                            span { class: "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)] flex-1",
                                if editing().is_some() { "Edit role" } else { "New role" }
                            }
                            if editing().is_some() {
                                button {
                                    class: "text-[10px] uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--text-muted)]",
                                    onclick: move |_| reset_form(),
                                    "+ new instead"
                                }
                            }
                        }
                        div { class: "space-y-2",
                            div { class: "flex items-center gap-2",
                                input {
                                    class: "flex-1 bg-transparent border border-[var(--border)] focus:border-[var(--accent)] rounded px-2 py-1 text-xs text-[var(--text)] outline-none transition-colors",
                                    placeholder: "Role name (e.g. Moderator)",
                                    value: "{name}",
                                    maxlength: 32,
                                    oninput: move |e| name.set(e.value()),
                                }
                                input {
                                    r#type: "color",
                                    class: "w-7 h-7 rounded border border-[var(--border)] bg-transparent cursor-pointer",
                                    value: "{color().unwrap_or_else(|| \"#e0a06a\".into())}",
                                    oninput: move |e| color.set(Some(e.value())),
                                }
                            }
                            div { class: "text-[10px] uppercase tracking-wider text-[var(--text-dim)] pt-1", "Permissions" }
                            for p in Permission::ALL.iter().copied() {
                                {
                                    let owner_only = matches!(p, Permission::ManageRoles | Permission::ManageGuild);
                                    let locked = owner_only && !is_owner;
                                    rsx! {
                                        label {
                                            class: if locked {
                                                "flex items-center gap-2 text-xs text-[var(--text-dim)] cursor-not-allowed select-none"
                                            } else {
                                                "flex items-center gap-2 text-xs text-[var(--text)] cursor-pointer select-none"
                                            },
                                            input {
                                                r#type: "checkbox",
                                                disabled: locked,
                                                checked: perms.read().contains(&p),
                                                onchange: move |_| {
                                                    let mut v = perms.write();
                                                    if let Some(i) = v.iter().position(|x| *x == p) { v.remove(i); }
                                                    else { v.push(p); }
                                                },
                                            }
                                            span { "{p.label()}" }
                                            if owner_only {
                                                span { class: "text-[8px] px-1 py-px rounded bg-[var(--warn)]/15 text-[var(--warn)] uppercase tracking-wider font-semibold",
                                                    "Owner-only"
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            button {
                                class: "w-full mt-1 rounded px-2 py-1.5 text-[11px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors",
                                onclick: move |_| submit(),
                                if editing().is_some() { "Save role" } else { "Create role" }
                            }
                        }
                    }
                }
            }
        }
    }
}
