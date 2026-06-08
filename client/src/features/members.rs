use dioxus::prelude::*;

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
        aside { class: "w-60 shrink-0 bg-[#2b2d31] border-l border-black/20 flex flex-col",
            div { class: "flex-1 overflow-y-auto py-4 space-y-3",
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
                    div { class: "px-4 text-sm text-gray-500", "No members" }
                }
            }
        }
    }
}

#[component]
fn Section(label: String, members: Vec<Member>, voice_states: Vec<VoiceState>) -> Element {
    rsx! {
        div {
            div { class: "px-3 mb-1 text-xs font-bold uppercase tracking-wide text-gray-400",
                "{label}"
            }
            for m in members.iter() {
                {
                    let vs = voice_states
                        .iter()
                        .find(|v| v.user_id == m.user.id)
                        .cloned();
                    rsx! {
                        MemberRow { key: "{m.user.id}", member: m.clone(), voice: vs }
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
    let avatar_opacity = if member.online { "opacity-100" } else { "opacity-50" };
    let dot_class = if member.online { "bg-green-500" } else { "bg-gray-500" };
    let name_class = if member.online { "text-gray-200" } else { "text-gray-500" };
    let speaking = voice.as_ref().map(|v| v.speaking).unwrap_or(false);
    let speaking_ring = if speaking { "ring-2 ring-emerald-400" } else { "" };

    rsx! {
        div { class: "flex items-center gap-2 px-3 py-1.5 mx-1 rounded hover:bg-white/5 cursor-pointer",
            div { class: "relative {avatar_opacity}",
                div { class: "w-8 h-8 rounded-full bg-indigo-600 flex items-center justify-center text-xs font-bold text-white {speaking_ring}",
                    "{initial}"
                }
                span { class: "absolute -bottom-0.5 -right-0.5 w-3 h-3 rounded-full border-2 border-[#2b2d31] {dot_class}" }
            }
            span { class: "text-sm font-medium truncate flex-1 {name_class}", "{member.user.username}" }
            if let Some(vs) = voice {
                VoiceBadges { vs: vs }
            }
        }
    }
}

#[component]
fn VoiceBadges(vs: VoiceState) -> Element {
    rsx! {
        div { class: "flex items-center gap-1 text-xs",
            if vs.channel_id.is_some() {
                span { class: "text-emerald-400", title: "In voice", "🔊" }
            }
            if vs.muted {
                span { class: "text-red-300", title: "Muted", "🔇" }
            }
        }
    }
}

#[allow(dead_code)]
fn id_label(id: Id) -> String {
    id.to_string().chars().take(4).collect()
}
