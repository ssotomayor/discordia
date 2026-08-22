//! The contact list, finally visible.
//!
//! `nostr::nip02` has carried it since DMs moved off the gateway: a kind:3
//! event signed by your key, so a fresh install on another machine pulls your
//! people back from the key alone. Until now the only thing that read it was a
//! button on a profile deciding whether to say "add" or "remove" — the list
//! synced, survived and was never once shown.
//!
//! **The module's warning becomes a design requirement here.** Adding someone
//! is a public act and messaging them is not, and with both on screen at once
//! the difference has to be stated rather than assumed. That is what the line
//! under the header is for; it is not decoration.

use dioxus::prelude::*;
use dioxus_grid_layout::NoDrag;

use crate::nostr::service::{NostrCmd, NostrTx};
use crate::state::use_app_state;

#[component]
pub fn FriendsPanel() -> Element {
    let state = use_app_state();
    let nostr = use_context::<NostrTx>();
    let mut adding = use_signal(String::new);
    let mut add_error = use_signal(|| Option::<String>::None);
    let mut renaming = use_signal(|| Option::<String>::None);

    let contacts = state.read().contacts.contacts.clone();

    let mut add = move || {
        let raw = adding().trim().to_string();
        if raw.is_empty() {
            return;
        }
        match crate::identity::pubkey_from_input(&raw) {
            Ok(pubkey) => {
                add_error.set(None);
                adding.set(String::new());
                nostr.send(NostrCmd::SetContact {
                    peer: pubkey,
                    keep: true,
                });
            }
            Err(e) => add_error.set(Some(e)),
        }
    };

    rsx! {
        div { class: "panel-hover w-full h-full bg-[var(--panel)] border border-[var(--border)] rounded-lg flex flex-col overflow-hidden",
            div { class: "px-3 py-2 border-b border-[var(--border)] shrink-0",
                div { class: "flex items-center gap-2",
                    span { class: "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)] flex-1",
                        "Friends"
                    }
                    if !contacts.is_empty() {
                        span { class: "text-[10px] text-[var(--text-dim)] font-mono",
                            "{contacts.len()}"
                        }
                    }
                }
                div { class: "text-[10px] text-[var(--text-dim)] leading-relaxed mt-1",
                    "Anyone can see who you add — this list is public and signed by your key. What you say to them is not."
                }
            }

            NoDrag {
            div { class: "px-2 pt-2 shrink-0 space-y-1",
                input {
                    class: "w-full bg-[var(--bg2)] border border-[var(--border)] rounded px-2 py-1 text-xs outline-none focus:border-[var(--accent)]",
                    r#type: "text",
                    placeholder: "Add by npub1… or hex key",
                    value: "{adding}",
                    oninput: move |e| { adding.set(e.value()); add_error.set(None); },
                    onkeydown: move |e| {
                        if e.key() == Key::Enter { add(); }
                    },
                }
                if let Some(err) = add_error() {
                    div { class: "px-1 text-[10px] text-[var(--danger)]", "{err}" }
                }
            }

            div { class: "flex-1 overflow-y-auto px-2 py-2 space-y-1",
                if contacts.is_empty() {
                    div { class: "px-1 text-xs text-[var(--text-dim)] leading-relaxed",
                        "Nobody yet. Add someone by key above, and they come with you to any machine you sign in on."
                    }
                }
                for contact in contacts.iter().cloned() {
                    {
                        let pk = contact.pubkey.clone();
                        let shown = state.read().person_name(&pk);
                        let has_petname = contact.petname.is_some();
                        let is_renaming = renaming() == Some(pk.clone());
                        rsx! {
                            FriendRow {
                                key: "{pk}",
                                pubkey: pk.clone(),
                                shown,
                                petname: contact.petname.clone(),
                                has_petname,
                                renaming: is_renaming,
                                on_rename_open: move |open: bool| {
                                    renaming.set(if open { Some(pk.clone()) } else { None });
                                },
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
fn FriendRow(
    pubkey: String,
    shown: String,
    petname: Option<String>,
    has_petname: bool,
    renaming: bool,
    on_rename_open: EventHandler<bool>,
) -> Element {
    let nostr = use_context::<NostrTx>();
    let mut draft = use_signal(|| petname.clone().unwrap_or_default());

    let pk_open = pubkey.clone();
    let pk_save = pubkey.clone();
    let pk_drop = pubkey.clone();
    let nostr_open = nostr.clone();
    let nostr_save = nostr.clone();

    let commit = move || {
        let value = draft().trim().to_string();
        nostr_save.send(NostrCmd::SetPetname {
            peer: pk_save.clone(),
            petname: Some(value),
        });
        on_rename_open.call(false);
    };

    rsx! {
        div { class: "group flex items-center gap-2 px-2 py-1 rounded hover:bg-white/[0.03] transition-colors",
            crate::features::profiles::Avatar {
                pubkey: pubkey.clone(),
                name: shown.clone(),
                size: "w-6 h-6",
                text: "text-[10px]",
            }
            if renaming {
                input {
                    class: "flex-1 min-w-0 bg-transparent border border-[var(--border)] rounded px-1 py-0.5 text-xs outline-none focus:border-[var(--accent)]",
                    r#type: "text",
                    autofocus: true,
                    // Yours, not theirs: a petname travels with your list rather
                    // than with their profile, so leaving it empty falls back to
                    // whatever they call themselves.
                    placeholder: "your name for them",
                    value: "{draft}",
                    oninput: move |e| draft.set(e.value()),
                    onkeydown: move |e| {
                        if e.key() == Key::Enter { commit(); }
                        if e.key() == Key::Escape { on_rename_open.call(false); }
                    },
                }
            } else {
                button {
                    r#type: "button",
                    class: "flex-1 min-w-0 text-left truncate text-sm text-[var(--text-muted)] hover:text-[var(--accent)] transition-colors",
                    title: "Open a conversation",
                    onclick: move |_| {
                        nostr_open.send(NostrCmd::Open { peer: pk_open.clone() });
                    },
                    "{shown}"
                }
                button {
                    r#type: "button",
                    class: "shrink-0 opacity-0 group-hover:opacity-100 text-[10px] uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--accent)] transition-all",
                    title: "Rename — only you see this",
                    onclick: move |_| on_rename_open.call(true),
                    if has_petname { "renamed" } else { "name" }
                }
                button {
                    r#type: "button",
                    class: "shrink-0 opacity-0 group-hover:opacity-100 px-1 text-[var(--text-dim)] hover:text-[var(--danger)] text-xs transition-all",
                    title: "Remove — this republishes your public list without them",
                    onclick: move |_| {
                        nostr.send(NostrCmd::SetContact { peer: pk_drop.clone(), keep: false });
                    },
                    "✕"
                }
            }
        }
    }
}
