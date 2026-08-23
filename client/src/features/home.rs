//! The connect screen, with the half that needs no server beside it: DMs are
//! Nostr events on relays, so arriving somewhere was never a precondition.

use dioxus::prelude::*;

use crate::identity::Identity;
use crate::session::SavedSession;
use crate::state::{AppState, DmInfo, GatewayTx, SessionParams, use_app_state};

#[component]
pub fn HomeView(
    identity: Identity,
    error: Option<String>,
    last_session: Option<SavedSession>,
    on_connect: EventHandler<SessionParams>,
    on_rename: EventHandler<String>,
    on_sign_out: EventHandler<()>,
) -> Element {
    let state = use_signal(AppState::empty);
    let settings = use_context::<Signal<crate::settings::ClientSettings>>();

    let nostr_tx = use_hook(|| {
        let relays = {
            let saved = settings.read();
            if saved.dm_relays.is_empty() {
                crate::nostr::relay::DEFAULT_RELAYS
                    .iter()
                    .map(|s| s.to_string())
                    .collect()
            } else {
                saved.dm_relays.clone()
            }
        };
        crate::nostr::service::spawn_nostr(identity.clone(), relays, state)
    });

    provide_context(state);
    provide_context(nostr_tx.clone());
    provide_context(identity.clone());
    // Every panel asks for a gateway; there is none here. Safe only because
    // `chat.rs` routes by `dm_of` and no non-DM channel can exist on this screen.
    provide_context(use_hook(|| {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        GatewayTx(tx)
    }));

    let mac_top_pad = if cfg!(target_os = "macos") {
        "pt-5"
    } else {
        ""
    };

    rsx! {
        div { class: "h-full w-full flex flex-col bg-[var(--bg)] {mac_top_pad}",
            crate::features::chat::ImageViewer {}
            crate::features::profiles::ProfileCard {}
            TopBar { identity: identity.clone(), on_rename, on_sign_out }
            div { class: "flex-1 flex min-h-0",
                // Half the window, as the comp draws it. `flex-1` split what was
                // left after the talk column and gave the form 42%.
                div { class: "basis-1/2 shrink-0 min-w-0 border-r border-[var(--border)] flex flex-col",
                    div { class: "px-5 pt-4 pb-2 shrink-0",
                        h2 { class: "dxf-display text-[15px] font-semibold text-[var(--text)]",
                            "Go to a server"
                        }
                    }
                    crate::features::connect::ConnectForm {
                        identity: identity.clone(),
                        error,
                        last_session,
                        on_connect,
                    }
                }
                TalkHalf {}
            }
        }
    }
}

/// Brand on the left, who you are on the right, and the relay count between
/// them — the one number that says the talking half is actually working.
#[component]
fn TopBar(
    identity: Identity,
    on_rename: EventHandler<String>,
    on_sign_out: EventHandler<()>,
) -> Element {
    let state = use_app_state();
    let relays = state.read().nostr_relays_up.len();
    let mut open = use_signal(|| false);
    let npub = identity.npub();
    let short = format!("{}…{}", &npub[..9.min(npub.len())], &npub[npub.len() - 3..]);

    rsx! {
        div { class: "relative shrink-0",
            div {
                class: "dxf-drag-region h-12 px-3 flex items-center gap-2 border-b border-[var(--border)]",
                onmousedown: move |_| crate::app::start_window_drag(),
                crate::app::DiscordiaLogo { class: "w-5 h-5 shrink-0" }
                span { class: "dxf-display dxf-wordmark text-base font-bold tracking-tight", "Discordia" }
                span {
                    class: "px-2 py-0.5 rounded-full border text-[10px] font-mono",
                    style: if relays > 0 {
                        "color: var(--up); border-color: color-mix(in srgb, var(--up) 40%, transparent);"
                    } else {
                        "color: var(--warn); border-color: color-mix(in srgb, var(--warn) 40%, transparent);"
                    },
                    if relays == 1 { "1 relay" } else { "{relays} relays" }
                }
                div { class: "flex-1" }
                div { class: "dxf-no-drag flex items-center gap-2",
                    onmousedown: move |e| e.stop_propagation(),
                    crate::features::profiles::Avatar {
                        pubkey: identity.pubkey.clone(),
                        name: identity.display_name.clone(),
                        size: "w-7 h-7",
                        text: "text-[10px]",
                    }
                    div { class: "min-w-0 leading-tight",
                        div { class: "text-xs text-[var(--text)] truncate", "{identity.display_name}" }
                        div { class: "text-[9px] font-mono uppercase tracking-wider text-[var(--text-dim)]",
                            "{short}"
                        }
                    }
                    button {
                        class: "px-2.5 py-1 rounded-md border border-[var(--border)] text-[11px] text-[var(--text-muted)] hover:text-[var(--accent)] hover:border-[var(--accent)] transition-colors",
                        onclick: move |_| open.set(!open()),
                        "Account"
                    }
                }
            }
            if open() {
                div { class: "absolute right-3 top-12 z-50 w-80",
                    crate::features::connect::IdentityCard {
                        identity: identity.clone(),
                        on_rename,
                        on_sign_out,
                    }
                }
            }
        }
    }
}

/// The half that needs no server: conversations, and the people in them.
#[component]
fn TalkHalf() -> Element {
    let mut state = use_app_state();
    let nostr = use_context::<crate::nostr::service::NostrTx>();
    let mut filter = use_signal(String::new);
    let mut showing_friends = use_signal(|| false);

    let snapshot = state.read();
    let selected = snapshot.selected_channel;
    let self_pk = snapshot
        .self_user
        .as_ref()
        .map(|u| u.pubkey.clone())
        .unwrap_or_default();
    let rows: Vec<Row> = snapshot
        .dms_by_recency()
        .into_iter()
        .map(|dm| {
            let last = snapshot.dm_last_message(dm.channel_id);
            Row {
                preview: last.map(|m| {
                    let body = if m.content.trim().is_empty() && m.image.is_some() {
                        "📎 image".to_string()
                    } else {
                        m.content.clone()
                    };
                    if m.author.pubkey == self_pk {
                        format!("You: {body}")
                    } else {
                        body
                    }
                }),
                when: last.map(|m| m.created_at.format("%H:%M").to_string()),
                unread: snapshot.dm_unread.get(&dm.channel_id).copied().unwrap_or(0),
                info: dm,
            }
        })
        .collect();
    let contacts: Vec<(String, String)> = snapshot
        .contacts
        .contacts
        .iter()
        .filter(|c| c.pubkey != self_pk)
        .map(|c| {
            let n = c.petname.clone().unwrap_or_else(|| {
                let k = &c.pubkey;
                format!("npub…{}", &k[k.len().saturating_sub(6)..])
            });
            (c.pubkey.clone(), n)
        })
        .collect();
    drop(snapshot);

    let needle = filter().trim().to_lowercase();
    let shown: Vec<Row> = rows
        .iter()
        .filter(|r| r.matches(&needle))
        .cloned()
        .collect();

    rsx! {
        div { class: "w-[195px] shrink-0 border-r border-[var(--border)] flex flex-col min-h-0",
            div { class: "px-3 pt-4 pb-2 space-y-2 shrink-0",
                h2 { class: "dxf-display text-[15px] font-semibold text-[var(--text)]", "Talk" }
                input {
                    class: "w-full bg-transparent border border-[var(--border)] focus:border-[var(--accent)] rounded-md px-2.5 py-1.5 text-[11px] text-[var(--text)] outline-none transition-colors",
                    r#type: "text",
                    placeholder: "Search, or paste an npub…",
                    value: "{filter}",
                    oninput: move |e| filter.set(e.value()),
                    onkeydown: move |e| {
                        if e.key() != Key::Enter {
                            return;
                        }
                        // A typed name is not a failed key, so only a parse that
                        // succeeds does anything.
                        if let Ok(peer) = crate::identity::pubkey_from_input(filter().trim()) {
                            nostr.send(crate::nostr::service::NostrCmd::Open { peer });
                            filter.set(String::new());
                        }
                    },
                }
                div { class: "flex gap-1",
                    Chip {
                        label: "Direct".to_string(),
                        on: !showing_friends(),
                        onclick: move |_| showing_friends.set(false),
                    }
                    Chip {
                        label: "Friends".to_string(),
                        on: showing_friends(),
                        onclick: move |_| showing_friends.set(true),
                    }
                }
            }

            div { class: "flex-1 overflow-y-auto px-1.5 pb-2 space-y-0.5 min-h-0",
                if showing_friends() {
                    if contacts.is_empty() {
                        div { class: "px-2 py-3 text-[11px] text-[var(--text-dim)] leading-relaxed",
                            "No contacts yet."
                        }
                    }
                    for (pk, name) in contacts.iter().cloned() {
                        {
                            let nostr = nostr.clone();
                            let peer = pk.clone();
                            rsx! {
                                button {
                                    key: "ct-{pk}",
                                    class: "w-full flex items-center gap-2 px-2 py-1.5 rounded-md text-left hover:bg-white/[0.03] transition-colors",
                                    onclick: move |_| nostr.send(
                                        crate::nostr::service::NostrCmd::Open { peer: peer.clone() },
                                    ),
                                    crate::features::profiles::Avatar {
                                        pubkey: pk.clone(),
                                        name: name.clone(),
                                        size: "w-7 h-7",
                                        text: "text-[10px]",
                                    }
                                    span { class: "flex-1 min-w-0 truncate text-xs text-[var(--text-muted)]", "{name}" }
                                }
                            }
                        }
                    }
                } else {
                    if shown.is_empty() {
                        div { class: "px-2 py-3 text-[11px] text-[var(--text-dim)] leading-relaxed",
                            "No conversations yet. Paste someone's npub above — no server needed."
                        }
                    }
                    for row in shown.iter().cloned() {
                        {
                            let cid = row.info.channel_id;
                            let active = selected == Some(cid);
                            let cls = if active {
                                "bg-[var(--panel2)] border-[var(--border-strong)]"
                            } else {
                                "border-transparent hover:bg-white/[0.03]"
                            };
                            let uname = row.info.other.username.clone();
                            rsx! {
                                button {
                                    key: "{cid}",
                                    class: "w-full flex items-center gap-2 px-2 py-1.5 rounded-md border text-left transition-colors {cls}",
                                    onclick: move |_| {
                                        let mut s = state.write();
                                        s.dm_mode = true;
                                        s.selected_channel = Some(cid);
                                        s.dm_unread.remove(&cid);
                                    },
                                    span { class: "relative shrink-0",
                                        crate::features::profiles::Avatar {
                                            pubkey: row.info.other.pubkey.clone(),
                                            name: uname.clone(),
                                            size: "w-7 h-7",
                                            text: "text-[10px]",
                                        }
                                    }
                                    span { class: "flex-1 min-w-0",
                                        span { class: "flex items-baseline gap-1",
                                            span { class: "truncate text-xs text-[var(--text)]", "{uname}" }
                                            span { class: "flex-1" }
                                            if let Some(w) = row.when.clone() {
                                                span { class: "shrink-0 font-mono text-[9px] text-[var(--text-dim)]", "{w}" }
                                            }
                                        }
                                        if let Some(p) = row.preview.clone() {
                                            span { class: "block truncate text-[11px] text-[var(--text-muted)]", "{p}" }
                                        }
                                    }
                                    if row.unread > 0 {
                                        span { class: "shrink-0 min-w-4 h-4 px-1 rounded-full bg-[var(--accent)] text-[var(--bg)] text-[9px] font-bold flex items-center justify-center",
                                            "{row.unread}"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        div { class: "flex-1 min-w-0 flex flex-col",
            if selected.is_some() {
                crate::features::chat::ChatView {}
            } else {
                div { class: "flex-1 flex items-center justify-center p-8",
                    div { class: "max-w-xs text-center space-y-2",
                        div { class: "text-sm text-[var(--text)]", "Nothing open" }
                        div { class: "text-[11px] text-[var(--text-muted)] leading-relaxed",
                            "Direct messages are Nostr events signed by your key: they work with no server at all, and follow you wherever you connect."
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn Chip(label: String, on: bool, onclick: EventHandler<()>) -> Element {
    let cls = if on {
        "border-[var(--border-strong)] bg-[var(--accent-soft)] text-[var(--accent)]"
    } else {
        "border-[var(--border)] text-[var(--text-dim)] hover:text-[var(--text)]"
    };
    rsx! {
        button {
            class: "flex-1 py-1 rounded-md border text-[10px] transition-colors {cls}",
            onclick: move |_| onclick.call(()),
            "{label}"
        }
    }
}

/// One conversation as the column draws it.
#[derive(Clone, PartialEq)]
struct Row {
    info: DmInfo,
    /// Prefixed with "Tú:" when ours — how you tell "they replied" from "I
    /// said the last thing" without opening it.
    preview: Option<String>,
    when: Option<String>,
    unread: u32,
}

impl Row {
    /// An empty box asks for everything; anything else matches the name or the
    /// key, because both are ways people refer to somebody here.
    fn matches(&self, needle: &str) -> bool {
        if needle.is_empty() {
            return true;
        }
        self.info.other.username.to_lowercase().contains(needle)
            || self.info.other.pubkey.to_lowercase().contains(needle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Id, User};

    fn row(name: &str, pubkey: &str) -> Row {
        Row {
            info: DmInfo {
                channel_id: Id::nil(),
                other: User {
                    pubkey: pubkey.into(),
                    username: name.into(),
                },
            },
            preview: None,
            when: None,
            unread: 0,
        }
    }

    /// Typing nothing must not hide the list the field is meant to narrow.
    #[test]
    fn an_empty_search_keeps_everyone() {
        assert!(row("malvina", "ab12").matches(""));
    }

    /// The name is what somebody types when they are looking for a person.
    #[test]
    fn a_name_matches_case_insensitively() {
        assert!(row("Malvina", "ab12").matches("malv"));
        assert!(!row("Malvina", "ab12").matches("jotace"));
    }

    /// Pasting part of a key should find the conversation you already have
    /// with it, rather than looking like there is none.
    #[test]
    fn part_of_a_key_finds_the_conversation() {
        assert!(row("malvina", "ab12cd34").matches("cd34"));
    }
}
