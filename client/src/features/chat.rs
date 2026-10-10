use dioxus::html::HasFileData;
use dioxus::prelude::*;
use dioxus_grid_layout::NoDrag;
use serde_json::Value;

use crate::identity::discriminator;
use crate::protocol::{ClientMessage, GuildEmoji, Id, Message};
use crate::state::{use_app_state, use_gateway};

/// Named so `:fi` can offer 🔥. Names follow the usual shortcode set.
#[rustfmt::skip]
pub(crate) const EMOJIS: &[(&str, &str)] = &[
    ("😀", "grinning"), ("😂", "joy"), ("😅", "sweat_smile"), ("😍", "heart_eyes"),
    ("😎", "sunglasses"), ("🤔", "thinking"), ("😭", "sob"), ("😡", "rage"),
    ("👍", "thumbsup"), ("👎", "thumbsdown"), ("🙏", "pray"), ("🔥", "fire"),
    ("🎉", "tada"), ("❤️", "heart"), ("💯", "100"), ("✨", "sparkles"),
    ("🚀", "rocket"), ("👀", "eyes"), ("🙌", "raised_hands"), ("😉", "wink"),
    ("🥳", "partying_face"), ("😴", "sleeping"), ("🤯", "exploding_head"), ("🤝", "handshake"),
    ("👋", "wave"), ("💀", "skull"), ("✅", "white_check_mark"), ("❌", "x"),
    ("⚡", "zap"), ("🌈", "rainbow"), ("🍕", "pizza"), ("☕", "coffee"),
    ("🎮", "video_game"), ("💸", "money_with_wings"), ("🐛", "bug"), ("📎", "paperclip"),
    ("🖼️", "framed_picture"), ("🤖", "robot"), ("🫡", "saluting_face"), ("😬", "grimacing"),
];

const QUICK_REACTIONS: &[&str] = &["👍", "❤️", "😂", "🎉", "🔥", "👀", "🙏", "✅"];

const GROUP_WINDOW_SECS: i64 = 300;

const SCROLL_JS: &str = r#"
return (async function () {
  if (window.__dxfChatScrollOff) window.__dxfChatScrollOff();
  let node = null, channel = null, sequence = 0, alive = true, wake = null;
  let frame = null, observer = null, anchor = null;
  const observed = new Set();
  const queueMeasure = () => {
    if (!alive || frame !== null) return;
    frame = requestAnimationFrame(() => { frame = null; measure('resize', channel); });
  };
  if (typeof ResizeObserver !== 'undefined') observer = new ResizeObserver(queueMeasure);
  const measure = (mode, cid) => {
    if (!alive) return;
    const el = document.getElementById('dxf-chat-scroll');
    if (!el) return;
    if (node !== el) {
      if (node) node.removeEventListener('scroll', onScroll);
      node = el;
      node.addEventListener('scroll', onScroll, { passive: true });
      if (observer) { observer.disconnect(); observed.clear(); observer.observe(node); }
    }
    if (channel !== cid) anchor = null;
    channel = cid;
    const pages = el.querySelectorAll ? Array.from(el.querySelectorAll('[data-chat-page]')) : [];
    for (const old of observed) {
      if (!pages.includes(old)) { if (observer) observer.unobserve(old); observed.delete(old); }
    }
    const rect = el.getBoundingClientRect ? el.getBoundingClientRect() : { top: 0 };
    const measured = pages.map(page => {
      if (observer && !observed.has(page)) { observer.observe(page); observed.add(page); }
      const bounds = page.getBoundingClientRect();
      return { id: page.dataset.chatPage, top: bounds.top - rect.top, height: bounds.height };
    });
    const rows = el.querySelectorAll ? Array.from(el.querySelectorAll('[data-chat-row]')) : [];
    const candidates = rows.length ? rows : pages;
    const key = row => row.dataset.chatRow || row.dataset.chatPage;
    const oldAnchor = anchor && candidates.find(row => key(row) === anchor.id);
    const anchorShift = mode !== 'scroll' && oldAnchor
      ? oldAnchor.getBoundingClientRect().top - rect.top - anchor.offset : null;
    const first = candidates.find(row => row.getBoundingClientRect().bottom > rect.top);
    anchor = first ? { id: key(first), offset: first.getBoundingClientRect().top - rect.top } : null;
    dioxus.send({ mode, channel, sequence: ++sequence, height: el.scrollHeight,
      top: el.scrollTop, viewport: el.clientHeight, pages: measured, anchor_shift: anchorShift });
  };
  const onScroll = () => measure('scroll', channel);
  window.__dxfChatScrollMeasure = measure;
  const off = () => {
    alive = false;
    if (wake) wake(null);
    if (frame !== null) cancelAnimationFrame(frame);
    if (observer) observer.disconnect();
    observed.clear();
    if (node) node.removeEventListener('scroll', onScroll);
    window.__dxfChatScrollMeasure = null;
    window.__dxfChatScrollOff = null;
  };
  window.__dxfChatScrollOff = off;
  dioxus.send(true);
  try {
    while (alive) {
      const command = await new Promise(resolve => {
        wake = resolve;
        dioxus.recv().then(resolve, () => resolve(null));
      });
      wake = null;
      if (!alive || command === null) break;
      if (command.sequence === sequence && node === document.getElementById('dxf-chat-scroll')
          && command.channel === channel && command.top !== null) node.scrollTop = command.top;
    }
  } finally {
    if (window.__dxfChatScrollOff === off) off();
  }
})();
"#;

const PASTE_JS: &str = r#"
(function () {
  window.__dxfPasteSink = function () { try { dioxus.send({k: 'paste'}); } catch (_) {} };
  if (window.__dxfPasteWired) return;
  window.__dxfPasteWired = true;
  document.addEventListener('paste', function (e) {
    if (!e.target.closest || !e.target.closest('#dxf-chat-drop')) return;
    var items = (e.clipboardData && e.clipboardData.items) || [];
    for (var i = 0; i < items.length; i++) {
      if (items[i].kind === 'file' && items[i].type.indexOf('image/') === 0) {
        e.preventDefault();
        window.__dxfPasteSink();
        return;
      }
    }
  });
})();
"#;

fn load_attachment(
    path: Option<std::path::PathBuf>,
    mut pending: Signal<Option<String>>,
    mut error: Signal<Option<String>>,
    mut generation: Signal<u64>,
) {
    let request = generation.peek().wrapping_add(1);
    generation.set(request);
    spawn(async move {
        let result = tokio::task::spawn_blocking(move || match path {
            Some(path) => crate::chat_image::read_file(&path),
            None => crate::chat_image::read_clipboard(),
        })
        .await;
        if *generation.peek() != request {
            return;
        }
        match result.unwrap_or_else(|e| Err(format!("Couldn't load that image: {e}"))) {
            Ok(url) => {
                error.set(None);
                pending.set(Some(url));
            }
            Err(message) => error.set(Some(message)),
        }
    });
}

#[component]
pub fn ChatView() -> Element {
    let state = use_app_state();
    let gateway = use_gateway();
    let mut drag_over = use_signal(|| false);
    let mut dropped_file = use_signal::<Option<std::path::PathBuf>>(|| None);

    let snapshot = state.read();
    let selected_channel = snapshot.selected_channel;
    let dm = selected_channel.and_then(|cid| snapshot.dm_of(cid).cloned());
    let channel_meta =
        selected_channel.and_then(|cid| snapshot.channels.iter().find(|c| c.id == cid).cloned());
    let typers = selected_channel
        .map(|cid| snapshot.typers_in(cid))
        .unwrap_or_default();
    let drop_id = match selected_channel {
        Some(cid) if !composer_locked(&snapshot, cid) => "dxf-chat-drop",
        _ => "dxf-chat-none",
    };
    let dm_name = dm
        .as_ref()
        .map(|d| snapshot.display_name(&d.other_pubkey))
        .unwrap_or_default();
    drop(snapshot);

    let (is_dm, header_name, composer_label) = match &dm {
        Some(_) => (true, dm_name.clone(), format!("@{dm_name}")),
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
    let channel_guild = channel_meta.as_ref().map(|c| c.guild_id);
    let typing_label = typing_label(&typers);

    let scroll_key = use_memo(move || {
        let s = state.read();
        let cid = s.selected_channel;
        let msgs = cid.and_then(|c| s.messages.get(&c));
        (
            cid,
            msgs.and_then(|m| m.first().map(|x| x.id)),
            msgs.and_then(|m| m.last().map(|x| x.id)),
            s.command_notes.len(),
        )
    });
    let mut page_states =
        use_signal(std::collections::HashMap::<Id, super::chat_scroll::PageState>::new);
    let mut scroll_ready = use_signal(|| false);
    let mut virtual_enabled = use_signal(|| true);
    let mut scroll_mounted = use_signal(|| 0_u64);
    use_future(move || async move {
        let mut eval = document::eval(SCROLL_JS);
        if eval.recv::<bool>().await.is_err() {
            virtual_enabled.set(false);
            return;
        }
        scroll_ready.set(true);
        let mut policy = super::chat_scroll::ScrollState::default();
        let mut page_channel = None;
        while let Ok(sample) = eval.recv::<super::chat_scroll::ScrollSample>().await {
            if sample.channel != state.peek().selected_channel {
                continue;
            }
            let next_pages = super::chat_scroll::page_states(&sample.pages, sample.viewport);
            if page_channel != sample.channel || *page_states.peek() != next_pages {
                page_channel = sample.channel;
                page_states.set(next_pages);
            }
            let command = policy.update(sample);
            if eval.send(command).is_err() {
                break;
            }
        }
        virtual_enabled.set(false);
    });
    use_drop(|| {
        let _ = document::eval("window.__dxfChatScrollOff && window.__dxfChatScrollOff();");
    });
    let mut prev_key = use_signal(|| (None::<Id>, None::<Id>));
    use_effect(move || {
        let (cid, first, _last, _notes) = scroll_key();
        let _ = scroll_mounted();
        if !scroll_ready() {
            return;
        }
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
        let channel = serde_json::to_string(&cid).unwrap_or_else(|_| "null".into());
        let _ = document::eval(&format!(
            "window.__dxfChatScrollMeasure && window.__dxfChatScrollMeasure({mode:?}, {channel});"
        ));
    });
    use_effect(move || {
        let _ = page_states.read();
        if !scroll_ready() {
            return;
        }
        let channel =
            serde_json::to_string(&state.peek().selected_channel).unwrap_or_else(|_| "null".into());
        let _ = document::eval(&format!(
            "window.__dxfChatScrollMeasure && window.__dxfChatScrollMeasure('resize', {channel});"
        ));
    });

    let history = state.read();
    let messages = selected_channel
        .and_then(|cid| history.messages.get(&cid))
        .map(Vec::as_slice)
        .unwrap_or_default();
    let pages = page_states.read();
    rsx! {
        div { id: "{drop_id}", class: "relative flex flex-col h-full min-h-0",
            ondragover: move |event: DragEvent| {
                event.prevent_default();
                if drop_id == "dxf-chat-drop" { drag_over.set(true); }
            },
            ondragleave: move |_| drag_over.set(false),
            ondrop: move |event: DragEvent| {
                event.prevent_default();
                drag_over.set(false);
                if drop_id == "dxf-chat-drop"
                    && let Some(file) = event.files().into_iter().next() {
                    dropped_file.set(Some(file.path()));
                }
            },

            if drag_over() {
                div {
                    class: "dxf-fade pointer-events-none absolute inset-0 z-30 flex items-center justify-center rounded-lg border-2 border-dashed border-[var(--accent)] bg-[var(--accent-soft)]",
                    style: "margin: 0.5rem;",
                    span { class: "text-sm font-medium text-[var(--accent)]", "Drop an image to attach it" }
                }
            }

            header { class: "h-12 px-3.5 flex items-center gap-3 border-b border-[var(--border)] shrink-0",
                span { class: "shrink-0 font-mono text-base text-[var(--text-dim)]", if is_dm { "@" } else { "#" } }
                // The name yields first: a wrapped badge costs a line, a
                // wrapped key costs nothing you could not read from the list.
                span { class: "dxf-display min-w-0 truncate text-[16px] font-bold tracking-tight text-[var(--text)]", "{header_name}" }
                if is_dm {
                    if let Some(dm) = &dm {
                        crate::features::dm_call::CallButton { peer: dm.other_pubkey.clone() }
                    }
                    span {
                        class: "shrink-0 whitespace-nowrap text-[10px] px-1.5 py-0.5 rounded border border-[var(--border)] text-[var(--text-dim)]",
                        title: "End-to-end encrypted and sent over Nostr relays, not through this server. The relays cannot read it and cannot see who sent it. Your conversation follows your key to any server.",
                        "🔒 private · relays"
                    }
                }
                if let Some(topic) = channel_topic {
                    span {
                        class: "min-w-0 truncate pl-3 text-[12.5px] text-[var(--text-dim)] border-l border-[var(--border-strong)]",
                        EmojiText { text: topic, guild_id: channel_guild }
                    }
                }
            }

            if !is_dm && state.read().server_chat_tools {
                for channel_id in selected_channel.into_iter() {
                    super::chat_tools::ChatTools { key: "{channel_id}", channel_id }
                }
            }
            NoDrag {
                div { id: "dxf-chat-scroll",
                style: "overflow-anchor: none;", onmounted: move |_| scroll_mounted += 1, class: "flex-1 overflow-y-auto px-4 py-4 min-h-0",
                    if messages.is_empty() && selected_channel.is_some() {
                        div { class: "h-full flex items-center justify-center text-[var(--text-dim)] text-xs",
                            if is_dm { "No messages yet. Say hi 👋" } else { "No messages yet." }
                        }
                    } else {
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
                        for start in (0..messages.len()).step_by(PAGE_SIZE) {
                            {
                                let end = (start + PAGE_SIZE).min(messages.len());
                                let id = messages[start].id;
                                let page = pages.get(&id);
                                let active = !virtual_enabled() || page.map(|p| p.active).unwrap_or(end + PAGE_SIZE >= messages.len());
                                let height = page.map(|p| p.height).unwrap_or((end - start) as f64 * 80.0);
                                rsx! {
                                    MessagePage { key: "{id}", id, channel_id: messages[start].channel_id, start, end, active, height, is_dm }
                                }
                            }
                        }
                    }
                    if let Some(channel_id) = selected_channel {
                        crate::features::bot_commands::CommandNotes { channel_id }
                    }
                }

                if let Some(label) = typing_label {
                    div { class: "px-4 pb-1 h-4 text-[11px] text-[var(--text-dim)] italic dxf-fade",
                        "{label}"
                    }
                }

                for channel_id in selected_channel {
                    Composer { key: "{channel_id}", channel_id, composer_label: composer_label.clone(), dropped_file }
                }
            }
        }
    }
}

#[component]
fn MessagePage(
    id: Id,
    channel_id: Id,
    start: usize,
    end: usize,
    active: bool,
    height: f64,
    is_dm: bool,
) -> Element {
    let state = use_app_state();
    if !active {
        return rsx! {
            div { "data-chat-page": "{id}", style: "display: flow-root; height: {height}px;" }
        };
    }
    let snapshot = state.read();
    let messages = snapshot
        .messages
        .get(&channel_id)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let start = start.min(messages.len());
    let end = end.min(messages.len()).max(start);
    let style = if active {
        "display: flow-root; height: auto;".to_string()
    } else {
        format!("display: flow-root; height: {height}px;")
    };
    rsx! {
        div { "data-chat-page": "{id}", style,
            if active {
                for i in start..end {
                    {
                        let msg = &messages[i];
                        let new_day = i == 0 || day_of(&messages[i - 1]) != day_of(msg);
                        let grouped = !new_day && i > 0 && groups_with(&messages[i - 1], msg);
                        let day = new_day.then(|| day_label(day_of(msg)));
                        let mut message = msg.clone();
                        if is_dm { message.author.username = snapshot.display_name(&message.author.pubkey); }
                        rsx! {
                            div { key: "{msg.id}", "data-chat-row": "{msg.id}", style: "display: flow-root;",
                                if let Some(day) = day {
                                    div { class: "flex items-center gap-3", style: "margin: 0.85rem 0 0.6rem;",
                                        div { class: "flex-1", style: "height:1px; background: var(--border);" }
                                        span { class: "text-[10px] uppercase tracking-wider text-[var(--text-dim)]", "{day}" }
                                        div { class: "flex-1", style: "height:1px; background: var(--border);" }
                                    }
                                }
                                MessageRow { message, grouped }
                            }
                        }
                    }
                }
            }
        }
    }
}

const PAGE_SIZE: usize = 50;

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

fn groups_with(prev: &Message, cur: &Message) -> bool {
    prev.author.pubkey == cur.author.pubkey
        && (cur.created_at - prev.created_at).num_seconds().abs() < GROUP_WINDOW_SECS
}

/// Local, not UTC: a divider that says "Today" against a clock nobody is
/// reading puts the evening's messages under tomorrow.
fn day_of(m: &Message) -> chrono::NaiveDate {
    m.created_at.with_timezone(&chrono::Local).date_naive()
}

fn day_label(day: chrono::NaiveDate) -> String {
    let today = chrono::Local::now().date_naive();
    if day == today {
        "Today".to_string()
    } else if Some(day) == today.pred_opt() {
        "Yesterday".to_string()
    } else {
        day.format("%a, %b %-d, %Y").to_string()
    }
}

/// A bare `@word` is already painted for everyone; this is the narrower
/// question of whether the word names *you*.
fn mentions_user(content: &str, username: &str) -> bool {
    content.split_whitespace().any(|w| {
        w.strip_prefix('@')
            .map(|rest| rest.trim_end_matches(|c: char| c.is_ascii_punctuation()))
            .is_some_and(|name| name.eq_ignore_ascii_case(username))
    })
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
pub(super) fn MessageRow(message: Message, grouped: bool) -> Element {
    let mut state = use_app_state();
    let gateway = use_gateway();
    let nostr = use_context::<crate::nostr::service::NostrTx>();
    let delivery = state.read().dm_delivery.get(&message.id).copied();
    let mut show_react = use_signal(|| false);
    let mut confirm_delete = use_signal(|| false);

    let self_pubkey = state.read().self_user.as_ref().map(|u| u.pubkey.clone());
    let mentions_me = state
        .read()
        .self_user
        .as_ref()
        .is_some_and(|u| mentions_user(&message.content, &u.username));
    let mention_style = if mentions_me {
        "background: color-mix(in srgb, var(--accent) 6%, transparent); box-shadow: inset 2px 0 0 var(--accent);"
    } else {
        ""
    };
    let timestamp = message
        .created_at
        .with_timezone(&chrono::Local)
        .format("%H:%M")
        .to_string();
    let has_text = !message.content.is_empty();
    let author_pubkey = message.author.pubkey.clone();
    let channel_id = message.channel_id;
    let message_id = message.id;
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

    let guild_id = state
        .read()
        .channels
        .iter()
        .find(|c| c.id == channel_id)
        .map(|c| c.guild_id);

    let can_pin = state.read().server_chat_tools
        && guild_id.is_some_and(|gid| {
            state
                .read()
                .can(gid, crate::protocol::Permission::ManageMessages)
        });
    let pinned = message.pinned;
    let can_delete = {
        let s = state.read();
        let is_author = self_pubkey.as_deref() == Some(message.author.pubkey.as_str());
        is_author
            || guild_id
                .map(|gid| s.can(gid, crate::protocol::Permission::ManageMessages))
                .unwrap_or(false)
    };

    let delivery_mark = delivery.map(|delivery| {
        use crate::nostr::delivery::Delivery;
        let icon = "inline-block w-3 h-3 align-[-1px] text-[var(--text-dim)]";
        match delivery {
            Delivery::Pending => rsx! {
                span { class: icon, title: "Sending…", dangerous_inner_html: crate::features::icons::CLOCK }
            },
            Delivery::Accepted => rsx! {
                span {
                    class: icon,
                    title: "Accepted by relay. Not a delivery or read receipt.",
                    dangerous_inner_html: crate::features::icons::CHECK,
                }
            },
            Delivery::Failed => rsx! {
                span { class: "text-[10px] text-[var(--danger)]",
                    "Not accepted · "
                    button { class: "underline", onclick: move |_| nostr.send(crate::nostr::service::NostrCmd::Retry { message_id }), "Retry" }
                }
            },
        }
    });

    let menu_open = show_react() || confirm_delete();
    let bar_visibility = if menu_open {
        "opacity-100"
    } else {
        "opacity-0 group-hover:opacity-100"
    };

    rsx! {
        if let Some(q) = quoted {
            div { class: "flex gap-3 -mx-4 px-4 pt-1",
                div { class: "w-9 shrink-0" }
                div { class: "min-w-0 flex items-center gap-1.5 text-[11px] text-[var(--text-dim)]",
                    span { class: "shrink-0 opacity-60", "↩" }
                    span { class: "shrink-0 font-medium text-[var(--text-muted)]",
                        "{q.author_username}"
                    }
                    span { class: "truncate", "{q.excerpt}" }
                }
            }
        }
        div {
            class: "group relative flex gap-3 -mx-4 px-4 py-0.5 hover:bg-white/[0.02] dxf-msg-in",
            style: "{mention_style}",

            if grouped {
                div { class: "w-9 shrink-0 font-mono text-[9.5px] text-[var(--text-dim)] text-right pt-1.5 opacity-0 group-hover:opacity-100 transition-opacity",
                    "{timestamp}"
                }
            } else {
                div {
                    class: "cursor-pointer mt-0.5",
                    onclick: move |_| state.write().profile_card = Some(author_pubkey.clone()),
                    crate::features::profiles::Avatar {
                        pubkey: message.author.pubkey.clone(),
                        name: message.author.username.clone(),
                        size: "w-9 h-9",
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
                        MessageContent { content: message.content.clone(), channel_id, message_id }
                        if let Some(mark) = delivery_mark.clone() {
                            span { class: "ml-1.5", {mark} }
                        }
                    }
                } else if let Some(mark) = delivery_mark.clone() {
                    div { class: "mt-0.5", {mark} }
                }
                if pinned { div { class: "text-xs text-[var(--text-dim)]", "📌 Pinned" } }
                if let Some(attachment) = message.attachment.clone() {
                    super::chat_tools::FileAttachment { attachment }
                }
                if let Some(img) = message.image.as_ref() {
                    {
                        let resolved = state.read().media_src(img).map(str::to_string);
                        match resolved {
                            Some(src) => {
                                let full = src.clone();
                                rsx! {
                                    img {
                                        class: "mt-1 rounded-md border border-[var(--border-strong)] bg-[var(--panel2)] max-w-xs max-h-80 object-contain block hover:border-[var(--accent)] transition-colors",
                                        style: "cursor: zoom-in;",
                                        src: "{src}",
                                        alt: "attachment",
                                        title: "Click to view full size",
                                        onclick: move |_| state.write().image_viewer = Some(full.clone()),
                                    }
                                }
                            }
                            None => rsx! {
                                div {
                                    class: "mt-1 rounded-md border border-[var(--border-strong)] bg-[var(--panel2)] w-48 h-24 flex items-center justify-center text-[10px] text-[var(--text-dim)]",
                                    "Loading image…"
                                }
                            },
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
                                        class: "dxf-pop flex items-center gap-1 px-1.5 min-h-6 py-0.5 rounded-full border text-xs leading-none transition-colors {cls}",
                                        onclick: move |_| g.send(ClientMessage::React { channel_id, message_id, emoji: emoji.clone() }),
                                        span { class: "flex items-center", EmojiText { text: r.emoji.clone(), guild_id, reaction: true } }
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
                    if can_pin {
                        button {
                            title: if pinned { "Unpin message" } else { "Pin message" },
                            onclick: { let g = gateway.clone(); move |_| g.send(ClientMessage::PinMessage { channel_id, message_id, pinned: !pinned }) },
                            "📌"
                        }
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
                        div { class: "dxf-pop-in absolute right-0 bottom-full mb-1 z-30 p-1 bg-[var(--panel-solid)] border border-[var(--border)] rounded-md shadow-lg",
                            div { class: "flex gap-1",
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
                            if let Some(gid) = guild_id {
                                GuildReactionPicker {
                                    guild_id: gid,
                                    on_pick: move |emoji: String| {
                                        gateway.send(ClientMessage::React { channel_id, message_id, emoji });
                                        show_react.set(false);
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

/// This guild's emoji under the quick row. The server accepts `:code:` only
/// for an emoji the guild has, so the list is the guild's, not a search.
#[component]
fn GuildReactionPicker(guild_id: Id, on_pick: EventHandler<String>) -> Element {
    let state = use_app_state();
    let mut emoji_menu = use_signal(|| None::<EmojiMenuTarget>);
    let emojis: Vec<(String, String, String)> = {
        let s = state.read();
        s.emojis_of(guild_id)
            .iter()
            .map(|e| {
                let url = s.emoji_images.get(&e.image).cloned().unwrap_or_default();
                (e.shortcode.clone(), e.image.clone(), url)
            })
            .collect()
    };
    if emojis.is_empty() {
        return rsx! {};
    }
    if let Some(target) = emoji_menu() {
        return rsx! {
            div { class: "mt-1 pt-1 border-t border-[var(--border)]",
                EmojiSaveForm { target, on_done: move |_| emoji_menu.set(None) }
            }
        };
    }
    rsx! {
        div { class: "mt-1 pt-1 border-t border-[var(--border)] max-h-32 overflow-y-auto",
            div { class: "grid grid-cols-8 gap-0.5 w-max max-w-[15rem]",
                for (code, image, url) in emojis.into_iter() {
                    {
                        let emoji = format!(":{code}:");
                        let target = EmojiMenuTarget { shortcode: code.clone(), image, guild_id, url: url.clone() };
                        rsx! {
                            button {
                                key: "{code}",
                                class: "w-7 h-7 flex items-center justify-center rounded hover:bg-white/[0.06] text-base leading-none",
                                title: "{emoji} — right-click to save or remove",
                                onclick: move |_| on_pick.call(emoji.clone()),
                                oncontextmenu: move |ev: MouseEvent| {
                                    ev.prevent_default();
                                    emoji_menu.set(Some(target.clone()));
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
    }
}

/// Loads saved emoji pictures off disk into the media cache, once each: a
/// DM has no gateway to ask, and a guild's picker shows them as saved.
pub fn use_saved_emoji_pictures() {
    let mut state = use_app_state();
    let mut warmed = use_signal(std::collections::HashSet::<String>::new);
    use_effect(move || {
        let s = state.read();
        let missing: Vec<String> = s
            .saved_emoji
            .iter()
            .map(|e| e.image.clone())
            .filter(|image| !s.emoji_images.contains_key(image) && !warmed.peek().contains(image))
            .collect();
        drop(s);
        if missing.is_empty() {
            return;
        }
        warmed.write().extend(missing.iter().cloned());
        spawn(async move {
            let loaded = tokio::task::spawn_blocking(move || {
                missing
                    .into_iter()
                    .filter_map(|image: String| {
                        crate::emoji::load_saved(&image).map(|d| (image, d))
                    })
                    .collect::<Vec<_>>()
            })
            .await
            .unwrap_or_default();
            if !loaded.is_empty() {
                let mut s = state.write();
                for (image, data) in loaded {
                    s.emoji_images.insert(image, data);
                }
            }
        });
    });
}

#[derive(Clone, PartialEq)]
struct EmojiMenuTarget {
    shortcode: String,
    image: String,
    guild_id: Id,
    url: String,
}

/// What a right-click on a custom emoji opens, in place of the picker's
/// grid: save it under a name of your own, or drop it from your saved ones.
#[component]
fn EmojiSaveForm(target: EmojiMenuTarget, on_done: EventHandler<()>) -> Element {
    let mut state = use_app_state();
    let saved = state.read().saved_emoji_of(&target.image).cloned();
    let guild_name = state
        .read()
        .guilds
        .iter()
        .find(|g| g.id == target.guild_id)
        .map(|g| g.name.clone())
        .or_else(|| saved.as_ref().map(|s| s.guild_name.clone()))
        .unwrap_or_default();
    let mut name = use_signal(|| target.shortcode.clone());
    let mut error = use_signal(|| None::<String>);
    let image = target.image.clone();
    let image_rm = target.image.clone();
    rsx! {
        div { class: "w-56 p-1",
            div { class: "flex items-center gap-2 mb-2",
                if target.url.is_empty() {
                    span { class: "text-[8px] text-[var(--text-dim)]", "…" }
                } else {
                    img { src: "{target.url}", style: "height:1.6em;width:auto;" }
                }
                span { class: "font-mono text-xs text-[var(--text)] truncate", ":{target.shortcode}:" }
                span { class: "text-[10px] text-[var(--text-dim)] truncate", "{guild_name}" }
            }
            if let Some(s) = saved {
                div { class: "text-[10px] text-[var(--text-dim)] mb-1.5",
                    "Saved as :{s.shortcode}: — yours to use in DMs."
                }
                div { class: "flex gap-2",
                    button {
                        r#type: "button",
                        class: "text-[10px] uppercase tracking-wider text-[var(--danger)] border border-[var(--border)] rounded px-2 py-0.5 hover:border-[var(--danger)] transition-colors",
                        onclick: move |_| {
                            state.write().remove_saved_emoji(&image_rm);
                            on_done.call(());
                        },
                        "Remove from my emoji"
                    }
                    button {
                        r#type: "button",
                        class: "text-[10px] uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--text)] transition-colors",
                        onclick: move |_| on_done.call(()),
                        "Back"
                    }
                }
            } else {
                div { class: "text-[10px] text-[var(--text-dim)] mb-1.5",
                    "Save it to use in DMs, under a name of your own:"
                }
                div { class: "flex items-center gap-0.5 font-mono text-xs",
                    span { class: "text-[var(--text-dim)]", ":" }
                    input {
                        class: "flex-1 min-w-0 bg-transparent border border-[var(--border)] rounded px-1.5 py-0.5 text-xs text-[var(--text)] focus:outline-none focus:border-[var(--accent)]",
                        maxlength: crate::protocol::MAX_SHORTCODE_LEN as i64,
                        value: "{name}",
                        oninput: move |e| name.set(e.value()),
                    }
                    span { class: "text-[var(--text-dim)]", ":" }
                }
                if let Some(e) = error() {
                    div { class: "text-[10px] text-[var(--danger)] mt-1", "{e}" }
                }
                div { class: "flex gap-2 mt-1.5",
                    button {
                        r#type: "button",
                        class: "text-[10px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] rounded px-2 py-0.5 hover:border-[var(--accent)] transition-colors",
                        onclick: move |_| {
                            match state.write().save_emoji(&name(), &image, target.guild_id) {
                                Ok(()) => on_done.call(()),
                                Err(e) => error.set(Some(e)),
                            }
                        },
                        "Save to my emoji"
                    }
                    button {
                        r#type: "button",
                        class: "text-[10px] uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--text)] transition-colors",
                        onclick: move |_| on_done.call(()),
                        "Back"
                    }
                }
            }
        }
    }
}

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

/// 100% emoji size. Emoji glyphs draw smaller than their box, so this sits
/// above `1em`; the chat emoji slider scales it.
const EMOJI_EM: f64 = 1.8;

/// `reaction`: sized by the reaction slider, not the chat one, so a big chat
/// emoji does not blow up every pill under a message.
/// `extra`: what a DM carried for its own shortcodes (NIP-30), consulted
/// after the guild.
#[component]
pub(crate) fn EmojiText(
    text: String,
    guild_id: Option<Id>,
    #[props(default)] reaction: bool,
    #[props(default)] extra: Vec<(String, String)>,
) -> Element {
    let state = use_app_state();
    let settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let percent = if reaction {
        settings.read().reaction_size_percent
    } else {
        settings.read().emoji_size_percent
    };
    let emoji_scale = f64::from(percent.clamp(50, 250)) / 100.0;
    let base = EMOJI_EM;
    let parts: Vec<(String, Option<String>)> = {
        let s = state.read();
        crate::emoji::split_shortcodes(&text)
            .into_iter()
            .map(|p| match p {
                crate::emoji::Piece::Text(t) => (t.to_string(), None),
                crate::emoji::Piece::Shortcode(code) => {
                    let url = guild_id
                        .and_then(|g| s.emoji_image(g, code))
                        .map(str::to_string)
                        .or_else(|| {
                            extra
                                .iter()
                                .find(|(c, _)| c == code)
                                .map(|(_, u)| u.clone())
                        });
                    (code.to_string(), Some(url.unwrap_or_default()))
                }
            })
            .collect()
    };

    rsx! {
        for (body, emoji) in parts.into_iter() {
            match emoji {
                Some(url) if !url.is_empty() => rsx! {
                    img {
                        src: "{url}",
                        alt: ":{body}:",
                        title: ":{body}:",
                        style: "height:calc({base}em * {emoji_scale});width:auto;display:inline-block;vertical-align:-0.3em;",
                    }
                },
                Some(_) => rsx! { ":{body}:" },
                None => rsx! { UnicodeEmojiText { text: body, scale: emoji_scale } },
            }
        }
    }
}

#[component]
fn UnicodeEmojiText(text: String, scale: f64) -> Element {
    let base = EMOJI_EM;
    rsx! {
        for (part, emoji) in crate::emoji::unicode_parts(&text) {
            if emoji {
                span { style: "font-size:calc({base}em * {scale});", "{part}" }
            } else {
                "{part}"
            }
        }
    }
}

#[component]
fn MessageContent(content: String, channel_id: Id, message_id: Id) -> Element {
    let state = use_app_state();
    let guild_id = state
        .read()
        .channels
        .iter()
        .find(|c| c.id == channel_id)
        .map(|c| c.guild_id);
    let extra: Vec<(String, String)> = state
        .read()
        .dm_emoji
        .get(&message_id)
        .cloned()
        .unwrap_or_default();
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
                            } else if crate::emoji::needs_rendering(&w) {
                                rsx! { span { EmojiText { text: w.clone(), guild_id, extra: extra.clone() } "{trailing}" } }
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

/// The composer is a contenteditable so a custom emoji shows as its picture
/// while being typed. Listeners sit on the document and find the element by
/// id on every event, so a composer that is rebuilt (a channel locking and
/// unlocking) is picked up without rewiring. Serialising walks the children:
/// an `img[data-code]` is `:code:`, everything else is its text.
const COMPOSER_JS: &str = r#"
return (async function () {
  const id = '__ID__', mine = '__GEN__';
  window.__dxfComposers = window.__dxfComposers || {};
  if (window.__dxfComposers[id]) window.__dxfComposers[id].off();
  const find = () => document.getElementById(id);
  let suggesting = false, saved = null, alive = true;
  const serialize = (node) => {
    let out = '';
    node.childNodes.forEach(n => {
      if (n.nodeType === 3) out += n.nodeValue;
      else if (n.nodeType === 1) {
        if (n.tagName === 'IMG' && n.dataset.code) out += ':' + n.dataset.code + ':';
        else if (n.tagName !== 'BR') out += serialize(n);
      }
    });
    return out;
  };
  const caretIn = (el) => {
    const sel = window.getSelection();
    if (!sel || !sel.rangeCount) return null;
    const r = sel.getRangeAt(0);
    return el.contains(r.startContainer) ? r : null;
  };
  const beforeCaret = (el) => {
    const r = caretIn(el);
    if (!r) return null;
    const pre = document.createRange();
    pre.setStart(el, 0);
    pre.setEnd(r.startContainer, r.startOffset);
    return serialize(pre.cloneContents());
  };
  const report = () => {
    const el = find();
    if (!el || !alive) return;
    const text = serialize(el);
    if (text === '' && el.innerHTML !== '') el.innerHTML = '';
    dioxus.send({ k: 'input', text: text, before: beforeCaret(el) });
  };
  const remember = () => {
    const el = find();
    const r = el && caretIn(el);
    if (r) saved = r.cloneRange();
  };
  const restore = (el) => {
    el.focus();
    const sel = window.getSelection();
    sel.removeAllRanges();
    if (saved && el.contains(saved.startContainer)) sel.addRange(saved);
    else { const r = document.createRange(); r.selectNodeContents(el); r.collapse(false); sel.addRange(r); }
  };
  const tokenRange = (r) => {
    const n = r.startContainer;
    if (n.nodeType !== 3) return r;
    const m = /(^|\s):([A-Za-z0-9_]*)$/.exec(n.nodeValue.slice(0, r.startOffset));
    if (!m) return r;
    const t = document.createRange();
    t.setStart(n, r.startOffset - m[2].length - 1);
    t.setEnd(n, r.startOffset);
    return t;
  };
  const insert = (nodes, replaceToken) => {
    const el = find();
    if (!el) return;
    restore(el);
    const sel = window.getSelection();
    let r = sel.getRangeAt(0);
    if (!sel.isCollapsed) { r.deleteContents(); r.collapse(true); }
    if (replaceToken) { r = tokenRange(r); r.deleteContents(); }
    let last = null;
    for (const node of nodes) {
      if (last) r.setStartAfter(last); else r.collapse(true);
      r.insertNode(node);
      last = node;
    }
    if (last) { r.setStartAfter(last); r.collapse(true); }
    sel.removeAllRanges();
    sel.addRange(r);
    saved = r.cloneRange();
    report();
  };
  const api = {
    insertText: (text, replaceToken) => insert([document.createTextNode(text)], replaceToken),
    insertEmoji: (code, url, replaceToken, space) => {
      const img = document.createElement('img');
      img.src = url;
      img.alt = ':' + code + ':';
      img.title = ':' + code + ':';
      img.dataset.code = code;
      img.draggable = false;
      img.contentEditable = 'false';
      insert(space ? [img, document.createTextNode(' ')] : [img], replaceToken);
    },
    clear: () => { const el = find(); if (el) { el.innerHTML = ''; saved = null; report(); } },
    suggest: (on) => { suggesting = !!on; },
    focus: () => { const el = find(); if (el) restore(el); },
    off: (gen) => {
      if (gen && gen !== mine) return;
      alive = false;
      document.removeEventListener('input', onInput);
      document.removeEventListener('keydown', onKey);
      document.removeEventListener('paste', onPaste);
      document.removeEventListener('selectionchange', remember);
      if (window.__dxfComposers[id] === api) delete window.__dxfComposers[id];
    },
  };
  const within = (e) => { const el = find(); return el && e.target && el.contains(e.target) ? el : null; };
  const onInput = (e) => { if (within(e)) report(); };
  const onKey = (e) => {
    if (!within(e) || e.isComposing) return;
    if (e.key === 'Enter') { e.preventDefault(); dioxus.send({ k: suggesting ? 'pick' : 'submit' }); return; }
    if (!suggesting) return;
    if (e.key === 'ArrowUp' || e.key === 'ArrowDown') { e.preventDefault(); dioxus.send({ k: 'move', down: e.key === 'ArrowDown' }); }
    else if (e.key === 'Tab') { e.preventDefault(); dioxus.send({ k: 'pick' }); }
    else if (e.key === 'Escape') { e.preventDefault(); dioxus.send({ k: 'dismiss' }); }
  };
  const onPaste = (e) => {
    if (!within(e) || !e.clipboardData) return;
    for (const it of e.clipboardData.items || []) if (it.kind === 'file') return;
    e.preventDefault();
    const text = (e.clipboardData.getData('text/plain') || '').replace(/[\r\n]+/g, ' ');
    if (text) document.execCommand('insertText', false, text);
  };
  document.addEventListener('input', onInput);
  document.addEventListener('keydown', onKey);
  document.addEventListener('paste', onPaste);
  document.addEventListener('selectionchange', remember);
  window.__dxfComposers[id] = api;
  try {
    while (alive) {
      const m = await new Promise(resolve => dioxus.recv().then(resolve, () => resolve(null)));
      if (m === null) break;
    }
  } finally {
    api.off();
  }
})();
"#;

fn composer_call(id: &str, method: &str, args: &[Value]) {
    let args: Vec<String> = args.iter().map(Value::to_string).collect();
    let _ = document::eval(&format!(
        "(window.__dxfComposers || {{}})[{}]?.{method}({});",
        Value::String(id.to_string()),
        args.join(",")
    ));
}

#[derive(Clone, PartialEq)]
enum EmojiPick {
    Unicode(&'static str),
    Custom { code: String, url: String },
}

#[derive(Clone, PartialEq)]
struct EmojiSuggestion {
    name: String,
    pick: EmojiPick,
}

const MAX_SUGGESTIONS: usize = 8;

/// The `:ab…` the caret sits at the end of: two or more name characters
/// after a colon that starts a word, so a URL or a time never opens the list.
fn active_token(before: &str) -> Option<String> {
    let colon = before.rfind(':')?;
    let token = &before[colon + 1..];
    if token.len() < 2 || !token.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    before[..colon]
        .chars()
        .next_back()
        .is_none_or(char::is_whitespace)
        .then(|| token.to_ascii_lowercase())
}

/// Prefix matches before substring ones, this guild's emoji before the
/// built-in set within each.
fn emoji_suggestions(
    token: &str,
    guild: &[GuildEmoji],
    urls: &std::collections::HashMap<String, String>,
) -> Vec<EmojiSuggestion> {
    let mut out = Vec::new();
    for prefix in [true, false] {
        let hit = |name: &str| {
            if prefix {
                name.starts_with(token)
            } else {
                !name.starts_with(token) && name.contains(token)
            }
        };
        out.extend(
            guild
                .iter()
                .filter(|e| hit(&e.shortcode))
                .map(|e| EmojiSuggestion {
                    name: e.shortcode.clone(),
                    pick: EmojiPick::Custom {
                        code: e.shortcode.clone(),
                        url: urls.get(&e.image).cloned().unwrap_or_default(),
                    },
                }),
        );
        out.extend(
            EMOJIS
                .iter()
                .filter(|(_, name)| hit(name))
                .map(|(emoji, name)| EmojiSuggestion {
                    name: (*name).to_string(),
                    pick: EmojiPick::Unicode(emoji),
                }),
        );
        if out.len() >= MAX_SUGGESTIONS {
            break;
        }
    }
    out.truncate(MAX_SUGGESTIONS);
    out
}

/// The emoji a composer may offer: the guild's in a guild channel, the
/// person's saved ones in a DM. The urls are whatever the media cache holds
/// right now.
fn guild_emojis_of(
    s: &crate::state::AppState,
    channel_id: Id,
) -> (Vec<GuildEmoji>, std::collections::HashMap<String, String>) {
    let gid = s
        .channels
        .iter()
        .find(|c| c.id == channel_id)
        .map(|c| c.guild_id);
    let list = match gid {
        Some(g) => s.emojis_of(g).to_vec(),
        None if s.dm_of(channel_id).is_some() => s
            .saved_emoji
            .iter()
            .map(|e| GuildEmoji {
                id: e.guild_id,
                guild_id: e.guild_id,
                shortcode: e.shortcode.clone(),
                image: e.image.clone(),
                added_by: String::new(),
                created_ms: 0,
            })
            .collect(),
        None => Vec::new(),
    };
    let urls = list
        .iter()
        .filter_map(|e| {
            s.emoji_images
                .get(&e.image)
                .map(|u| (e.image.clone(), u.clone()))
        })
        .collect();
    (list, urls)
}

fn suggestions_now(
    s: &crate::state::AppState,
    channel_id: Id,
    token: &str,
) -> Vec<EmojiSuggestion> {
    let (list, urls) = guild_emojis_of(s, channel_id);
    emoji_suggestions(token, &list, &urls)
}

/// Puts the chosen emoji where the `:token` was. A custom emoji whose picture
/// has not arrived goes in as text, which still renders once sent.
fn insert_pick(id: &str, pick: &EmojiPick, replace_token: bool, space: bool) {
    let tail = if space { " " } else { "" };
    match pick {
        EmojiPick::Unicode(e) => composer_call(
            id,
            "insertText",
            &[
                Value::String(format!("{e}{tail}")),
                Value::Bool(replace_token),
            ],
        ),
        EmojiPick::Custom { code, url } if url.is_empty() => composer_call(
            id,
            "insertText",
            &[
                Value::String(format!(":{code}:{tail}")),
                Value::Bool(replace_token),
            ],
        ),
        EmojiPick::Custom { code, url } => composer_call(
            id,
            "insertEmoji",
            &[
                Value::String(code.clone()),
                Value::String(url.clone()),
                Value::Bool(replace_token),
                Value::Bool(space),
            ],
        ),
    }
}

/// Saved emoji used in `content` whose picture has no blob url yet and is
/// loaded: `(code, image, data url)`, each once.
fn uploads_needed(
    s: &crate::state::AppState,
    channel_id: Id,
    content: &str,
) -> Vec<(String, String, String)> {
    let (list, urls) = guild_emojis_of(s, channel_id);
    let mut out: Vec<(String, String, String)> = Vec::new();
    for piece in crate::emoji::split_shortcodes(content) {
        let crate::emoji::Piece::Shortcode(code) = piece else {
            continue;
        };
        let Some(e) = list.iter().find(|e| e.shortcode == code) else {
            continue;
        };
        if out.iter().any(|(_, image, _)| image == &e.image) {
            continue;
        }
        let saved = s.saved_emoji_of(&e.image);
        if saved.is_some_and(|saved| saved.blossom_url.is_some()) {
            continue;
        }
        if let Some(data) = urls.get(&e.image).filter(|d| d.starts_with("data:")) {
            out.push((code.to_string(), e.image.clone(), data.clone()));
        }
    }
    out
}

/// Builds the tags and hands the message to the Nostr service. True when it
/// went; the caller clears the composer.
fn send_dm(
    mut state: Signal<crate::state::AppState>,
    nostr: &crate::nostr::service::NostrTx,
    channel_id: Id,
    peer: String,
    content: String,
    reply_event: Option<String>,
) -> bool {
    let (emoji, skipped) = {
        let s = state.read();
        let (list, urls) = guild_emojis_of(&s, channel_id);
        crate::emoji::dm_emoji_tags(&content, |code| {
            let e = list.iter().find(|e| e.shortcode == code)?;
            s.saved_emoji_of(&e.image)
                .and_then(|saved| saved.blossom_url.clone())
                .or_else(|| urls.get(&e.image).cloned())
        })
    };
    if !skipped.is_empty() {
        let names: Vec<String> = skipped.iter().map(|c| format!(":{c}:")).collect();
        state.write().error_toast = Some(format!(
            "{} too large to send in a DM — sent as text.",
            names.join(", ")
        ));
    }
    if !nostr.try_send(crate::nostr::service::NostrCmd::Send {
        peer,
        text: content,
        reply_to: reply_event,
        emoji,
    }) {
        state.write().error_toast =
            Some("The message service is unavailable. Your draft has been kept.".into());
        return false;
    }
    true
}

#[component]
fn Composer(
    channel_id: Id,
    composer_label: String,
    mut dropped_file: Signal<Option<std::path::PathBuf>>,
) -> Element {
    let mut state = use_app_state();
    let replying_to = use_memo(move || {
        state
            .read()
            .replying_to
            .clone()
            .filter(|r| r.channel_id == channel_id)
    });
    let mut draft = use_signal(String::new);
    let mut pending_image = use_signal::<Option<String>>(|| None);
    let attach_err = use_signal::<Option<String>>(|| None);
    let mut show_emoji = use_signal(|| false);
    let mut show_file = use_signal(|| false);
    let mut caret_token = use_signal(|| None::<String>);
    let mut selected = use_signal(|| 0_usize);
    let mut last_typing = use_signal::<Option<std::time::Instant>>(|| None);
    let gateway = use_gateway();
    let gateway_submit = gateway.clone();
    let nostr_submit = use_context::<crate::nostr::service::NostrTx>();
    let settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let identity = use_context::<crate::identity::Identity>();
    let mut uploading = use_signal(|| false);
    let composer_id = format!("dxf-composer-{channel_id}");
    // Names this mount's script, so a drop that lands after the next mount's
    // script has started leaves that one alone.
    let instance = use_hook(|| uuid::Uuid::new_v4().to_string());
    {
        let id = composer_id.clone();
        let instance = instance.clone();
        use_drop(move || composer_call(&id, "off", &[Value::String(instance)]));
    }

    let mut generation = use_signal(|| 0_u64);
    use_effect(move || {
        if let Some(path) = dropped_file() {
            dropped_file.set(None);
            load_attachment(Some(path), pending_image, attach_err, generation);
        }
    });
    use_future(move || async move {
        let mut eval = document::eval(PASTE_JS);
        while let Ok(msg) = eval.recv::<Value>().await {
            if msg.get("k").and_then(Value::as_str) == Some("paste") {
                load_attachment(None, pending_image, attach_err, generation);
            }
        }
    });

    let is_dm = state.read().dm_of(channel_id).is_some();
    let file_guild = state
        .read()
        .channels
        .iter()
        .find(|channel| channel.id == channel_id)
        .map(|channel| channel.guild_id)
        .filter(|guild| {
            !is_dm
                && state.read().server_chat_tools
                && state
                    .read()
                    .can(*guild, crate::protocol::Permission::SendMessages)
        });
    let mut emoji_menu = use_signal(|| None::<EmojiMenuTarget>);
    use_saved_emoji_pictures();
    let (guild_emojis, emoji_urls) = guild_emojis_of(&state.read(), channel_id);
    let suggestions = caret_token()
        .as_deref()
        .map(|t| emoji_suggestions(t, &guild_emojis, &emoji_urls))
        .unwrap_or_default();
    let has_suggestions = use_memo(move || {
        caret_token()
            .as_deref()
            .is_some_and(|t| !suggestions_now(&state.read(), channel_id, t).is_empty())
    });
    {
        let id = composer_id.clone();
        use_effect(move || composer_call(&id, "suggest", &[Value::Bool(has_suggestions())]));
    }
    use_effect(move || {
        let _ = caret_token();
        selected.set(0);
    });

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

    let finish_id = composer_id.clone();
    let mut finish = move || -> bool {
        let reply_to = replying_to.peek().as_ref().map(|r| r.message_id);
        draft.set(String::new());
        caret_token.set(None);
        composer_call(&finish_id, "clear", &[]);
        generation.with_mut(|n| *n = n.wrapping_add(1));
        pending_image.set(None);
        show_emoji.set(false);
        if reply_to.is_some() {
            state.write().replying_to = None;
        }
        true
    };
    let mut submit = move || -> bool {
        let content = draft().trim().to_string();
        let image = pending_image();
        if content.is_empty() && image.is_none() {
            return false;
        }
        tracing::debug!(%channel_id, chars = content.len(), "composer submit");
        let reply_to = replying_to().map(|r| r.message_id);
        let dm_peer = state
            .read()
            .dm_of(channel_id)
            .map(|d| d.other_pubkey.clone());
        if let Some(peer) = dm_peer {
            if state
                .peek()
                .dm_delivery
                .values()
                .filter(|status| **status != crate::nostr::delivery::Delivery::Accepted)
                .count()
                >= 128
            {
                state.write().error_toast = Some("Too many messages await delivery. Your draft has been kept; retry a failed message first.".into());
                return false;
            }
            if image.is_some() {
                state.write().error_toast =
                    Some("Images in DMs are not supported yet — the text was not sent.".into());
                return false;
            }
            let reply_event =
                reply_to.and_then(|id| state.read().nostr_event_ids.get(&id).cloned());
            // With a blob server, a saved emoji's picture goes up once, on
            // first use, and only then does the message go — the tag needs
            // the url. Without one, or if the upload fails, inline as before.
            let pending = settings
                .peek()
                .blossom_server
                .clone()
                .map(|server| (server, uploads_needed(&state.peek(), channel_id, &content)))
                .filter(|(_, needed)| !needed.is_empty());
            let Some((server, needed)) = pending else {
                return send_dm(state, &nostr_submit, channel_id, peer, content, reply_event)
                    && finish();
            };
            if uploading() {
                return false;
            }
            uploading.set(true);
            let secret = identity.secret_key();
            let nostr = nostr_submit.clone();
            let mut finish = finish.clone();
            spawn(async move {
                for (code, image, data) in needed {
                    let now = chrono::Utc::now().timestamp();
                    match crate::nostr::blossom::upload(&server, &secret, &data, now).await {
                        Ok(url) => {
                            let mut s = state.write();
                            if let Some(e) = s.saved_emoji.iter_mut().find(|e| e.image == image) {
                                e.blossom_url = Some(url);
                            }
                        }
                        Err(why) => {
                            state.write().error_toast = Some(format!(
                                "Couldn't upload :{code}: — {why}. Sending it inside the message instead."
                            ));
                        }
                    }
                }
                uploading.set(false);
                if send_dm(state, &nostr, channel_id, peer, content, reply_event) {
                    finish();
                }
            });
            return false;
        } else {
            if state.peek().status != crate::state::ConnectionStatus::Ready {
                state.write().error_toast =
                    Some("The server is reconnecting. Your draft has been kept.".into());
                return false;
            }
            gateway_submit.send(ClientMessage::SendMessage {
                channel_id,
                content,
                image,
                reply_to,
            });
        }
        finish()
    };

    let gateway_typing = gateway.clone();
    let notify_typing = move || {
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

    let pick = {
        let id = composer_id.clone();
        move |i: usize| {
            let Some(token) = caret_token.peek().clone() else {
                return;
            };
            let list = suggestions_now(&state.peek(), channel_id, &token);
            if let Some(s) = list.get(i) {
                insert_pick(&id, &s.pick, true, true);
            }
            caret_token.set(None);
        }
    };
    {
        let id = composer_id.clone();
        let instance = instance.clone();
        let submit = submit.clone();
        let notify_typing = notify_typing.clone();
        let pick = pick.clone();
        use_future(move || {
            let js = COMPOSER_JS
                .replace("__ID__", &id)
                .replace("__GEN__", &instance);
            let mut submit = submit.clone();
            let mut notify_typing = notify_typing.clone();
            let mut pick = pick.clone();
            async move {
                let mut eval = document::eval(&js);
                while let Ok(msg) = eval.recv::<Value>().await {
                    match msg.get("k").and_then(Value::as_str) {
                        Some("input") => {
                            let text = msg.get("text").and_then(Value::as_str).unwrap_or_default();
                            let token = msg
                                .get("before")
                                .and_then(Value::as_str)
                                .and_then(active_token);
                            if *draft.peek() != text {
                                draft.set(text.to_string());
                                if !text.is_empty() {
                                    notify_typing();
                                }
                            }
                            if *caret_token.peek() != token {
                                caret_token.set(token);
                            }
                        }
                        Some("submit") => {
                            submit();
                        }
                        Some("pick") => pick(*selected.peek()),
                        Some("move") => {
                            let down = msg.get("down").and_then(Value::as_bool).unwrap_or(true);
                            let n = caret_token
                                .peek()
                                .as_deref()
                                .map(|t| suggestions_now(&state.peek(), channel_id, t).len())
                                .unwrap_or(0);
                            if n > 0 {
                                let cur = *selected.peek() % n;
                                selected.set(if down {
                                    (cur + 1) % n
                                } else {
                                    (cur + n - 1) % n
                                });
                            }
                        }
                        Some("dismiss") => caret_token.set(None),
                        _ => {}
                    }
                }
            }
        });
    }

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

            if show_emoji() {
                div {
                    class: "dxf-pop-in absolute bottom-full right-3 mb-2 p-1.5 bg-[var(--panel-solid)] border border-[var(--border)] rounded-md shadow-lg z-30",
                    if let Some(target) = emoji_menu() {
                        EmojiSaveForm { target, on_done: move |_| emoji_menu.set(None) }
                    } else {
                    if !guild_emojis.is_empty() {
                        div { class: "text-[9px] uppercase tracking-wider text-[var(--text-dim)] px-1 pb-1",
                            if is_dm { "Your emoji" } else { "This guild" }
                        }
                        div { class: "grid grid-cols-8 gap-0.5 pb-1.5 mb-1.5 border-b border-[var(--border)]",
                            for e in guild_emojis.iter().cloned() {
                                {
                                    let code = e.shortcode.clone();
                                    let url = emoji_urls.get(&e.image).cloned().unwrap_or_default();
                                    let id = composer_id.clone();
                                    let target = EmojiMenuTarget { shortcode: e.shortcode.clone(), image: e.image.clone(), guild_id: e.guild_id, url: url.clone() };
                                    rsx! {
                                        button {
                                            key: "{e.shortcode}",
                                            r#type: "button",
                                            class: "w-6 h-6 flex items-center justify-center rounded hover:bg-white/[0.06] text-base leading-none",
                                            title: ":{code}: — right-click to save or remove",
                                            onclick: move |_| {
                                                insert_pick(&id, &EmojiPick::Custom { code: code.clone(), url: url.clone() }, false, false);
                                                show_emoji.set(false);
                                            },
                                            oncontextmenu: move |ev: MouseEvent| {
                                                ev.prevent_default();
                                                emoji_menu.set(Some(target.clone()));
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
                        for (emoji, name) in EMOJIS.iter().copied() {
                            {
                                let id = composer_id.clone();
                                rsx! {
                                    button {
                                        r#type: "button",
                                        class: "w-6 h-6 flex items-center justify-center rounded hover:bg-white/[0.06] text-base leading-none",
                                        title: ":{name}:",
                                        onclick: move |_| {
                                            insert_pick(&id, &EmojiPick::Unicode(emoji), false, false);
                                            show_emoji.set(false);
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

            if !suggestions.is_empty() {
                div {
                    class: "dxf-pop-in absolute bottom-full left-3 right-3 mb-2 p-1 bg-[var(--panel-solid)] border border-[var(--border)] rounded-md shadow-lg z-30",
                    div { class: "text-[9px] uppercase tracking-wider text-[var(--text-dim)] px-2 pb-1",
                        "Emoji — ↑↓ to choose, Enter or Tab to insert"
                    }
                    for (i, s) in suggestions.iter().cloned().enumerate() {
                        {
                            let is_selected = i == selected() % suggestions.len().max(1);
                            let mut pick = pick.clone();
                            rsx! {
                                button {
                                    key: "{s.name}",
                                    r#type: "button",
                                    class: if is_selected {
                                        "w-full flex items-center gap-2 px-2 h-7 rounded bg-white/[0.08] text-xs text-[var(--text)] text-left"
                                    } else {
                                        "w-full flex items-center gap-2 px-2 h-7 rounded text-xs text-[var(--text-muted)] text-left hover:bg-white/[0.04]"
                                    },
                                    onmouseenter: move |_| selected.set(i),
                                    onmousedown: move |e| e.prevent_default(),
                                    onclick: move |_| pick(i),
                                    span { class: "w-6 flex items-center justify-center text-base leading-none shrink-0",
                                        match &s.pick {
                                            EmojiPick::Unicode(e) => rsx! { "{e}" },
                                            EmojiPick::Custom { url, .. } if !url.is_empty() => rsx! {
                                                img { src: "{url}", style: "height:1.2em;width:auto;" }
                                            },
                                            EmojiPick::Custom { .. } => rsx! { span { class: "text-[8px] text-[var(--text-dim)]", "…" } },
                                        }
                                    }
                                    span { class: "truncate", ":{s.name}:" }
                                }
                            }
                        }
                    }
                }
            }

            if let Some(img) = pending_image() {
                div { class: "mb-2 flex items-center gap-2",
                    img {
                        class: "h-16 w-16 object-cover rounded border border-[var(--border-strong)] bg-[var(--panel2)]",
                        src: "{img}",
                        alt: "pending attachment",
                    }
                    button {
                        r#type: "button",
                        class: "text-[10px] uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--danger)] transition-colors",
                        onclick: move |_| { generation.with_mut(|n| *n = n.wrapping_add(1)); pending_image.set(None); },
                        "Remove"
                    }
                }
            }
            if let Some(err) = attach_err() {
                div { class: "mb-2 text-[10px] text-[var(--danger)]", "{err}" }
            }

            if show_file() {
                if let Some(guild_id) = file_guild {
                    div { class: "mb-2 text-xs",
                        button { r#type: "button", onclick: move |_| show_file.set(false), "Close file attachment" }
                        super::chat_tools::FileSender { guild_id, channel_id }
                    }
                }
            }

            form {
                onsubmit: move |e| { e.prevent_default(); submit(); },
                div { class: "h-12 border border-[var(--border-strong)] rounded-xl bg-[var(--panel)] flex items-center pl-2 pr-2.5 gap-2 focus-within:border-[var(--accent)] transition-colors",

                    label {
                        class: "w-8 h-8 shrink-0 flex items-center justify-center rounded-lg border border-[var(--border)] bg-[var(--panel2)] text-lg leading-none text-[var(--text-muted)] hover:text-[var(--accent)] hover:border-[var(--accent)] transition-colors cursor-pointer select-none",
                        style: "width: auto; padding: 0 8px; font-size: 12px;",
                        title: "Upload an image with a preview in chat",
                        "🖼 Image"
                        input {
                            r#type: "file",
                            accept: "image/*",
                            class: "hidden",
                            onchange: move |evt: FormEvent| {
                                if let Some(file) = evt.files().into_iter().next() {
                                    load_attachment(Some(file.path()), pending_image, attach_err, generation);
                                }
                            },
                        }
                    }

                    if file_guild.is_some() {
                        button {
                            r#type: "button",
                            class: "h-8 shrink-0 flex items-center justify-center rounded-lg border border-[var(--border)] bg-[var(--panel2)] text-[var(--text-muted)] hover:text-[var(--accent)] hover:border-[var(--accent)] transition-colors",
                            style: "padding: 0 8px; font-size: 12px;",
                            title: "Upload a downloadable file without a preview",
                            aria_expanded: show_file(),
                            onclick: move |_| show_file.toggle(),
                            "📎 File"
                        }
                    }

                    // `min-w-0`: the field's intrinsic width is not zero, so
                    // without it a narrow composer pushes Send off the row.
                    // Its children are the webview's (COMPOSER_JS); nothing
                    // here renders into it.
                    div {
                        id: "{composer_id}",
                        class: "dxf-composer flex-1 min-w-0 py-2 text-[14px] text-[var(--text)]",
                        contenteditable: "true",
                        "data-placeholder": "Message {composer_label}",
                        role: "textbox",
                        aria_multiline: "false",
                        aria_label: "Message {composer_label}",
                    }

                    button {
                        r#type: "button",
                        class: "px-1.5 text-base leading-none text-[var(--text-muted)] hover:text-[var(--accent)] transition-colors",
                        title: "Emoji",
                        onclick: move |_| show_emoji.toggle(),
                        "🙂"
                    }

                    // Enter already sends; the button is for the pointer, and a
                    // permanently greyed one is a control that never does anything.
                    if !draft().trim().is_empty() || pending_image().is_some() {
                        button {
                            class: "dxf-cta shrink-0 text-xs font-semibold uppercase tracking-wider px-4 py-1.5 rounded-lg transition-all",
                            r#type: "submit",
                            "Send"
                        }
                    }
                }
                div { class: "flex gap-3.5 pt-1.5 px-1 font-mono text-[10px] text-[var(--text-dim)]",
                    // Only what the composer can actually do: the draft is a
                    // single-line input, so Shift+Enter submits like Enter.
                    span { "Enter send" }
                    span { ":name for emoji" }
                    if uploading() {
                        span { class: "text-[var(--accent)]", "Uploading emoji…" }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod composer_tests {
    use super::*;

    fn custom(code: &str) -> GuildEmoji {
        GuildEmoji {
            id: uuid::Uuid::new_v4(),
            guild_id: uuid::Uuid::new_v4(),
            shortcode: code.into(),
            image: format!("{code}.png"),
            added_by: String::new(),
            created_ms: 0,
        }
    }

    #[test]
    fn a_token_needs_two_name_characters_after_a_word_initial_colon() {
        assert_eq!(active_token("hi :lp"), Some("lp".into()));
        assert_eq!(active_token(":LP"), Some("lp".into()));
        assert_eq!(active_token("a :fire_"), Some("fire_".into()));
        assert_eq!(active_token("hi :l"), None, "one character is too eager");
        assert_eq!(active_token("http://x"), None, "no space before the colon");
        assert_eq!(active_token("at 10:30"), None);
        assert_eq!(
            active_token("done :tada: "),
            None,
            "a closed code is finished"
        );
        assert_eq!(active_token("x :a-b"), None);
    }

    #[test]
    fn suggestions_rank_this_guild_and_prefixes_first_and_stay_bounded() {
        let guild = vec![custom("fireball"), custom("campfire")];
        let urls =
            std::collections::HashMap::from([("fireball.png".to_string(), "data:x".to_string())]);
        let got = emoji_suggestions("fi", &guild, &urls);
        let names: Vec<&str> = got.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["fireball", "fire", "campfire"]);
        assert!(matches!(&got[0].pick, EmojiPick::Custom { url, .. } if url == "data:x"));
        assert!(matches!(&got[2].pick, EmojiPick::Custom { url, .. } if url.is_empty()));
        assert!(matches!(got[1].pick, EmojiPick::Unicode("🔥")));
        assert!(emoji_suggestions("zzzz", &guild, &urls).is_empty());
        assert_eq!(emoji_suggestions("_", &guild, &urls).len(), MAX_SUGGESTIONS);
    }

    #[test]
    fn composer_calls_quote_their_arguments_for_the_webview() {
        // The id and every argument are JSON, so a quote or a backslash in a
        // shortcode cannot escape the call.
        let args = [Value::String("a\"b".into()), Value::Bool(true)];
        let rendered: Vec<String> = args.iter().map(Value::to_string).collect();
        assert_eq!(rendered.join(","), r#""a\"b",true"#);
    }
}
