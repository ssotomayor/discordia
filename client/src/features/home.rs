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
    let settings = use_context::<Signal<crate::settings::ClientSettings>>();
    // Seeded at creation, not after: `spawn_nostr` starts replaying history on
    // the next poll, and an empty watermark map lets the cleared ones back in.
    let state = use_signal(|| {
        let mut s = AppState::empty();
        s.dm_cleared_at = settings.read().dm_cleared_at.iter().cloned().collect();
        s.dm_read_at = settings.read().dm_read_at.iter().cloned().collect();
        s
    });
    crate::state::use_dm_read_persistence(state);

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

    let mut social = use_signal(|| false);
    use_effect(move || {
        let mut app = state;
        app.write().dm_pane_open = social();
    });

    let mac_top_pad = if cfg!(target_os = "macos") {
        "pt-5"
    } else {
        ""
    };
    // Width, not transform: the panel has to take its room from the form rather
    // than cover it. Always mounted, so it has something to open from.
    //
    // 40% is the rule and 560px is the floor, because under ~1400 the rule
    // stops leaving a conversation worth opening: 40% of 1024 is 410, and the
    // list alone is 320. The floor wins there and the drawer runs wide.
    let drawer = if social() {
        "w-[max(560px,40vw)]"
    } else {
        "w-0"
    };

    rsx! {
        div { class: "h-full w-full flex flex-col bg-[var(--bg)] {mac_top_pad}",
            crate::features::chat::ImageViewer {}
            crate::features::profiles::ProfileCard {}
            TopBar { identity: identity.clone(), social, on_rename, on_sign_out }
            div { class: "flex-1 flex min-h-0",
                // Takes what the drawer leaves and centres its content: pinned
                // left, a wide window reads as a layout that lost a column.
                div { class: "flex-1 min-w-0 flex flex-col",
                    div { class: "w-full max-w-[588px] mx-auto flex-1 min-h-0 flex flex-col",
                        div { class: "px-7 pt-5 shrink-0",
                            h2 { class: "dxf-display text-xl font-semibold tracking-tight text-[var(--text)]",
                                "Go to a server"
                            }
                            p { class: "mt-1.5 text-[13px] text-[var(--text-muted)] text-pretty",
                                "Reconnect to your last one, or join with a code from a friend."
                            }
                        }
                        crate::features::connect::ConnectForm {
                            identity: identity.clone(),
                            error,
                            last_session,
                            on_connect,
                        }
                    }
                }
                // The conversation belongs to the drawer, not to the screen:
                // with nowhere to go it was a permanent empty state next to a
                // form that has nothing to do with it.
                div { class: "shrink-0 overflow-hidden transition-[width] duration-200 ease-out {drawer}",
                    // Held at the open width so the inside does not reflow while
                    // the outside is still narrowing — the clip is the movement.
                    div { class: "h-full flex w-[max(560px,40vw)] border-l border-[var(--edge)]",
                        // Full height on purpose: the whole edge is the target,
                        // so closing never asks anyone to aim.
                        button {
                            class: "w-4 shrink-0 h-full flex items-center justify-center border-r border-[var(--edge)] bg-[var(--panel)] text-[10px] text-[var(--text-dim)] hover:bg-[var(--bg2)] hover:text-[var(--accent)] transition-colors",
                            title: "Collapse the social panel",
                            onclick: move |_| social.set(false),
                            "❯"
                        }
                        // 320 when there is room, down to 240 when there is
                        // not: inside a capped drawer the list is the half
                        // that still reads narrow, so it is the half that gives.
                        div { class: "w-[320px] min-w-[240px] shrink flex flex-col border-r border-[var(--edge)] bg-[var(--panel)]",
                            SocialPanel { on_close: move |_| social.set(false) }
                        }
                        TalkPane {}
                    }
                }
            }
        }
    }
}

/// Brand on the left, who you are on the right, and the relay line between
/// them — the one reading that says the talking half is actually working.
#[component]
fn TopBar(
    identity: Identity,
    social: Signal<bool>,
    on_rename: EventHandler<String>,
    on_sign_out: EventHandler<()>,
) -> Element {
    let mut social = social;
    let state = use_app_state();
    let settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let configured: Vec<String> = {
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
    let up = state.read().nostr_relays_up.clone();
    let relays = up.len();
    let total = configured.len();
    // The count alone hid the interesting half: three of four is one relay
    // down, and which one is the part `nostr_relays_up` keeps a set for.
    let relay_detail = {
        let mut lines: Vec<String> = configured
            .iter()
            .map(|r| {
                let host = r.trim_start_matches("wss://").trim_start_matches("ws://");
                if up.contains(r) {
                    format!("· {host} — connected")
                } else {
                    format!("· {host} — not connected")
                }
            })
            .collect();
        lines.insert(
            0,
            "Direct messages travel over these Nostr relays. One is enough to send and receive."
                .to_string(),
        );
        lines.join("\n")
    };
    let relay_label = if relays == 0 {
        "No relays connected".to_string()
    } else {
        format!("{relays} of {total} relays connected")
    };
    // Two labels rather than one that truncates: "3 of 4 relays conn…" says
    // less than "3/4" and takes more room to say it.
    let relay_short = format!("{relays}/{total}");
    let dot = if relays == 0 {
        "var(--danger)"
    } else if relays < total {
        "var(--warn)"
    } else {
        "var(--up)"
    };
    let unread = state.read().dm_unread_total();
    let social_cls = if social() {
        "border-[var(--accent)] text-[var(--accent)]"
    } else {
        "border-[var(--border-strong)] text-[var(--text-muted)] hover:text-[var(--text)] hover:border-[var(--accent)]"
    };
    let mut open = use_signal(|| false);
    let npub = identity.npub();
    let short = format!("{}…{}", &npub[..9.min(npub.len())], &npub[npub.len() - 3..]);

    rsx! {
        div { class: "relative shrink-0 z-50",
            div {
                class: "dxf-drag-region h-[58px] px-5 flex items-center gap-3.5 bg-[var(--panel)] border-b border-[var(--edge)]",
                onmousedown: move |_| crate::app::start_window_drag(),
                // Everything here is `shrink-0`, so a narrow window cannot
                // squeeze the bar — it drops the least load-bearing parts by
                // width instead, and Account never falls off the end.
                div { class: "flex items-center gap-2.5 shrink-0",
                    crate::app::DiscordiaLogo { class: "w-6 h-6 shrink-0" }
                    span { class: "dxf-display dxf-wordmark text-[17px] font-semibold tracking-tight hidden min-[900px]:inline",
                        "Discordia"
                    }
                }
                span {
                    class: "flex items-center gap-2 shrink-0 pl-2.5 pr-3 py-1.5 rounded-full border",
                    style: "background: color-mix(in srgb, var(--accent) 10%, transparent); border-color: color-mix(in srgb, var(--accent) 28%, transparent);",
                    title: "{relay_detail}",
                    span { class: "w-[7px] h-[7px] rounded-full shrink-0", style: "background: {dot};" }
                    span {
                        class: "text-xs whitespace-nowrap hidden min-[1180px]:inline",
                        style: "color: color-mix(in srgb, var(--accent) 45%, var(--text));",
                        "{relay_label}"
                    }
                    span {
                        class: "text-xs whitespace-nowrap min-[1180px]:hidden",
                        style: "color: color-mix(in srgb, var(--accent) 45%, var(--text));",
                        "{relay_short}"
                    }
                }
                div { class: "flex-1" }
                div { class: "dxf-no-drag flex items-center gap-3 shrink-0",
                    onmousedown: move |e| e.stop_propagation(),
                    button {
                        class: "flex items-center gap-2 px-3.5 py-1.5 rounded-lg border text-[13px] transition-colors {social_cls}",
                        onclick: move |_| {
                            open.set(false);
                            let now = social();
                            social.set(!now);
                        },
                        span {
                            class: "shrink-0 flex items-center",
                            dangerous_inner_html: crate::features::icons::USERS,
                        }
                        "Social"
                        if unread > 0 {
                            span { class: "text-[var(--accent)] font-semibold", "{unread}" }
                        }
                    }
                    crate::features::profiles::Avatar {
                        pubkey: identity.pubkey.clone(),
                        name: identity.display_name.clone(),
                        size: "w-8 h-8",
                        text: "text-xs",
                    }
                    div { class: "min-w-0 max-w-40 leading-tight hidden min-[1060px]:block",
                        div { class: "text-sm font-semibold text-[var(--text)] truncate",
                            "{identity.display_name}"
                        }
                        div { class: "text-[10.5px] font-mono tracking-wide text-[var(--text-dim)] truncate",
                            "{short}"
                        }
                    }
                    button {
                        class: "px-3.5 py-1.5 rounded-lg border border-[var(--border-strong)] text-[13px] text-[var(--text-muted)] hover:text-[var(--text)] hover:border-[var(--accent)] transition-colors",
                        onclick: move |_| {
                            social.set(false);
                            let now = open();
                            open.set(!now);
                        },
                        "Account"
                    }
                }
            }
            if open() {
                div { class: "absolute right-5 top-[58px] z-50 w-80",
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

/// Conversations and the people in them — the half that needs no server, kept
/// out of the way until it is asked for.
#[component]
fn SocialPanel(on_close: EventHandler<()>) -> Element {
    let mut state = use_app_state();
    let mut settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let nostr = use_context::<crate::nostr::service::NostrTx>();
    let mut filter = use_signal(String::new);
    let mut showing_friends = use_signal(|| false);
    // Deleting is not undoable, so the ✕ only arms; the second click acts.
    let mut confirming = use_signal(|| None::<crate::protocol::Id>);

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
                name: snapshot.display_name(&dm.other_pubkey),
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
            let n = snapshot.display_name(&c.pubkey);
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
        div { class: "px-[18px] pt-5 pb-3.5 shrink-0",
            div { class: "flex items-center gap-2",
                h2 { class: "dxf-display flex-1 text-[17px] font-semibold tracking-tight text-[var(--text)]",
                    "Social"
                }
                button {
                    class: "w-6 h-6 shrink-0 rounded-md flex items-center justify-center text-[13px] text-[var(--text-dim)] hover:text-[var(--text)] hover:bg-[var(--bg2)] transition-colors",
                    title: "Close",
                    onclick: move |_| on_close.call(()),
                    "✕"
                }
            }
            div { class: "mt-3 relative",
                span { class: "absolute left-3 top-1/2 -translate-y-1/2 text-[13px] text-[var(--text-dim)] pointer-events-none",
                    "⌕"
                }
                input {
                    class: "w-full bg-[var(--panel2)] border border-[var(--border)] focus:border-[var(--border-strong)] rounded-[10px] pl-[30px] pr-3 py-2.5 text-[13px] text-[var(--text)] outline-none transition-colors",
                    r#type: "text",
                    placeholder: "Search, or paste an npub…",
                    value: "{filter}",
                    oninput: move |e| {
                        confirming.set(None);
                        filter.set(e.value());
                    },
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
            }
            div { class: "mt-2.5 flex gap-1 p-[3px] rounded-[10px] bg-[var(--panel2)] border border-[var(--edge)]",
                Chip {
                    label: "Direct".to_string(),
                    on: !showing_friends(),
                    onclick: move |_| {
                        confirming.set(None);
                        showing_friends.set(false);
                    },
                }
                Chip {
                    label: "Friends".to_string(),
                    on: showing_friends(),
                    onclick: move |_| {
                        confirming.set(None);
                        showing_friends.set(true);
                    },
                }
            }
        }

        div { class: "flex-1 overflow-y-auto px-2.5 pb-4 space-y-0.5 min-h-0",
            if showing_friends() {
                if contacts.is_empty() {
                    div { class: "px-2.5 py-3 text-[12.5px] text-[var(--text-dim)] leading-relaxed",
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
                                class: "w-full flex items-center gap-2.5 px-2.5 py-2.5 rounded-[10px] text-left hover:bg-[var(--bg2)] transition-colors",
                                onclick: move |_| nostr.send(
                                    crate::nostr::service::NostrCmd::Open { peer: peer.clone() },
                                ),
                                crate::features::profiles::Avatar {
                                    pubkey: pk.clone(),
                                    name: name.clone(),
                                    size: "w-[34px] h-[34px]",
                                    text: "text-xs",
                                }
                                span { class: "flex-1 min-w-0 truncate text-[12.5px] text-[var(--text-muted)]", "{name}" }
                            }
                        }
                    }
                }
            } else {
                if shown.is_empty() {
                    div { class: "px-2.5 py-3 text-[12.5px] text-[var(--text-dim)] leading-relaxed text-pretty",
                        "No conversations yet. Paste someone's npub above — no server needed."
                    }
                }
                for row in shown.iter().cloned() {
                    {
                        let cid = row.info.channel_id;
                        let peer = row.info.other_pubkey.clone();
                        let active = selected == Some(cid);
                        let asking = confirming() == Some(cid);
                        let cls = if active {
                            "bg-[var(--panel2)]"
                        } else {
                            "hover:bg-[var(--bg2)]"
                        };
                        let del_cls = if asking {
                            "px-2 h-5 rounded-full border border-[var(--danger)] text-[10px] font-bold uppercase tracking-wide text-[var(--danger)]"
                        } else {
                            "w-5 h-5 rounded-full hidden group-hover:flex text-[11px] text-[var(--text-dim)] hover:text-[var(--danger)]"
                        };
                        let uname = row.name.clone();
                        rsx! {
                            div {
                                key: "{cid}",
                                class: "group w-full flex items-center gap-2.5 px-2.5 py-2.5 rounded-[10px] transition-colors {cls}",
                                button {
                                    class: "flex-1 min-w-0 flex items-center gap-2.5 text-left",
                                    onclick: move |_| {
                                        confirming.set(None);
                                        let mut s = state.write();
                                        s.dm_mode = true;
                                        s.selected_channel = Some(cid);
                                        s.mark_dm_read(cid);
                                    },
                                    span { class: "relative shrink-0",
                                        crate::features::profiles::Avatar {
                                            pubkey: row.info.other_pubkey.clone(),
                                            name: uname.clone(),
                                            size: "w-[34px] h-[34px]",
                                            text: "text-xs",
                                        }
                                    }
                                    span { class: "flex-1 min-w-0",
                                        span { class: "flex items-baseline gap-2",
                                            span { class: "truncate text-[12.5px] font-medium text-[var(--text)]", "{uname}" }
                                            span { class: "flex-1" }
                                            if let Some(w) = row.when.clone() {
                                                span { class: "shrink-0 text-[11px] text-[var(--text-dim)]", "{w}" }
                                            }
                                        }
                                        if let Some(p) = row.preview.clone() {
                                            span { class: "block truncate mt-0.5 text-[12.5px] text-[var(--text-muted)]", "{p}" }
                                        }
                                    }
                                }
                                if row.unread > 0 && !asking {
                                    span { class: "shrink-0 min-w-5 h-5 px-1.5 rounded-full bg-[var(--accent)] text-[#1a1206] text-[11.5px] font-bold flex items-center justify-center group-hover:hidden",
                                        "{row.unread}"
                                    }
                                }
                                button {
                                    class: "shrink-0 items-center justify-center transition-colors {del_cls}",
                                    title: "Delete this conversation on this machine. The relays keep their copy, and a new message reopens it.",
                                    onclick: move |_| {
                                        if !asking {
                                            confirming.set(Some(cid));
                                            return;
                                        }
                                        let at = chrono::Utc::now().timestamp();
                                        state.write().clear_dm(&peer, at);
                                        let mut next = settings.read().clone();
                                        next.clear_dm(&peer, at);
                                        settings.set(next.clone());
                                        crate::settings::save(&next);
                                        confirming.set(None);
                                    },
                                    if asking { "Delete?" } else { "✕" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The conversation itself, which is what the space freed by the panel is for.
#[component]
fn TalkPane() -> Element {
    let state = use_app_state();
    let snapshot = state.read();
    let selected = snapshot.selected_channel;
    let any_dms = !snapshot.dms.is_empty();
    drop(snapshot);

    // `app.rs` pins the version label at `bottom-3 right-3`, which lands on the
    // composer's Send button; this is the room it needs.
    let chat_pad = if selected.is_some() { "pb-7" } else { "" };

    rsx! {
        // The floor that makes the list yield: without it flex hands the list
        // its full 320 and the conversation takes whatever is left, however
        // little. 240 + 300 + the strip is what the drawer's own floor allows.
        div { class: "flex-1 min-w-[300px] flex flex-col {chat_pad}",
            if selected.is_some() {
                crate::features::chat::ChatView {}
            } else {
                div { class: "flex-1 flex flex-col items-center justify-center p-10",
                    div {
                        class: "w-14 h-14 rounded-2xl border border-[var(--edge-strong)] flex items-center justify-center",
                        style: "background: linear-gradient(150deg, color-mix(in srgb, var(--accent) 12%, var(--panel2)), var(--panel2));",
                        crate::app::DiscordiaLogo { class: "w-[22px] h-[22px]" }
                    }
                    if any_dms {
                        div { class: "mt-5 dxf-display text-xl font-semibold tracking-tight text-[var(--text)]",
                            "Pick a conversation"
                        }
                        div { class: "mt-2 max-w-[400px] text-center text-[13.5px] text-[var(--text-muted)] leading-relaxed text-pretty",
                            "Yours are on the left. They travel with your key, so they will still be
                             here whichever server you go to next."
                        }
                    } else {
                        div { class: "mt-5 dxf-display text-xl font-semibold tracking-tight text-[var(--text)]",
                            "Your messages, before any server"
                        }
                        div { class: "mt-2 max-w-[400px] text-center text-[13.5px] text-[var(--text-muted)] leading-relaxed text-pretty",
                            "They are signed by your key and carried by relays, so they belong to you
                             rather than to whoever you are connected to. Paste someone's npub on the
                             left to start one."
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
        "bg-[var(--accent-soft)] text-[var(--accent-strong)]"
    } else {
        "text-[var(--text-dim)] hover:text-[var(--text-muted)]"
    };
    rsx! {
        button {
            class: "flex-1 py-1.5 rounded-[7px] text-[12.5px] font-semibold transition-colors {cls}",
            onclick: move |_| onclick.call(()),
            "{label}"
        }
    }
}

/// One conversation as the panel draws it.
#[derive(Clone, PartialEq)]
struct Row {
    info: DmInfo,
    /// Resolved at build time from `display_name`, never stored on `DmInfo`:
    /// every source of a name arrives after the row is first drawn.
    name: String,
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
        self.name.to_lowercase().contains(needle)
            || self.info.other_pubkey.to_lowercase().contains(needle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Id;

    fn row(name: &str, pubkey: &str) -> Row {
        Row {
            info: DmInfo {
                channel_id: Id::nil(),
                other_pubkey: pubkey.into(),
            },
            name: name.into(),
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
