use dioxus::prelude::*;
use dioxus_grid_layout::NoDrag;

use crate::identity::discriminator;
use crate::protocol::{ClientMessage, Id, Message};
use crate::state::{use_app_state, use_gateway};

#[component]
pub fn ChatView() -> Element {
    let state = use_app_state();

    let snapshot = state.read();
    let selected_channel = snapshot.selected_channel;
    let channel_meta = selected_channel
        .and_then(|cid| snapshot.channels.iter().find(|c| c.id == cid).cloned());
    let messages: Vec<Message> = selected_channel
        .and_then(|cid| snapshot.messages.get(&cid).cloned())
        .unwrap_or_default();
    drop(snapshot);

    let channel_name = channel_meta
        .as_ref()
        .map(|c| c.name.clone())
        .unwrap_or_else(|| "no-channel".into());
    let channel_topic = channel_meta.as_ref().and_then(|c| c.topic.clone());

    rsx! {
        div { class: "flex flex-col h-full min-h-0",
            header { class: "h-11 px-3 flex items-center gap-3 border-b border-[var(--border)] shrink-0",
                span { class: "text-[var(--text-dim)] font-medium", "#" }
                span { class: "text-sm text-[var(--accent)] font-medium", "{channel_name}" }
                if let Some(topic) = channel_topic {
                    span { class: "text-[var(--text-dim)]", "·" }
                    span { class: "text-xs text-[var(--text-muted)] truncate", "{topic}" }
                }
            }

            NoDrag {
                div { class: "flex-1 overflow-y-auto px-4 py-4 space-y-2 min-h-0",
                    if messages.is_empty() && selected_channel.is_some() {
                        div { class: "h-full flex items-center justify-center text-[var(--text-dim)] text-xs",
                            "No messages yet."
                        }
                    } else {
                        for msg in messages.iter() {
                            MessageRow { key: "{msg.id}", message: msg.clone() }
                        }
                    }
                }

                if let Some(channel_id) = selected_channel {
                    Composer { channel_id, channel_name }
                }
            }
        }
    }
}

#[component]
fn MessageRow(message: Message) -> Element {
    let initial = message
        .author
        .username
        .chars()
        .next()
        .unwrap_or('?')
        .to_ascii_uppercase();
    let timestamp = message.created_at.format("%H:%M").to_string();

    rsx! {
        div { class: "flex gap-3 -mx-4 px-4 py-1 hover:bg-white/[0.02]",
            div { class: "w-8 h-8 rounded-md border border-[var(--border)] flex items-center justify-center text-xs text-[var(--accent)] font-medium shrink-0 mt-0.5",
                "{initial}"
            }
            div { class: "flex-1 min-w-0",
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
                div { class: "text-sm text-[var(--text)] break-words whitespace-pre-wrap leading-relaxed", "{message.content}" }
            }
        }
    }
}

#[component]
fn Composer(channel_id: Id, channel_name: String) -> Element {
    let mut draft = use_signal(String::new);
    let gateway = use_gateway();

    let mut submit = move || {
        let content = draft().trim().to_string();
        if content.is_empty() {
            return;
        }
        gateway.send(ClientMessage::SendMessage {
            channel_id,
            content,
        });
        draft.set(String::new());
    };

    rsx! {
        form {
            class: "px-3 pb-3 shrink-0",
            onsubmit: move |e| { e.prevent_default(); submit(); },
            div { class: "border border-[var(--border)] rounded flex items-center px-3 focus-within:border-[var(--accent)] transition-colors",
                input {
                    class: "flex-1 bg-transparent py-2 text-sm text-[var(--text)] focus:outline-none",
                    r#type: "text",
                    placeholder: "Message #{channel_name}",
                    value: "{draft}",
                    oninput: move |e| draft.set(e.value()),
                }
                button {
                    class: "text-xs text-[var(--text-muted)] hover:text-[var(--accent)] font-medium uppercase tracking-wider px-2 disabled:opacity-30 transition-colors",
                    r#type: "submit",
                    disabled: draft().trim().is_empty(),
                    "Send"
                }
            }
        }
    }
}
