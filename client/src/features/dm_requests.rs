use dioxus::prelude::*;

use crate::nostr::service::{NostrCmd, NostrTx};
use crate::state::{DmStanding, use_app_state};

const ACTION: &str = "flex-1 py-1 rounded-md border text-[11px] font-semibold transition-colors";

/// Conversations from keys we share nothing with (#311). Opening one marks it
/// read but accepts nothing; replying, or Accept, does.
#[component]
pub fn RequestList() -> Element {
    let mut state = use_app_state();
    let nostr = use_context::<NostrTx>();

    let snapshot = state.read();
    let selected = snapshot.selected_channel;
    let rows: Vec<(crate::state::DmInfo, String, Option<String>)> = snapshot
        .dms_by_recency(DmStanding::Request)
        .into_iter()
        .map(|dm| {
            let name = snapshot.display_name(&dm.other_pubkey);
            let preview = snapshot
                .dm_last_message(dm.channel_id)
                .map(|m| m.content.clone());
            (dm, name, preview)
        })
        .collect();
    drop(snapshot);

    rsx! {
        if rows.is_empty() {
            div { class: "px-2.5 py-3 text-[12.5px] text-[var(--text-dim)] leading-relaxed text-pretty",
                "No requests. Messages from people you share no server with, and have not added, wait here."
            }
        }
        for (dm, name, preview) in rows {
            {
                let cid = dm.channel_id;
                let cls = if selected == Some(cid) {
                    "bg-[var(--panel2)]"
                } else {
                    "hover:bg-[var(--bg2)]"
                };
                let (accept, ignore, block) = (
                    dm.other_pubkey.clone(),
                    dm.other_pubkey.clone(),
                    dm.other_pubkey.clone(),
                );
                let nostr = nostr.clone();
                rsx! {
                    div {
                        key: "rq-{cid}",
                        class: "px-2.5 py-2 rounded-[10px] transition-colors {cls}",
                        button {
                            class: "w-full flex items-center gap-2.5 text-left",
                            onclick: move |_| {
                                let mut s = state.write();
                                s.dm_mode = true;
                                s.selected_channel = Some(cid);
                                s.mark_dm_read(cid);
                            },
                            crate::features::profiles::Avatar {
                                pubkey: dm.other_pubkey.clone(),
                                name: name.clone(),
                                size: "w-[34px] h-[34px]",
                                text: "text-xs",
                            }
                            span { class: "flex-1 min-w-0",
                                span { class: "block truncate text-[12.5px] font-medium text-[var(--text)]", "{name}" }
                                if let Some(p) = preview {
                                    span { class: "block truncate mt-0.5 text-[12.5px] text-[var(--text-muted)]", "{p}" }
                                }
                            }
                        }
                        div { class: "mt-2 flex gap-1.5",
                            button {
                                class: "{ACTION} border-[var(--accent)] text-[var(--accent)]",
                                title: "Move this conversation to your messages.",
                                onclick: move |_| state.write().accept_dm(&accept),
                                "Accept"
                            }
                            button {
                                class: "{ACTION} border-[var(--border-strong)] text-[var(--text-muted)] hover:text-[var(--text)]",
                                title: "Hide this request on this machine.",
                                onclick: move |_| state.write().ignore_dm(&ignore),
                                "Ignore"
                            }
                            button {
                                class: "{ACTION} border-[var(--danger)] text-[var(--danger)]",
                                title: "Mute this key in your public Nostr mute list. Its messages are dropped.",
                                onclick: move |_| nostr.send(NostrCmd::SetMute { peer: block.clone(), mute: true }),
                                "Block"
                            }
                        }
                    }
                }
            }
        }
    }
}
