use dioxus::prelude::*;

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
        div { class: "flex flex-col h-full bg-[#313338] min-h-0",
            // Channel header
            header { class: "h-12 px-4 flex items-center gap-3 border-b border-black/30 shadow-sm shrink-0",
                span { class: "text-xl text-gray-400 font-bold", "#" }
                span { class: "font-bold text-white", "{channel_name}" }
                if let Some(topic) = channel_topic {
                    div { class: "border-l border-white/10 h-5 mx-1" }
                    span { class: "text-sm text-gray-400 truncate", "{topic}" }
                }
            }

            // Message list
            div { class: "flex-1 overflow-y-auto px-4 py-4 space-y-3 min-h-0",
                if messages.is_empty() && selected_channel.is_some() {
                    div { class: "h-full flex items-center justify-center text-gray-500 text-sm",
                        "No messages yet. Say something."
                    }
                } else {
                    for msg in messages.iter() {
                        MessageRow { key: "{msg.id}", message: msg.clone() }
                    }
                }
            }

            // Composer
            if let Some(channel_id) = selected_channel {
                Composer { channel_id, channel_name }
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
        div { class: "flex gap-3 hover:bg-white/[0.02] -mx-4 px-4 py-1 group",
            div { class: "w-10 h-10 rounded-full bg-indigo-600 flex items-center justify-center text-sm font-bold text-white shrink-0 mt-0.5",
                "{initial}"
            }
            div { class: "flex-1 min-w-0",
                div { class: "flex items-baseline gap-2",
                    span { class: "font-semibold text-white", "{message.author.username}" }
                    span { class: "text-xs text-gray-500", "{timestamp}" }
                }
                div { class: "text-gray-200 break-words whitespace-pre-wrap", "{message.content}" }
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
            class: "px-4 pb-4 shrink-0",
            onsubmit: move |e| { e.prevent_default(); submit(); },
            div { class: "bg-[#383a40] rounded-lg flex items-center px-4",
                input {
                    class: "flex-1 bg-transparent py-3 text-gray-100 placeholder-gray-500 focus:outline-none",
                    r#type: "text",
                    placeholder: "Message #{channel_name}",
                    value: "{draft}",
                    oninput: move |e| draft.set(e.value()),
                }
                button {
                    class: "text-gray-400 hover:text-white text-sm font-semibold px-2 disabled:opacity-30",
                    r#type: "submit",
                    disabled: draft().trim().is_empty(),
                    "Send"
                }
            }
        }
    }
}
