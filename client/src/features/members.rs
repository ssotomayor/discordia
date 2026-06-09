use dioxus::prelude::*;
use dioxus_grid_layout::NoDrag;

use crate::protocol::{Id, Member, VoiceState};
use crate::state::use_app_state;

#[component]
pub fn MembersPanel() -> Element {
    let state = use_app_state();
    let snapshot = state.read();

    let members: Vec<Member> = snapshot
        .selected_guild
        .map(|gid| {
            snapshot
                .members_of(gid)
                .into_iter()
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let voice_states: Vec<VoiceState> = snapshot.voice_states.clone();
    drop(snapshot);

    let online_count = members.iter().filter(|m| m.online).count();
    let offline_count = members.len() - online_count;

    rsx! {
        aside { class: "panel-hover w-full h-full bg-[var(--panel)] border border-[var(--border)] rounded-lg flex flex-col overflow-hidden",
            // Header — drag surface in edit mode.
            div { class: "h-11 px-3 flex items-center border-b border-[var(--border)]",
                h2 { class: "text-sm text-[var(--accent)] truncate font-medium", "Members" }
                span { class: "ml-auto text-[10px] text-[var(--text-dim)] uppercase tracking-wider",
                    "{online_count} online"
                }
            }
            NoDrag {
                div { class: "flex-1 overflow-y-auto py-3 space-y-3",
                    if online_count > 0 {
                        Section {
                            label: format!("Online — {online_count}"),
                            members: members.iter().filter(|m| m.online).cloned().collect::<Vec<_>>(),
                            voice_states: voice_states.clone(),
                        }
                    }
                    if offline_count > 0 {
                        Section {
                            label: format!("Offline — {offline_count}"),
                            members: members.iter().filter(|m| !m.online).cloned().collect::<Vec<_>>(),
                            voice_states: Vec::new(),
                        }
                    }
                    if members.is_empty() {
                        div { class: "px-4 text-xs text-[var(--text-dim)]", "No members" }
                    }
                }
            }
        }
    }
}

#[component]
fn Section(label: String, members: Vec<Member>, voice_states: Vec<VoiceState>) -> Element {
    rsx! {
        div {
            div { class: "px-3 mb-1 text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)]",
                "{label}"
            }
            for m in members.iter() {
                {
                    let vs = voice_states
                        .iter()
                        .find(|v| v.user_pubkey == m.user.pubkey)
                        .cloned();
                    rsx! {
                        MemberRow { key: "{m.user.pubkey}", member: m.clone(), voice: vs }
                    }
                }
            }
        }
    }
}

#[component]
fn MemberRow(member: Member, voice: Option<VoiceState>) -> Element {
    let initial = member
        .user
        .username
        .chars()
        .next()
        .unwrap_or('?')
        .to_ascii_uppercase();
    let name_class = if member.online {
        "text-[var(--text)]"
    } else {
        "text-[var(--text-dim)]"
    };
    let avatar_class = if member.online {
        "text-[var(--accent)] border-[var(--border)]"
    } else {
        "text-[var(--text-dim)] border-[var(--border)] opacity-60"
    };
    let speaking = voice.as_ref().map(|v| v.speaking).unwrap_or(false);
    let speaking_ring = if speaking {
        "ring-1 ring-[var(--accent)]"
    } else {
        ""
    };

    rsx! {
        div { class: "flex items-center gap-2 px-3 py-1 hover:bg-white/[0.02] cursor-pointer",
            div { class: "w-7 h-7 rounded-md border flex items-center justify-center text-xs font-medium {avatar_class} {speaking_ring}",
                "{initial}"
            }
            span { class: "text-sm truncate flex-1 {name_class}", "{member.user.username}" }
            if let Some(vs) = voice {
                VoiceBadges { vs: vs }
            }
        }
    }
}

#[component]
fn VoiceBadges(vs: VoiceState) -> Element {
    rsx! {
        div { class: "flex items-center gap-1 text-[10px] uppercase tracking-wider",
            if vs.channel_id.is_some() {
                span { class: "text-[var(--accent)]", title: "In voice", "v" }
            }
            if vs.muted {
                span { class: "text-[var(--text-dim)]", title: "Muted", "m" }
            }
        }
    }
}

#[allow(dead_code)]
fn id_label(id: Id) -> String {
    id.to_string().chars().take(4).collect()
}
