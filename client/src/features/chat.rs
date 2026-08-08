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

/// Keep the message list pinned to the newest message, but only while the
/// reader is already at the bottom.
///
/// The "stick" flag has to come from a scroll listener rather than being
/// computed here: by the time this runs the new message is already in the DOM,
/// so the distance to the bottom no longer says anything about where the reader
/// was standing. Appending content doesn't fire a scroll event, so the flag
/// still holds its pre-update value — which is exactly what we need.
///
/// `mode` is one of:
/// - `channel` — switched channels, always jump to the newest message
/// - `prepend` — older history loaded above the viewport, hold position
/// - `append`  — new message arrived, follow it only if anchored
fn chat_scroll_js(mode: &str) -> String {
    format!(
        r#"
(function() {{
  var el = document.getElementById('dxf-chat-scroll');
  if (!el) return;
  if (!el._dxfWired) {{
    el._dxfWired = true;
    el._dxfStick = true;
    el._dxfPrevHeight = el.scrollHeight;
    el.addEventListener('scroll', function() {{
      // A few pixels of tolerance, so sub-pixel rounding and "near enough to
      // the bottom" still count as anchored.
      var gap = el.scrollHeight - el.scrollTop - el.clientHeight;
      el._dxfStick = gap <= 40;
    }}, {{ passive: true }});
  }}
  var mode = '{mode}';
  if (mode === 'channel') {{
    el.scrollTop = el.scrollHeight;
    el._dxfStick = true;
  }} else if (mode === 'prepend') {{
    // Older messages were inserted above the viewport. Shift by exactly how
    // much taller the content got, so what the reader was looking at stays put
    // instead of sliding down the screen.
    var grew = el.scrollHeight - el._dxfPrevHeight;
    if (grew > 0) {{ el.scrollTop = el.scrollTop + grew; }}
  }} else if (el._dxfStick) {{
    // Assigning scrollTop is instantaneous; a smooth animation here would
    // fight the reader if they start scrolling mid-flight.
    el.scrollTop = el.scrollHeight;
  }}
  el._dxfPrevHeight = el.scrollHeight;
}})();
"#
    )
}

#[component]
pub fn ChatView() -> Element {
    let state = use_app_state();
    let gateway = use_gateway();

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

    // Auto-scroll. The key is deliberately narrow — the channel plus the ids at
    // each end — so edits and reactions on existing messages don't yank the
    // view around. Comparing the *first* id is what distinguishes "older
    // history was paged in above" from "a new message arrived below".
    let scroll_key = use_memo(move || {
        let s = state.read();
        let cid = s.selected_channel;
        let msgs = cid.and_then(|c| s.messages.get(&c));
        (
            cid,
            msgs.and_then(|m| m.first().map(|x| x.id)),
            msgs.and_then(|m| m.last().map(|x| x.id)),
        )
    });
    let mut prev_key = use_signal(|| (None::<Id>, None::<Id>));
    use_effect(move || {
        let (cid, first, _last) = scroll_key();
        // peek, not read: this effect writes prev_key below, and subscribing to
        // it here would re-trigger the effect forever.
        let (prev_cid, prev_first) = *prev_key.peek();
        let channel_changed = cid != prev_cid;
        let prepended = !channel_changed && prev_first.is_some() && first != prev_first;
        prev_key.set((cid, first));

        let mode = if channel_changed {
            "channel"
        } else if prepended {
            "prepend"
        } else {
            "append"
        };
        let _ = document::eval(&chat_scroll_js(mode));
    });

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
                div { id: "dxf-chat-scroll", class: "flex-1 overflow-y-auto px-4 py-4 min-h-0",
                    if messages.is_empty() && selected_channel.is_some() {
                        div { class: "h-full flex items-center justify-center text-[var(--text-dim)] text-xs",
                            if is_dm { "No messages yet. Say hi 👋" } else { "No messages yet." }
                        }
                    } else {
                        // Page back through older history. Shown when the loaded
                        // set is at least a full page (likely more behind it);
                        // fetches the slice before the oldest message we hold,
                        // which the net layer merges in chronological order.
                        if messages.len() >= PAGE_SIZE {
                            if let (Some(channel_id), Some(oldest)) =
                                (selected_channel, messages.first())
                            {
                                {
                                    let before_ms = oldest.created_at.timestamp_millis();
                                    let gw = gateway.clone();
                                    rsx! {
                                        div { class: "flex justify-center pb-3",
                                            button {
                                                class: "text-[11px] uppercase tracking-wider text-[var(--text-muted)] border border-[var(--border)] rounded px-3 py-1 hover:text-[var(--accent)] hover:border-[var(--accent)] transition-colors",
                                                onclick: move |_| {
                                                    gw.send(ClientMessage::FetchMessages {
                                                        channel_id,
                                                        limit: PAGE_SIZE as u32,
                                                        before_ms: Some(before_ms),
                                                    });
                                                },
                                                "Load earlier messages"
                                            }
                                        }
                                    }
                                }
                            }
                        }
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

/// The initial history page size (mirrors the server default). When a channel
/// holds at least this many messages there is probably older history to page
/// back through, so we surface the "load earlier" affordance.
const PAGE_SIZE: usize = 50;

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
    let mut confirm_delete = use_signal(|| false);

    let self_pubkey = state.read().self_user.as_ref().map(|u| u.pubkey.clone());
    let timestamp = message.created_at.format("%H:%M").to_string();
    let has_text = !message.content.is_empty();
    let author_pubkey = message.author.pubkey.clone();
    let channel_id = message.channel_id;
    let message_id = message.id;

    // Guild that owns this channel. None for DMs — which is also why custom
    // emoji don't resolve there: they belong to a guild.
    let guild_id = state
        .read()
        .channels
        .iter()
        .find(|c| c.id == channel_id)
        .map(|c| c.guild_id);

    // Delete affordance: your own messages always; others' need ManageMessages
    // in the channel's guild (DMs: author-only). Server re-checks.
    let can_delete = {
        let s = state.read();
        let is_author = self_pubkey.as_deref() == Some(message.author.pubkey.as_str());
        is_author
            || guild_id
                .map(|gid| s.can(gid, crate::protocol::Permission::ManageMessages))
                .unwrap_or(false)
    };

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
                            class: "text-sm font-semibold",
                            style: "color: {crate::identity::signature_accent(&message.author.pubkey)};",
                            title: "{message.author.pubkey}",
                            "{message.author.username}"
                            span { class: "text-[var(--text-dim)] font-mono text-[10px] ml-0.5 font-normal",
                                "#{discriminator(&message.author.pubkey)}"
                            }
                        }
                        span { class: "text-[var(--up)] text-[10px]", title: "Key verified", "✓" }
                        span { class: "text-[10px] text-[var(--text-dim)]", "{timestamp}" }
                    }
                }
                if has_text {
                    div { class: "text-sm text-[var(--text)] break-words whitespace-pre-wrap leading-relaxed",
                        MessageContent { content: message.content.clone(), channel_id }
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
                                        // `Reaction.emoji` is just a string, so a
                                        // custom emoji rides in it as `:shortcode:`
                                        // and needs the same resolution as message
                                        // text — otherwise reacting with a guild
                                        // emoji shows the raw code in the chip.
                                        span { EmojiText { text: r.emoji.clone(), guild_id } }
                                        span { class: "text-[10px]", "{count}" }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Hover action bar: add a reaction / delete.
            div { class: "absolute -top-2 right-3 opacity-0 group-hover:opacity-100 transition-opacity",
                div { class: "relative flex gap-1",
                    button {
                        class: "w-7 h-7 rounded-md border border-[var(--border)] bg-[var(--panel-solid)] text-[var(--text-muted)] hover:text-[var(--accent)] hover:border-[var(--accent)] text-sm leading-none transition-colors",
                        title: "Add reaction",
                        onclick: move |_| show_react.set(!show_react()),
                        "☺"
                    }
                    if can_delete {
                        button {
                            class: "w-7 h-7 rounded-md border border-[var(--border)] bg-[var(--panel-solid)] text-[var(--text-muted)] hover:text-[var(--danger)] hover:border-[var(--danger)] text-sm leading-none transition-colors",
                            title: "Delete message",
                            onclick: move |_| confirm_delete.set(!confirm_delete()),
                            "🗑"
                        }
                    }
                    if confirm_delete() {
                        div { class: "dxf-pop-in absolute right-0 top-full mt-1 z-30 flex items-center gap-1 p-1 bg-[var(--panel-solid)] border border-[var(--border)] rounded-md shadow-lg",
                            span { class: "text-[10px] text-[var(--text-muted)] px-1", "Delete?" }
                            button {
                                class: "px-2 h-6 rounded text-[10px] uppercase tracking-wider text-[var(--danger)] border border-[var(--danger)]/40 hover:bg-[var(--danger)]/10 transition-colors",
                                onclick: {
                                    let g = gateway.clone();
                                    move |_| {
                                        g.send(ClientMessage::DeleteMessage { channel_id, message_id });
                                        confirm_delete.set(false);
                                    }
                                },
                                "Yes"
                            }
                            button {
                                class: "px-2 h-6 rounded text-[10px] uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--text-muted)] transition-colors",
                                onclick: move |_| confirm_delete.set(false),
                                "No"
                            }
                        }
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
/// Render one word that contains at least one `:shortcode:`, swapping in the
/// guild's custom emoji images.
///
/// A shortcode with no matching emoji — or one whose bytes haven't arrived yet
/// — renders as the literal `:shortcode:`. That's deliberate: it degrades to
/// exactly what a client without the emoji sees, so a slow fetch looks like
/// plain text rather than a hole in the message.
#[component]
fn EmojiText(text: String, guild_id: Option<Id>) -> Element {
    let state = use_app_state();
    // Resolve to owned data up front so the state borrow ends before rendering.
    let parts: Vec<(String, Option<String>)> = {
        let s = state.read();
        crate::emoji::split_shortcodes(&text)
            .into_iter()
            .map(|p| match p {
                crate::emoji::Piece::Text(t) => (t.to_string(), None),
                crate::emoji::Piece::Shortcode(code) => {
                    let url = guild_id
                        .and_then(|g| s.emoji_image(g, code))
                        .map(str::to_string);
                    (code.to_string(), Some(url.unwrap_or_default()))
                }
            })
            .collect()
    };

    rsx! {
        for (body, emoji) in parts.into_iter() {
            match emoji {
                // Sized in `em` so emoji track the surrounding text rather than
                // a fixed pixel height, and nudged down to sit on the baseline.
                Some(url) if !url.is_empty() => rsx! {
                    img {
                        src: "{url}",
                        alt: ":{body}:",
                        title: ":{body}:",
                        style: "height:1.4em;width:auto;display:inline-block;vertical-align:-0.3em;",
                    }
                },
                Some(_) => rsx! { ":{body}:" },
                None => rsx! { "{body}" },
            }
        }
    }
}

#[component]
fn MessageContent(content: String, channel_id: Id) -> Element {
    let state = use_app_state();
    // Custom emoji are guild-scoped, so a DM (no guild) simply renders the
    // literal `:shortcode:` — which is also what a client that doesn't have
    // the emoji shows, so nothing is lost.
    let guild_id = state
        .read()
        .channels
        .iter()
        .find(|c| c.id == channel_id)
        .map(|c| c.guild_id);
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
                            } else if crate::emoji::has_shortcode(&w) {
                                rsx! {
                                    span {
                                        EmojiText { text: w.clone(), guild_id }
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

    // The guild's custom emoji, for the picker's first section. Snapshotted
    // (rather than read inside the RSX) so the state borrow doesn't span the
    // closures below.
    let (guild_emojis, emoji_urls) = {
        let state = use_app_state();
        let s = state.read();
        let gid = s.channels.iter().find(|c| c.id == channel_id).map(|c| c.guild_id);
        let list = gid.map(|g| s.emojis_of(g).to_vec()).unwrap_or_default();
        let urls: std::collections::HashMap<String, String> = list
            .iter()
            .filter_map(|e| s.emoji_images.get(&e.image).map(|u| (e.image.clone(), u.clone())))
            .collect();
        (list, urls)
    };

    // Read-only channels: swap the composer for a lock notice unless the user
    // holds ManageMessages/ManageChannels there (mirrors the server gate).
    let locked = {
        let state = use_app_state();
        let s = state.read();
        s.channels
            .iter()
            .find(|c| c.id == channel_id)
            .filter(|c| c.read_only)
            .map(|c| {
                !(s.can(c.guild_id, crate::protocol::Permission::ManageMessages)
                    || s.can(c.guild_id, crate::protocol::Permission::ManageChannels))
            })
            .unwrap_or(false)
    };
    if locked {
        return rsx! {
            div { class: "px-3 pb-3 shrink-0",
                div { class: "flex items-center gap-2 border border-[var(--border)] rounded-lg px-3 py-2 text-xs text-[var(--text-dim)]",
                    span { "🔒" }
                    span { "This channel is read-only." }
                }
            }
        };
    }

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

            // Emoji picker popover, floats above the input row. Anchored to the
            // right so it sits under the button that opens it, and sized to its
            // content instead of stretching the full composer width.
            if show_emoji() {
                div {
                    class: "dxf-pop-in absolute bottom-full right-3 mb-2 p-1.5 bg-[var(--panel-solid)] border border-[var(--border)] rounded-md shadow-lg z-30",
                    // The guild's own emoji come first — they're the ones you
                    // can't type any other way. Inserted as `:shortcode:`, so
                    // the draft stays plain text and needs no special casing on
                    // send.
                    if !guild_emojis.is_empty() {
                        div { class: "text-[9px] uppercase tracking-wider text-[var(--text-dim)] px-1 pb-1", "This guild" }
                        div { class: "grid grid-cols-8 gap-0.5 pb-1.5 mb-1.5 border-b border-[var(--border)]",
                            for e in guild_emojis.iter().cloned() {
                                {
                                    let code = e.shortcode.clone();
                                    let url = emoji_urls.get(&e.image).cloned().unwrap_or_default();
                                    rsx! {
                                        button {
                                            key: "{e.id}",
                                            r#type: "button",
                                            class: "w-6 h-6 flex items-center justify-center rounded hover:bg-white/[0.06] text-base leading-none",
                                            title: ":{code}:",
                                            onclick: move |_| {
                                                draft.write().push_str(&format!(":{code}:"));
                                                show_emoji.set(false);
                                            },
                                            if url.is_empty() {
                                                span { class: "text-[8px] text-[var(--text-dim)]", "…" }
                                            } else {
                                                img { src: "{url}", style: "height:1.2em;width:auto;" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    div { class: "grid grid-cols-8 gap-0.5",
                        for emoji in EMOJIS.iter().copied() {
                            button {
                                r#type: "button",
                                class: "w-6 h-6 flex items-center justify-center rounded hover:bg-white/[0.06] text-base leading-none",
                                onclick: move |_| {
                                    draft.write().push_str(emoji);
                                    // Close on pick: the picker is for reaching
                                    // one emoji, and leaving it open covers the
                                    // message you were just typing.
                                    show_emoji.set(false);
                                },
                                "{emoji}"
                            }
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

                    // Attach, on the left (label opens the hidden file input).
                    // A "+" rather than a picture glyph: it's the affordance
                    // people look for on the left of a composer, and it reads as
                    // "add something" rather than "images only".
                    label {
                        class: "px-1.5 text-lg leading-none text-[var(--text-muted)] hover:text-[var(--accent)] transition-colors cursor-pointer select-none",
                        title: "Attach an image",
                        "+"
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

                    // Emoji toggle, on the right — next to Send, and directly
                    // under the picker it opens.
                    button {
                        r#type: "button",
                        class: "px-1.5 text-base leading-none text-[var(--text-muted)] hover:text-[var(--accent)] transition-colors",
                        title: "Emoji",
                        onclick: move |_| show_emoji.toggle(),
                        "🙂"
                    }

                    button {
                        class: "dxf-cta text-xs font-semibold uppercase tracking-wider px-4 py-1.5 rounded-lg disabled:opacity-30 transition-all",
                        r#type: "submit",
                        disabled: draft().trim().is_empty() && pending_image().is_none(),
                        "Send"
                    }
                }
            }
        }
    }
}
