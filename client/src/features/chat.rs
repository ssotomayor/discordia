use base64::Engine as _;
use dioxus::prelude::*;
use dioxus_grid_layout::NoDrag;
use serde_json::Value;

use crate::identity::discriminator;
use crate::protocol::{ClientMessage, Id, Message};
use crate::state::{use_app_state, use_gateway};

/// Curated emoji set for the composer picker. Plain Unicode — they travel as
/// ordinary message text, so no protocol support is needed.
const EMOJIS: &[&str] = &[
    "😀", "😂", "😅", "😍", "😎", "🤔", "😭", "😡", "👍", "👎", "🙏", "🔥", "🎉", "❤️", "💯", "✨",
    "🚀", "👀", "🙌", "😉", "🥳", "😴", "🤯", "🤝", "👋", "💀", "✅", "❌", "⚡", "🌈", "🍕", "☕",
    "🎮", "💸", "🐛", "📎", "🖼️", "🤖", "🫡", "😬",
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
    // One of three literals from the caller, so nothing hostile can reach this
    // — it goes through the shared sink anyway, because that is what keeps the
    // guarantee a property of the sink rather than of each caller.
    let mode = crate::features::screenshare::js_str(mode);
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
      // 40px tolerance for sub-pixel rounding and 'near bottom' anchoring.
      var gap = el.scrollHeight - el.scrollTop - el.clientHeight;
      el._dxfStick = gap <= 40;
    }}, {{ passive: true }});
  }}
  var mode = {mode};
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

/// Attachment bridge for drag-and-drop *and* paste, evaluated by the composer.
///
/// Both routes end in the same `{ k: 'file' }` message and the same
/// `pending_image` slot the "+" picker fills, so an image arrives identically
/// however it was offered.
///
/// Two things are worth knowing here:
///
/// * The listeners sit on `document` and are wired exactly once, but they report
///   through `window.__dxfDropSink`, which every *run* of this eval rebinds to
///   its own channel. A remount (switching to a channel-less view and back)
///   otherwise leaves the listeners talking into a dead channel nobody recvs on.
/// * `preventDefault` is called for drags anywhere in the document, not just
///   over the chat. A file dropped on a webview that doesn't claim it makes the
///   webview *navigate to that file* — the whole app simply disappears.
///
/// `$MAX` is substituted with the attachment size cap (plain `replace` rather
/// than `format!` so the script's braces don't all need doubling).
const DROP_JS: &str = r#"
(function () {
  window.__dxfDropSink = function (m) { try { dioxus.send(m); } catch (e) {} };
  if (window.__dxfDropWired) return;
  window.__dxfDropWired = true;

  function sink(m) { if (window.__dxfDropSink) window.__dxfDropSink(m); }
  function isFileDrag(e) {
    var t = e.dataTransfer && e.dataTransfer.types;
    return !!t && Array.prototype.indexOf.call(t, 'Files') >= 0;
  }
  function inZone(e) {
    var t = e.target;
    return !!(t && t.closest && t.closest('#dxf-chat-drop'));
  }

  // dragenter/dragleave fire once per element crossed, so a single drag over the
  // message list emits a burst of both. Counting them is what distinguishes
  // "moved onto a child" from "left the window".
  var depth = 0;
  document.addEventListener('dragover', function (e) { e.preventDefault(); }, false);
  document.addEventListener('dragenter', function (e) {
    e.preventDefault();
    if (!isFileDrag(e)) return;
    depth++;
    if (inZone(e)) sink({ k: 'over', v: true });
  }, false);
  document.addEventListener('dragleave', function (e) {
    if (!isFileDrag(e)) return;
    depth = Math.max(0, depth - 1);
    if (depth === 0) sink({ k: 'over', v: false });
  }, false);
  function readImage(f, typeChecked) {
    if (!typeChecked && f.type && f.type.indexOf('image/') !== 0) {
      sink({ k: 'err', v: "That's not an image." });
      return;
    }
    if (f.size > $MAX) {
      sink({ k: 'err', v: 'Image too large (max 2 MB).' });
      return;
    }
    var r = new FileReader();
    r.onload = function () {
      var url = String(r.result);
      // Webview may yield `data:;base64,...` which the server rejects. Claim
      // PNG to let the renderer sniff the real format.
      if (url.indexOf('data:image/') !== 0) url = url.replace(/^data:[^;]*;/, 'data:image/png;');
      sink({ k: 'file', v: url });
    };
    r.onerror = function () { sink({ k: 'err', v: "Couldn't read that file." }); };
    r.readAsDataURL(f);
  }

  document.addEventListener('drop', function (e) {
    e.preventDefault();
    depth = 0;
    sink({ k: 'over', v: false });
    if (!inZone(e)) return;
    var f = e.dataTransfer && e.dataTransfer.files && e.dataTransfer.files[0];
    if (!f) return;
    readImage(f);
  }, false);

  // Listen on `document` because the composer input is not always focused.
  document.addEventListener('paste', function (e) {
    // Scoped to the chat column, so an image pasted into a settings field or a
    // profile editor is not quietly turned into a message attachment.
    if (!inZone(e)) return;
    var items = (e.clipboardData && e.clipboardData.items) || [];
    var f = null;
    for (var i = 0; i < items.length; i++) {
      if (items[i].kind === 'file' && items[i].type && items[i].type.indexOf('image/') === 0) {
        f = items[i].getAsFile();
        break;
      }
    }
    // Return without preventDefault so text pasting still works.
    if (!f) return;
    // Prevent WebKit from also pasting the image or filename into the text
    // input.
    e.preventDefault();
    // Clipboard items may report size 0 until read, so rely on the size check.
    readImage(f, true);
  }, false);
})();
"#;

#[component]
pub fn ChatView() -> Element {
    let state = use_app_state();
    let gateway = use_gateway();
    // Owned here so the highlight covers the whole chat column, not just the
    // composer.
    let drag_over = use_signal(|| false);

    let snapshot = state.read();
    let selected_channel = snapshot.selected_channel;
    let dm = selected_channel.and_then(|cid| snapshot.dm_of(cid).cloned());
    let channel_meta =
        selected_channel.and_then(|cid| snapshot.channels.iter().find(|c| c.id == cid).cloned());
    let messages: Vec<Message> = selected_channel
        .and_then(|cid| snapshot.messages.get(&cid).cloned())
        .unwrap_or_default();
    let typers = selected_channel
        .map(|cid| snapshot.typers_in(cid))
        .unwrap_or_default();
    // Locked channels don't accept drops, so they use a different drop-zone
    // id.
    let drop_id = match selected_channel {
        Some(cid) if !composer_locked(&snapshot, cid) => "dxf-chat-drop",
        _ => "dxf-chat-none",
    };
    drop(snapshot);

    let (is_dm, header_name, composer_label) = match &dm {
        Some(d) => (
            true,
            d.other.username.clone(),
            format!("@{}", d.other.username),
        ),
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

    // Narrow key (channel + first/last ids) so edits/reactions don't trigger
    // scroll; first id distinguishes history paging from new messages.
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
        div { id: "{drop_id}", class: "relative flex flex-col h-full min-h-0",

            // Drop affordance. `pointer-events-none` matters: an overlay that
            // swallowed the cursor would fire dragleave the instant it appeared
            // and flicker itself away.
            if drag_over() {
                div {
                    class: "dxf-fade pointer-events-none absolute inset-0 z-30 flex items-center justify-center rounded-lg border-2 border-dashed border-[var(--accent)] bg-[var(--accent-soft)]",
                    style: "margin: 0.5rem;",
                    span { class: "text-sm font-medium text-[var(--accent)]", "Drop an image to attach it" }
                }
            }

            header { class: "h-11 px-3 flex items-center gap-3 border-b border-[var(--border)] shrink-0",
                span { class: "text-[var(--text-dim)] font-medium", if is_dm { "@" } else { "#" } }
                span { class: "text-sm text-[var(--accent)] font-medium", "{header_name}" }
                // Said in the header rather than per message, because it is a
                // property of the conversation. Shown only for DMs: a guild
                // channel is not encrypted and a badge there would be a lie.
                if is_dm {
                    span {
                        class: "text-[10px] px-1.5 py-0.5 rounded border border-[var(--border)] text-[var(--text-dim)]",
                        title: "End-to-end encrypted and sent over Nostr relays, not through this server. The relays cannot read it and cannot see who sent it. Your conversation follows your key to any server.",
                        "🔒 private · relays"
                    }
                }
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
                        // Fetches the slice before the oldest held message;
                        // the net layer merges it chronologically.
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

                if let Some(label) = typing_label {
                    div { class: "px-4 pb-1 h-4 text-[11px] text-[var(--text-dim)] italic dxf-fade",
                        "{label}"
                    }
                }

                if let Some(channel_id) = selected_channel {
                    Composer { channel_id, composer_label, drag_over }
                }
            }
        }
    }
}

/// The initial history page size (mirrors the server default). When a channel
/// holds at least this many messages there is probably older history to page
/// back through, so we surface the "load earlier" affordance.
const PAGE_SIZE: usize = 50;

/// Whether the composer is locked for this channel: it's read-only and we hold
/// neither ManageMessages nor ManageChannels there. Mirrors the server's gate
/// (which re-checks anyway) and also decides whether the chat accepts dropped
/// images, so both surfaces can't disagree.
fn composer_locked(s: &crate::state::AppState, channel_id: Id) -> bool {
    s.channels
        .iter()
        .find(|c| c.id == channel_id)
        .filter(|c| c.read_only)
        .map(|c| {
            !(s.can(c.guild_id, crate::protocol::Permission::ManageMessages)
                || s.can(c.guild_id, crate::protocol::Permission::ManageChannels))
        })
        .unwrap_or(false)
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
    let mut confirm_delete = use_signal(|| false);

    let self_pubkey = state.read().self_user.as_ref().map(|u| u.pubkey.clone());
    let timestamp = message.created_at.format("%H:%M").to_string();
    let has_text = !message.content.is_empty();
    let author_pubkey = message.author.pubkey.clone();
    let channel_id = message.channel_id;
    let message_id = message.id;
    // For the reply banner. Truncated the same way the server truncates its
    // authoritative excerpt, so the banner and the eventual quote match.
    let author_name = message.author.username.clone();
    let content_for_reply = {
        let flat = message
            .content
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if flat.is_empty() && message.image.is_some() {
            "[image]".to_string()
        } else if flat.chars().count() > crate::protocol::REPLY_EXCERPT_CHARS {
            let cut: String = flat
                .chars()
                .take(crate::protocol::REPLY_EXCERPT_CHARS)
                .collect();
            format!("{cut}…")
        } else {
            flat
        }
    };
    let quoted = message.reply_to.clone();

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

    // Open menus must pin the bar: they render inside the hover-gated wrapper,
    // so hover alone would fade them out when the pointer leaves the row to
    // reach the picker.
    let menu_open = show_react() || confirm_delete();
    let bar_visibility = if menu_open {
        "opacity-100"
    } else {
        "opacity-0 group-hover:opacity-100"
    };

    rsx! {
        // Rendered from the message's snapshot so it draws even if the parent
        // isn't in the current history page.
        if let Some(q) = quoted {
            div { class: "flex gap-3 -mx-4 px-4 pt-1",
                div { class: "w-8 shrink-0" }
                div { class: "min-w-0 flex items-center gap-1.5 text-[11px] text-[var(--text-dim)]",
                    span { class: "shrink-0 opacity-60", "↩" }
                    span { class: "shrink-0 font-medium text-[var(--text-muted)]",
                        "{q.author_username}"
                    }
                    span { class: "truncate", "{q.excerpt}" }
                }
            }
        }
        div { class: "group relative flex gap-3 -mx-4 px-4 py-0.5 hover:bg-white/[0.02] dxf-msg-in",

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
                    {
                        let full = img.clone();
                        rsx! {
                            img {
                                // Opaque backing and visible edge: the old
                                // `--border` is ~15% alpha, so screenshots
                                // with transparency bled into the background
                                // with no clear boundary.
                                class: "mt-1 rounded-md border border-[var(--border-strong)] bg-[var(--panel2)] max-w-xs max-h-80 object-contain block hover:border-[var(--accent)] transition-colors",
                                style: "cursor: zoom-in;",
                                src: "{img}",
                                alt: "attachment",
                                title: "Click to view full size",
                                onclick: move |_| state.write().image_viewer = Some(full.clone()),
                            }
                        }
                    }
                }
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
                                        // Custom emojis ride in as
                                        // `:shortcode:` strings and need the
                                        // same resolution as message text,
                                        // otherwise the chip shows the raw
                                        // code.
                                        span { EmojiText { text: r.emoji.clone(), guild_id } }
                                        span { class: "text-[10px]", "{count}" }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            div { class: "absolute -top-2 right-3 {bar_visibility} transition-opacity",
                div { class: "relative flex gap-1",
                    // z-20 sits under menus (z-30) but over content, enabling
                    // outside-click dismissal.
                    if menu_open {
                        div {
                            class: "fixed inset-0 z-20",
                            onclick: move |_| {
                                show_react.set(false);
                                confirm_delete.set(false);
                            },
                        }
                    }
                    button {
                        class: "w-7 h-7 rounded-md border border-[var(--border)] bg-[var(--panel-solid)] text-[var(--text-muted)] hover:text-[var(--accent)] hover:border-[var(--accent)] text-sm leading-none transition-colors",
                        title: "Reply",
                        onclick: {
                            // The banner's excerpt is local-only cosmetics; the
                            // server quotes from its own row on send.
                            let author = author_name.clone();
                            let body = content_for_reply.clone();
                            move |_| {
                                state.write().replying_to = Some(crate::state::ReplyDraft {
                                    message_id,
                                    channel_id,
                                    author_username: author.clone(),
                                    excerpt: body.clone(),
                                });
                            }
                        },
                        "↩"
                    }
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
                    // Menus open upward: downward would be clipped by the
                    // scrollport on the newest (most-used) message.
                    if confirm_delete() {
                        div { class: "dxf-pop-in absolute right-0 bottom-full mb-1 z-30 flex items-center gap-1 p-1 bg-[var(--panel-solid)] border border-[var(--border)] rounded-md shadow-lg",
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
                        div { class: "dxf-pop-in absolute right-0 bottom-full mb-1 z-30 flex gap-1 p-1 bg-[var(--panel-solid)] border border-[var(--border)] rounded-md shadow-lg",
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

/// Full-size viewer for a chat image, driven by `AppState.image_viewer`.
///
/// Mounted at the workspace root rather than inside the message row: panels can
/// overlap, so a modal rendered inside the chat panel sits in that panel's
/// stacking context and paints underneath whatever is stacked above it. Same
/// reason `ProfileCard` lives up there.
#[component]
pub fn ImageViewer() -> Element {
    let mut state = use_app_state();
    let Some(src) = state.read().image_viewer.clone() else {
        return rsx! { Fragment {} };
    };

    rsx! {
        div {
            class: "dxf-backdrop-in fixed inset-0 z-50 flex items-center justify-center",
            style: "padding: 2.5rem; background: rgba(0,0,0,0.8);",
            onclick: move |_| state.write().image_viewer = None,
            img {
                class: "dxf-modal-in object-contain rounded-lg shadow-2xl",
                style: "max-width: 100%; max-height: 100%;",
                src: "{src}",
                alt: "attachment",
                onclick: move |e| e.stop_propagation(),
            }
            button {
                class: "absolute w-9 h-9 flex items-center justify-center rounded-full border border-[var(--border)] bg-[var(--panel-solid)] text-[var(--text-muted)] hover:text-[var(--text)] hover:border-[var(--border-strong)] transition-colors",
                style: "top: 1rem; right: 1.25rem;",
                title: "Close",
                onclick: move |_| state.write().image_viewer = None,
                "✕"
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
fn Composer(channel_id: Id, composer_label: String, drag_over: Signal<bool>) -> Element {
    let mut state = use_app_state();
    // Filter by channel_id because it is a plain prop, not a signal; an effect
    // keyed on it would capture the first value and never re-run.
    let replying_to = use_memo(move || {
        state
            .read()
            .replying_to
            .clone()
            .filter(|r| r.channel_id == channel_id)
    });
    let mut draft = use_signal(String::new);
    let mut pending_image = use_signal::<Option<String>>(|| None);
    let mut attach_err = use_signal::<Option<String>>(|| None);
    let mut show_emoji = use_signal(|| false);
    let mut last_typing = use_signal::<Option<std::time::Instant>>(|| None);
    let gateway = use_gateway();
    let gateway_submit = gateway.clone();
    let nostr_submit = use_context::<crate::nostr::service::NostrTx>();

    let mut drag_over = drag_over;
    use_future(move || async move {
        let js = DROP_JS.replace("$MAX", &MAX_IMAGE_BYTES.to_string());
        let mut eval = document::eval(&js);
        while let Ok(msg) = eval.recv::<Value>().await {
            let v = msg.get("v");
            match msg.get("k").and_then(|k| k.as_str()) {
                Some("over") => drag_over.set(v.and_then(|v| v.as_bool()).unwrap_or(false)),
                Some("file") => {
                    if let Some(url) = v.and_then(|v| v.as_str()) {
                        attach_err.set(None);
                        pending_image.set(Some(url.to_string()));
                    }
                }
                Some("err") => attach_err.set(v.and_then(|v| v.as_str()).map(str::to_string)),
                _ => {}
            }
        }
    });

    // Snapshot to avoid spanning the state borrow across the closures below.
    let (guild_emojis, emoji_urls) = {
        let state = use_app_state();
        let s = state.read();
        let gid = s
            .channels
            .iter()
            .find(|c| c.id == channel_id)
            .map(|c| c.guild_id);
        let list = gid.map(|g| s.emojis_of(g).to_vec()).unwrap_or_default();
        let urls: std::collections::HashMap<String, String> = list
            .iter()
            .filter_map(|e| {
                s.emoji_images
                    .get(&e.image)
                    .map(|u| (e.image.clone(), u.clone()))
            })
            .collect();
        (list, urls)
    };

    let locked = {
        let state = use_app_state();
        let s = state.read();
        composer_locked(&s, channel_id)
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
        // Only the id goes out; the server rebuilds the quote from its own row.
        let reply_to = replying_to().map(|r| r.message_id);
        // DMs bypass the gateway entirely; they are Nostr events on relays,
        // allowing conversations to outlive the connected server.
        let dm_peer = state
            .read()
            .dm_of(channel_id)
            .map(|d| d.other.pubkey.clone());
        if let Some(peer) = dm_peer {
            if image.is_some() {
                // Nostr path does not yet support attachments (NIP-17 kind:15
                // requires encrypted blob upload); erroring is better than
                // silently dropping the image.
                state.write().error_toast =
                    Some("Images in DMs are not supported yet — the text was not sent.".into());
                return;
            }
            let reply_event =
                reply_to.and_then(|id| state.read().nostr_event_ids.get(&id).cloned());
            nostr_submit.send(crate::nostr::service::NostrCmd::Send {
                peer,
                text: content,
                reply_to: reply_event,
            });
        } else {
            gateway_submit.send(ClientMessage::SendMessage {
                channel_id,
                content,
                image,
                reply_to,
            });
        }
        draft.set(String::new());
        pending_image.set(None);
        show_emoji.set(false);
        if reply_to.is_some() {
            state.write().replying_to = None;
        }
    };

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

            if let Some(r) = replying_to() {
                div { class: "flex items-center gap-2 mb-1 px-2 py-1 rounded-t-md bg-[var(--panel-solid)] border border-b-0 border-[var(--border)] text-[11px]",
                    span { class: "shrink-0 text-[var(--text-dim)] opacity-60", "↩" }
                    span { class: "shrink-0 text-[var(--text-muted)]", "Replying to" }
                    span { class: "shrink-0 font-medium text-[var(--accent)]", "{r.author_username}" }
                    span { class: "truncate text-[var(--text-dim)]", "{r.excerpt}" }
                    button {
                        class: "ml-auto shrink-0 w-5 h-5 rounded text-[var(--text-dim)] hover:text-[var(--danger)] leading-none transition-colors",
                        title: "Cancel reply",
                        onclick: move |_| state.write().replying_to = None,
                        "✕"
                    }
                }
            }

            // Emoji picker popover, floats above the input row. Anchored to the
            // right so it sits under the button that opens it, and sized to its
            // content instead of stretching the full composer width.
            if show_emoji() {
                div {
                    class: "dxf-pop-in absolute bottom-full right-3 mb-2 p-1.5 bg-[var(--panel-solid)] border border-[var(--border)] rounded-md shadow-lg z-30",
                    // Guild emojis first (untypeable elsewhere); inserted as
                    // :shortcode: to keep draft plain text.
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
                                    // Close on pick to avoid covering the
                                    // message being typed.
                                    show_emoji.set(false);
                                },
                                "{emoji}"
                            }
                        }
                    }
                }
            }

            if let Some(img) = pending_image() {
                div { class: "mb-2 flex items-center gap-2",
                    img {
                        // Use --border-strong and opaque bg: --border is ~15%
                        // alpha (invisible), and transparency lets chat show
                        // through.
                        class: "h-16 w-16 object-cover rounded border border-[var(--border-strong)] bg-[var(--panel2)]",
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

                    // Use "+" not image glyph: reads as "add something" (not
                    // images-only) and matches left-side affordance
                    // expectations.
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
