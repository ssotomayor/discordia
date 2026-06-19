use base64::Engine as _;
use dioxus::prelude::*;
use dioxus_grid_layout::NoDrag;

use crate::identity::discriminator;
use crate::protocol::{ClientMessage, Id, Message};
use crate::state::{use_app_state, use_gateway};

/// Curated emoji set for the composer picker. Plain Unicode — they travel as
/// ordinary message text, so no protocol support is needed.
const EMOJIS: &[&str] = &[
    "😀", "😂", "😅", "😍", "😎", "🤔", "😭", "😡", "👍", "👎", "🙏", "🔥", "🎉", "❤️", "💯",
    "✨", "🚀", "👀", "🙌", "😉", "🥳", "😴", "🤯", "🤝", "👋", "💀", "✅", "❌", "⚡", "🌈",
    "🍕", "☕", "🎮", "💸", "🐛", "📎", "🖼️", "🤖", "🫡", "😬",
];

/// Quick reactions offered in the message hover bar.
const QUICK_REACTIONS: &[&str] = &["👍", "❤️", "😂", "🎉", "🔥", "👀", "🙏", "✅"];

/// Reject client-side anything over ~2 MB so we fail fast with a friendly
/// message instead of bouncing off the server's hard cap.
const MAX_IMAGE_BYTES: usize = 2_000_000;

/// Two messages group (compact, no repeated avatar/name) when they're from the
/// same author within this many seconds.
const GROUP_WINDOW_SECS: i64 = 300;

#[component]
pub fn ChatView() -> Element {
    let state = use_app_state();

    let snapshot = state.read();
    let selected_channel = snapshot.selected_channel;
    let dm = selected_channel.and_then(|cid| snapshot.dm_of(cid).cloned());
    let channel_meta = selected_channel
        .and_then(|cid| snapshot.channels.iter().find(|c| c.id == cid).cloned());
    let messages: Vec<Message> = selected_channel
        .and_then(|cid| snapshot.messages.get(&cid).cloned())
        .unwrap_or_default();
    let typers = selected_channel.map(|cid| snapshot.typers_in(cid)).unwrap_or_default();
    drop(snapshot);

    // Header + composer labelling differ for DMs ("@user") vs channels ("#name").
    let (is_dm, header_name, composer_label) = match &dm {
        Some(d) => (true, d.other.username.clone(), format!("@{}", d.other.username)),
        None => {
            let name = channel_meta
                .as_ref()
                .map(|c| c.name.clone())
                .unwrap_or_else(|| "no-channel".into());
            let label = format!("#{name}");
            (false, name, label)
        }
    };
    let channel_topic = channel_meta.as_ref().and_then(|c| c.topic.clone());
    let typing_label = typing_label(&typers);

    rsx! {
        div { class: "flex flex-col h-full min-h-0",
            header { class: "h-11 px-3 flex items-center gap-3 border-b border-[var(--border)] shrink-0",
                span { class: "text-[var(--text-dim)] font-medium", if is_dm { "@" } else { "#" } }
                span { class: "text-sm text-[var(--accent)] font-medium", "{header_name}" }
                if let Some(topic) = channel_topic {
                    span { class: "text-[var(--text-dim)]", "·" }
                    span { class: "text-xs text-[var(--text-muted)] truncate", "{topic}" }
                }
            }

            NoDrag {
                div { class: "flex-1 overflow-y-auto px-4 py-4 min-h-0",
                    if messages.is_empty() && selected_channel.is_some() {
                        div { class: "h-full flex items-center justify-center text-[var(--text-dim)] text-xs",
                            if is_dm { "No messages yet. Say hi 👋" } else { "No messages yet." }
                        }
                    } else {
                        for (i, msg) in messages.iter().enumerate() {
                            {
                                let grouped = i > 0 && groups_with(&messages[i - 1], msg);
                                rsx! { MessageRow { key: "{msg.id}", message: msg.clone(), grouped } }
                            }
                        }
                    }
                }

                // Typing indicator.
                if let Some(label) = typing_label {
                    div { class: "px-4 pb-1 h-4 text-[11px] text-[var(--text-dim)] italic dxf-fade",
                        "{label}"
                    }
                }

                if let Some(channel_id) = selected_channel {
                    Composer { channel_id, composer_label }
                }
            }
        }
    }
}

/// Whether `cur` should render compactly under `prev` (same author, close in time).
fn groups_with(prev: &Message, cur: &Message) -> bool {
    prev.author.pubkey == cur.author.pubkey
        && (cur.created_at - prev.created_at).num_seconds().abs() < GROUP_WINDOW_SECS
}

fn typing_label(typers: &[String]) -> Option<String> {
    match typers.len() {
        0 => None,
        1 => Some(format!("{} is typing…", typers[0])),
        2 => Some(format!("{} and {} are typing…", typers[0], typers[1])),
        _ => Some("several people are typing…".to_string()),
    }
}

#[component]
fn MessageRow(message: Message, grouped: bool) -> Element {
    let mut state = use_app_state();
    let gateway = use_gateway();
    let mut show_react = use_signal(|| false);

    let self_pubkey = state.read().self_user.as_ref().map(|u| u.pubkey.clone());
    let timestamp = message.created_at.format("%H:%M").to_string();
    let has_text = !message.content.is_empty();
    let author_pubkey = message.author.pubkey.clone();
    let channel_id = message.channel_id;
    let message_id = message.id;

    rsx! {
        div { class: "group relative flex gap-3 -mx-4 px-4 py-0.5 hover:bg-white/[0.02] dxf-msg-in",

            // Avatar column: real avatar for the first message in a run, an
            // on-hover timestamp for grouped ones.
            if grouped {
                div { class: "w-8 shrink-0 text-[9px] text-[var(--text-dim)] text-right pt-1 opacity-0 group-hover:opacity-100 transition-opacity",
                    "{timestamp}"
                }
            } else {
                div {
                    class: "cursor-pointer mt-0.5",
                    onclick: move |_| state.write().profile_card = Some(author_pubkey.clone()),
                    crate::features::profiles::Avatar {
                        pubkey: message.author.pubkey.clone(),
                        name: message.author.username.clone(),
                        size: "w-8 h-8",
                    }
                }
            }

            div { class: "flex-1 min-w-0",
                if !grouped {
                    div { class: "flex items-baseline gap-2",
                        span {
                            class: "text-sm text-[var(--text)] font-medium",
                            title: "{message.author.pubkey}",
                            "{message.author.username}"
                            span { class: "text-[var(--text-dim)] font-mono text-[10px] ml-0.5 font-normal",
                                "#{discriminator(&message.author.pubkey)}"
                            }
                        }
                        span { class: "text-[10px] text-[var(--text-dim)]", "{timestamp}" }
                    }
                }
                if has_text {
                    div { class: "text-sm text-[var(--text)] break-words whitespace-pre-wrap leading-relaxed",
                        MessageContent { content: message.content.clone() }
                    }
                }
                if let Some(img) = message.image.as_ref() {
                    img {
                        class: "mt-1 rounded-md border border-[var(--border)] max-w-xs max-h-80 object-contain block",
                        src: "{img}",
                        alt: "attachment",
                    }
                }
                // Reaction chips.
                if !message.reactions.is_empty() {
                    div { class: "flex flex-wrap gap-1 mt-1",
                        for r in message.reactions.iter().cloned() {
                            {
                                let mine = self_pubkey.as_deref().map(|pk| r.users.iter().any(|u| u == pk)).unwrap_or(false);
                                let cls = if mine {
                                    "border-[var(--accent)] bg-[var(--accent-soft)] text-[var(--accent)]"
                                } else {
                                    "border-[var(--border)] text-[var(--text-muted)] hover:border-[var(--border-strong)]"
                                };
                                let emoji = r.emoji.clone();
                                let g = gateway.clone();
                                let count = r.users.len();
                                rsx! {
                                    button {
                                        key: "{r.emoji}",
                                        class: "dxf-pop flex items-center gap-1 px-1.5 h-6 rounded-full border text-xs transition-colors {cls}",
                                        onclick: move |_| g.send(ClientMessage::React { channel_id, message_id, emoji: emoji.clone() }),
                                        span { "{r.emoji}" }
                                        span { class: "text-[10px]", "{count}" }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Hover action bar: add a reaction.
            div { class: "absolute -top-2 right-3 opacity-0 group-hover:opacity-100 transition-opacity",
                div { class: "relative",
                    button {
                        class: "w-7 h-7 rounded-md border border-[var(--border)] bg-[var(--panel-solid)] text-[var(--text-muted)] hover:text-[var(--accent)] hover:border-[var(--accent)] text-sm leading-none transition-colors",
                        title: "Add reaction",
                        onclick: move |_| show_react.set(!show_react()),
                        "☺"
                    }
                    if show_react() {
                        div { class: "dxf-pop-in absolute right-0 top-full mt-1 z-30 flex gap-1 p-1 bg-[var(--panel-solid)] border border-[var(--border)] rounded-md shadow-lg",
                            for emoji in QUICK_REACTIONS.iter().copied() {
                                {
                                    let g = gateway.clone();
                                    rsx! {
                                        button {
                                            class: "w-7 h-7 flex items-center justify-center rounded hover:bg-white/[0.06] text-base leading-none",
                                            onclick: move |_| {
                                                g.send(ClientMessage::React { channel_id, message_id, emoji: emoji.to_string() });
                                                show_react.set(false);
                                            },
                                            "{emoji}"
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

/// Renders message text with clickable URLs and highlighted @mentions, while
/// preserving line breaks.
#[component]
fn MessageContent(content: String) -> Element {
    let lines: Vec<&str> = content.split('\n').collect();
    let last = lines.len().saturating_sub(1);
    rsx! {
        for (li, line) in lines.iter().enumerate() {
            {
                let words: Vec<&str> = line.split(' ').collect();
                let lastw = words.len().saturating_sub(1);
                rsx! {
                    for (wi, word) in words.iter().enumerate() {
                        {
                            let w = word.to_string();
                            let trailing = if wi < lastw { " " } else { "" };
                            if is_url(&w) {
                                let href = w.clone();
                                rsx! {
                                    span {
                                        span {
                                            class: "text-[var(--accent)] underline cursor-pointer hover:text-[var(--accent-strong)]",
                                            onclick: move |_| crate::app::open_external(&href),
                                            "{w}"
                                        }
                                        "{trailing}"
                                    }
                                }
                            } else if w.starts_with('@') && w.len() > 1 {
                                rsx! {
                                    span {
                                        span { class: "text-[var(--accent)] bg-[var(--accent-soft)] rounded px-0.5", "{w}" }
                                        "{trailing}"
                                    }
                                }
                            } else {
                                rsx! { span { "{w}{trailing}" } }
                            }
                        }
                    }
                    if li < last {
                        br {}
                    }
                }
            }
        }
    }
}

fn is_url(w: &str) -> bool {
    w.starts_with("http://") || w.starts_with("https://")
}

#[component]
fn Composer(channel_id: Id, composer_label: String) -> Element {
    let mut draft = use_signal(String::new);
    let mut pending_image = use_signal::<Option<String>>(|| None);
    let attach_err = use_signal::<Option<String>>(|| None);
    let mut show_emoji = use_signal(|| false);
    let mut last_typing = use_signal::<Option<std::time::Instant>>(|| None);
    let gateway = use_gateway();
    let gateway_submit = gateway.clone();

    let mut submit = move || {
        let content = draft().trim().to_string();
        let image = pending_image();
        if content.is_empty() && image.is_none() {
            return;
        }
        gateway_submit.send(ClientMessage::SendMessage {
            channel_id,
            content,
            image,
        });
        draft.set(String::new());
        pending_image.set(None);
        show_emoji.set(false);
    };

    // Throttled typing notification (at most once / 2s while editing).
    let gateway_typing = gateway.clone();
    let mut notify_typing = move || {
        let now = std::time::Instant::now();
        let send = match *last_typing.peek() {
            Some(t) => now.duration_since(t).as_secs() >= 2,
            None => true,
        };
        if send {
            last_typing.set(Some(now));
            gateway_typing.send(ClientMessage::Typing { channel_id });
        }
    };

    rsx! {
        div { class: "px-3 pb-3 shrink-0 relative",

            // Emoji picker popover, floats above the input row.
            if show_emoji() {
                div {
                    class: "dxf-pop-in absolute bottom-full left-3 right-3 mb-2 p-2 bg-[var(--panel-solid)] border border-[var(--border)] rounded-md shadow-lg grid grid-cols-10 gap-1 z-30",
                    for emoji in EMOJIS.iter().copied() {
                        button {
                            r#type: "button",
                            class: "w-7 h-7 flex items-center justify-center rounded hover:bg-white/[0.06] text-lg leading-none",
                            onclick: move |_| {
                                let mut d = draft.write();
                                d.push_str(emoji);
                            },
                            "{emoji}"
                        }
                    }
                }
            }

            // Pending attachment preview.
            if let Some(img) = pending_image() {
                div { class: "mb-2 flex items-center gap-2",
                    img {
                        class: "h-16 w-16 object-cover rounded border border-[var(--border)]",
                        src: "{img}",
                        alt: "pending attachment",
                    }
                    button {
                        r#type: "button",
                        class: "text-[10px] uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--danger)] transition-colors",
                        onclick: move |_| pending_image.set(None),
                        "Remove"
                    }
                }
            }
            if let Some(err) = attach_err() {
                div { class: "mb-2 text-[10px] text-[var(--danger)]", "{err}" }
            }

            form {
                onsubmit: move |e| { e.prevent_default(); submit(); },
                div { class: "border border-[var(--border)] rounded flex items-center px-2 gap-1 focus-within:border-[var(--accent)] transition-colors",

                    // Emoji toggle.
                    button {
                        r#type: "button",
                        class: "px-1.5 text-base leading-none text-[var(--text-muted)] hover:text-[var(--accent)] transition-colors",
                        title: "Emoji",
                        onclick: move |_| show_emoji.set(!show_emoji()),
                        "🙂"
                    }

                    // Image attach (label opens the hidden file input).
                    label {
                        class: "px-1.5 text-base leading-none text-[var(--text-muted)] hover:text-[var(--accent)] transition-colors cursor-pointer",
                        title: "Attach image",
                        "🖼"
                        input {
                            r#type: "file",
                            accept: "image/*",
                            class: "hidden",
                            onchange: move |evt: FormEvent| {
                                let files = evt.files();
                                let mut pending = pending_image;
                                let mut err = attach_err;
                                spawn(async move {
                                    let Some(file) = files.into_iter().next() else { return };
                                    match file.read_bytes().await {
                                        Ok(bytes) => {
                                            if bytes.len() > MAX_IMAGE_BYTES {
                                                err.set(Some("Image too large (max 2 MB).".into()));
                                                return;
                                            }
                                            let mime = file
                                                .content_type()
                                                .filter(|m| m.starts_with("image/"))
                                                .unwrap_or_else(|| "image/png".to_string());
                                            let b64 = base64::engine::general_purpose::STANDARD
                                                .encode(&bytes);
                                            err.set(None);
                                            pending.set(Some(format!("data:{mime};base64,{b64}")));
                                        }
                                        Err(_) => err.set(Some("Couldn't read that file.".into())),
                                    }
                                });
                            },
                        }
                    }

                    input {
                        class: "flex-1 bg-transparent py-2 text-sm text-[var(--text)] focus:outline-none",
                        r#type: "text",
                        placeholder: "Message {composer_label}",
                        value: "{draft}",
                        oninput: move |e| { draft.set(e.value()); notify_typing(); },
                    }
                    button {
                        class: "text-xs text-[var(--text-muted)] hover:text-[var(--accent)] font-medium uppercase tracking-wider px-2 disabled:opacity-30 transition-colors",
                        r#type: "submit",
                        disabled: draft().trim().is_empty() && pending_image().is_none(),
                        "Send"
                    }
                }
            }
        }
    }
}
