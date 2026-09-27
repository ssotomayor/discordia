//! Mounted at the workspace root: the channel list is a grid panel, and a
//! raised one traps `position: fixed` children (trap 22).

use dioxus::prelude::*;

use crate::protocol::{Channel, ChannelAccess, ClientMessage, MAX_ACCESS_USERS, Permission};
use crate::state::{use_app_state, use_gateway};

#[component]
pub fn ChannelAccessHost() -> Element {
    let state = use_app_state();
    let open = state
        .read()
        .channel_access_open
        .and_then(|cid| state.read().channels.iter().find(|c| c.id == cid).cloned());
    // A one-element list, so opening another channel remounts the drafts (trap 6).
    rsx! {
        for channel in open {
            ChannelAccessDialog { key: "{channel.id}", channel }
        }
    }
}

#[component]
fn ChannelAccessDialog(channel: Channel) -> Element {
    let mut state = use_app_state();
    let gateway = use_gateway();
    let guild_id = channel.guild_id;
    let channel_id = channel.id;

    let mut restricted = use_signal(|| channel.access.is_some());
    let mut roles = use_signal(|| channel.access.clone().unwrap_or_default().roles);
    let mut users = use_signal(|| channel.access.clone().unwrap_or_default().users);
    let mut filter = use_signal(String::new);

    let guild_roles = state.read().roles_of(guild_id).to_vec();
    let managers_note = state.read().can(guild_id, Permission::ManageChannels);
    let people: Vec<(String, String)> = {
        let s = state.read();
        let needle = filter().trim().to_lowercase();
        let mut v: Vec<(String, String)> = s
            .members
            .iter()
            .filter(|m| m.guild_id == guild_id && !m.bot)
            .map(|m| (m.user.pubkey.clone(), s.display_name(&m.user.pubkey)))
            .filter(|(pk, name)| {
                needle.is_empty()
                    || name.to_lowercase().contains(&needle)
                    || pk.starts_with(&needle)
            })
            .collect();
        // The chosen first, so a long member list never hides who is already in.
        v.sort_by(|a, b| {
            let (ia, ib) = (users.read().contains(&a.0), users.read().contains(&b.0));
            ib.cmp(&ia)
                .then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase()))
        });
        v
    };
    let chosen = users.read().len();

    let close = move |_| state.write().channel_access_open = None;
    let save = {
        let gateway = gateway.clone();
        move |_| {
            let access = restricted().then(|| ChannelAccess {
                roles: roles(),
                users: users(),
            });
            gateway.send(ClientMessage::SetChannelAccess { channel_id, access });
            state.write().channel_access_open = None;
        }
    };

    rsx! {
        div {
            class: "dxf-backdrop-in fixed inset-0 z-[70] flex items-center justify-center bg-black/50 p-6",
            onclick: close,
            div {
                class: "dxf-modal-in w-[480px] max-w-full max-h-full flex flex-col bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg shadow-xl overflow-hidden",
                onclick: move |e: MouseEvent| e.stop_propagation(),
                div { class: "px-4 py-3 border-b border-[var(--border)] flex items-center shrink-0",
                    h3 { class: "text-sm font-medium text-[var(--accent)] flex-1 truncate", "Who can see {channel.name}" }
                    button {
                        class: "text-[var(--text-dim)] hover:text-[var(--text)] text-lg leading-none",
                        onclick: close,
                        "✕"
                    }
                }
                div { class: "flex-1 overflow-y-auto p-4 space-y-3",
                    label { class: "flex items-center gap-2 text-xs text-[var(--text)] cursor-pointer select-none",
                        input {
                            r#type: "radio",
                            checked: !restricted(),
                            onchange: move |_| restricted.set(false),
                        }
                        "Everyone in the guild"
                    }
                    label { class: "flex items-center gap-2 text-xs text-[var(--text)] cursor-pointer select-none",
                        input {
                            r#type: "radio",
                            checked: restricted(),
                            onchange: move |_| restricted.set(true),
                        }
                        "Only the roles and people below"
                    }
                    if restricted() {
                        div { class: "text-[10px] text-[var(--text-dim)]",
                            "Everyone else never sees the channel or who is in it. Anyone who can manage channels always sees it"
                            if managers_note { ", you included." } else { "." }
                        }
                        div {
                            div { class: "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)] mb-1", "Roles" }
                            if guild_roles.is_empty() {
                                div { class: "text-xs text-[var(--text-dim)]", "This guild has no roles yet." }
                            }
                            for role in guild_roles.iter().cloned() {
                                {
                                    let rid = role.id;
                                    let on = roles.read().contains(&rid);
                                    rsx! {
                                        label {
                                            key: "{rid}",
                                            class: "flex items-center gap-2 py-0.5 text-xs text-[var(--text)] cursor-pointer select-none",
                                            input {
                                                r#type: "checkbox",
                                                checked: on,
                                                onchange: move |_| toggle(&mut roles, rid),
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
                        div {
                            div { class: "flex items-center gap-2 mb-1",
                                div { class: "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)] flex-1", "People" }
                                div { class: "text-[10px] text-[var(--text-dim)]", "{chosen} chosen" }
                            }
                            input {
                                class: "w-full mb-1.5 bg-transparent border border-[var(--border)] focus:border-[var(--accent)] rounded px-2 py-1 text-xs text-[var(--text)] outline-none",
                                placeholder: "Find a member by name or key…",
                                value: "{filter}",
                                oninput: move |e| filter.set(e.value()),
                            }
                            div { class: "max-h-56 overflow-y-auto",
                                for (pk, name) in people.into_iter() {
                                    {
                                        let on = users.read().contains(&pk);
                                        let full = !on && chosen >= MAX_ACCESS_USERS;
                                        let short = crate::identity::truncate_pubkey(&pk);
                                        let key = pk.clone();
                                        rsx! {
                                            label {
                                                key: "{key}",
                                                class: "flex items-center gap-2 py-0.5 text-xs text-[var(--text)] cursor-pointer select-none",
                                                input {
                                                    r#type: "checkbox",
                                                    checked: on,
                                                    disabled: full,
                                                    onchange: move |_| toggle(&mut users, pk.clone()),
                                                }
                                                span { class: "truncate flex-1", "{name}" }
                                                span { class: "font-mono text-[10px] text-[var(--text-dim)]", "{short}" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                div { class: "px-3 py-2.5 border-t border-[var(--border)] flex items-center justify-end gap-2 shrink-0",
                    button {
                        class: "rounded px-3 py-1 text-[10px] uppercase tracking-wider text-[var(--text-muted)] border border-[var(--border)] hover:border-[var(--border-strong)] transition-colors",
                        onclick: close,
                        "Cancel"
                    }
                    button {
                        class: "dxf-cta rounded px-4 py-1.5 text-[11px] uppercase tracking-wider",
                        onclick: save,
                        "Save"
                    }
                }
            }
        }
    }
}

fn toggle<T: PartialEq + 'static>(list: &mut Signal<Vec<T>>, item: T) {
    let mut v = list.write();
    match v.iter().position(|x| *x == item) {
        Some(i) => {
            v.remove(i);
        }
        None => v.push(item),
    }
}
