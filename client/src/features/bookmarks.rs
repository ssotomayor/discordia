use dioxus::prelude::*;

use crate::bookmarks::{Bookmarks, Conversation};
use crate::state::use_app_state;

#[derive(Clone, Copy)]
struct Panel(Signal<bool>);

pub(super) fn use_panel() {
    let open = use_signal(|| false);
    use_context_provider(|| Panel(open));
}

#[component]
pub(super) fn BookmarksButton() -> Element {
    let panel = try_consume_context::<Panel>();
    if let Some(Panel(mut open)) = panel {
        rsx! {
            button {
                r#type: "button",
                class: "w-8 h-8 shrink-0 flex items-center justify-center rounded-lg border border-[var(--border)] bg-[var(--panel2)] text-[var(--text-muted)] hover:text-[var(--accent)]",
                title: crate::i18n::text("Saved messages", &[]),
                aria_label: crate::i18n::text("Saved messages", &[]),
                onclick: move |_| open.set(true),
                span { dangerous_inner_html: super::icons::BOOKMARK }
            }
        }
    } else {
        rsx! {}
    }
}

#[component]
pub(super) fn BookmarksDialog() -> Element {
    let bookmarks = try_consume_context::<Bookmarks>();
    let panel = try_consume_context::<Panel>();
    let app = use_app_state();
    let mut selected = use_signal(|| None::<Conversation>);
    let Some(bookmarks) = bookmarks else {
        return rsx! {};
    };
    let Some(Panel(mut open)) = panel else {
        return rsx! {};
    };
    if !open() {
        return rsx! {};
    }
    let state = bookmarks.state.read();
    let mut groups: Vec<(Conversation, String, usize)> = Vec::new();
    for message in &state.messages {
        if let Some(group) = groups
            .iter_mut()
            .find(|(key, _, _)| *key == message.conversation)
        {
            group.2 += 1;
        } else {
            let label = match &message.conversation {
                Conversation::Dm { peer } => format!("@{}", app.read().display_name(peer)),
                Conversation::Channel { .. } => {
                    format!("{} · #{}", message.guild_name, message.channel_name)
                }
            };
            groups.push((message.conversation.clone(), label, 1));
        }
    }
    let active = selected()
        .filter(|key| groups.iter().any(|(group, _, _)| group == key))
        .or_else(|| groups.first().map(|(key, _, _)| key.clone()));
    let messages: Vec<_> = state
        .messages
        .iter()
        .filter(|m| Some(&m.conversation) == active.as_ref())
        .cloned()
        .collect();
    let busy = state.busy;
    let error = state.error;
    let loaded = state.loaded;
    drop(state);
    rsx! {
        style { {include_str!("../../assets/bookmarks.css")} }
        div {
            class: "dxf-bookmarks-backdrop",
            onclick: move |_| open.set(false),
            onkeydown: move |e: KeyboardEvent| { if e.key() == Key::Escape { open.set(false); } },
            section {
                class: "dxf-bookmarks-dialog", role: "dialog", aria_modal: "true",
                aria_label: crate::i18n::text("Saved messages", &[]),
                onclick: move |e: MouseEvent| e.stop_propagation(),
                header {
                    div {
                        h2 { {crate::i18n::text("Saved messages", &[])} }
                        p { {crate::i18n::text("Personal copies on this device, grouped by conversation. Only your account can see them.", &[])} }
                    }
                    button { r#type: "button", autofocus: true,
                        aria_label: crate::i18n::text("Close", &[]), onclick: move |_| open.set(false), "✕" }
                }
                if error {
                    div { class: "dxf-bookmarks-error", role: "status",
                        {crate::i18n::text("Could not access your saved messages. Try again.", &[])}
                        button { disabled: busy, onclick: { let store = bookmarks.clone(); move |_| store.reload() }, {crate::i18n::text("Retry", &[])} }
                    }
                }
                if !loaded && !error { p { class: "dxf-bookmarks-empty", {crate::i18n::text("Loading saved messages…", &[])} } }
                else if groups.is_empty() && !error { p { class: "dxf-bookmarks-empty", {crate::i18n::text("No saved messages yet. Right-click a message to save it.", &[])} } }
                else {
                    div { class: "dxf-bookmarks-body",
                        nav { aria_label: crate::i18n::text("Conversations", &[]),
                            for (key, label, count) in groups {
                                button { key: "{key:?}", r#type: "button", class: if Some(&key) == active.as_ref() { "active" } else { "" },
                                    title: match &key { Conversation::Channel { origin, .. } => origin.clone(), Conversation::Dm { .. } => label.clone() },
                                    onclick: move |_| selected.set(Some(key.clone())),
                                    span { "{label}" } small { "{count}" }
                                }
                            }
                        }
                        div { class: "dxf-bookmarks-messages",
                            for message in messages {
                                {
                                    let author = app.read().display_name(&message.author);
                                    let timestamp = message.created_at.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string();
                                    let image = message.image.as_ref().and_then(|image| app.read().media_src(image).map(str::to_owned));
                                    let store = bookmarks.clone();
                                    let conversation = message.conversation.clone();
                                    rsx! {
                                        article { key: "{message.id}", "data-context-message": "{message.content}",
                                            div { class: "dxf-bookmarks-meta", strong { "{author}" } time { "{timestamp}" }
                                                button { r#type: "button", disabled: busy || !loaded,
                                                    title: crate::i18n::text("Remove bookmark", &[]),
                                                    aria_label: crate::i18n::text("Remove bookmark", &[]),
                                                    "data-context-action": "bookmark",
                                                    onclick: move |_| store.remove(conversation.clone(), message.id), "✕" }
                                            }
                                            p { class: "dxf-bookmarks-text", "{message.content}" }
                                            if let Some(image) = image {
                                                img { src: "{image}", alt: crate::i18n::text("attachment", &[]),
                                                    onclick: move |_| { open.set(false); app.clone().write().image_viewer = Some(image.clone()); } }
                                            } else if message.image.is_some() { p { {crate::i18n::text("Image unavailable", &[])} } }
                                            if let Some(attachment) = &message.attachment { p { class: "dxf-bookmarks-file", "📎 {attachment.name}" } }
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
