//! The guild settings section for experience: what earns it, where, and what
//! the results are called.
//!
//! Its own file because the dialog that holds it is already long, and its own
//! *component* rather than its own dialog because everything in there saves
//! through one button — so the draft lives in the parent and this only edits it.

use dioxus::prelude::*;

use crate::protocol::{Channel, ChannelKind, Id, LevelTier, Leveling, MemberSort};
use crate::state::use_app_state;

const FIELD: &str = "w-16 bg-transparent border border-[var(--edge)] focus:border-[var(--accent)] rounded px-2 py-1 text-xs text-[var(--text)] outline-none transition-colors";
const HEADING: &str =
    "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)] mb-1.5";
const HINT: &str = "text-[10px] text-[var(--text-dim)] mt-1";

/// `draft` is the parent's, so the dialog's one Save sees every edit made here.
#[component]
pub fn LevelingEditor(guild_id: Id, draft: Signal<Leveling>) -> Element {
    let state = use_app_state();

    let channels: Vec<Channel> = {
        let s = state.read();
        let mut v: Vec<Channel> = s
            .channels
            .iter()
            .filter(|c| c.guild_id == guild_id)
            .cloned()
            .collect();
        v.sort_by(|a, b| {
            a.position
                .cmp(&b.position)
                .then_with(|| a.name.cmp(&b.name))
        });
        v
    };

    let enabled = draft.read().enabled;
    let sort_value = match draft.read().member_sort {
        MemberSort::Name => "name",
        MemberSort::Level => "level",
    };

    rsx! {
        div { class: "border-t border-[var(--border)] pt-3",
            div { class: HEADING, "Levels" }

            label { class: "flex items-center gap-2 text-xs text-[var(--text)] cursor-pointer select-none",
                input {
                    r#type: "checkbox",
                    checked: enabled,
                    onchange: move |e| draft.write().enabled = e.checked(),
                }
                span { "Members earn experience here" }
            }

            if enabled {
                div { class: "mt-3",
                    div { class: HEADING, "Earned by" }
                    AmountRow {
                        label: "Sending a message",
                        value: draft.read().per_message,
                        onset: move |v| draft.write().per_message = v,
                    }
                    AmountRow {
                        label: "Adding a reaction",
                        value: draft.read().per_reaction,
                        onset: move |v| draft.write().per_reaction = v,
                    }
                    AmountRow {
                        label: "Each minute in voice",
                        value: draft.read().per_voice_minute,
                        onset: move |v| draft.write().per_voice_minute = v,
                    }
                    div { class: "flex items-center gap-2 mt-1.5",
                        span { class: "text-xs text-[var(--text-muted)] flex-1", "Cooldown between awards" }
                        input {
                            class: FIELD,
                            r#type: "number",
                            min: "0",
                            max: "{crate::protocol::MAX_XP_COOLDOWN}",
                            value: "{draft.read().cooldown_secs}",
                            oninput: move |e| {
                                let v = e.value().trim().parse::<u32>().unwrap_or(0);
                                draft.write().cooldown_secs = v.min(crate::protocol::MAX_XP_COOLDOWN);
                            },
                        }
                        span { class: "text-xs text-[var(--text-muted)] w-16", "seconds" }
                    }
                    div { class: HINT,
                        "Zero pays out every time. Voice ignores this — a minute is already the gap."
                    }
                }

                div { class: "mt-3",
                    div { class: HEADING, "Earned in" }
                    if channels.is_empty() {
                        div { class: "text-xs text-[var(--text-dim)]", "No channels yet." }
                    }
                    div { class: "max-h-32 overflow-y-auto",
                        for c in channels.iter().cloned() {
                            {
                                let cid = c.id;
                                let picked = draft.read().channels.contains(&cid);
                                rsx! {
                                    label {
                                        key: "{cid}",
                                        class: "flex items-center gap-2 py-0.5 text-xs text-[var(--text)] cursor-pointer select-none",
                                        input {
                                            r#type: "checkbox",
                                            checked: picked,
                                            onchange: move |_| {
                                                let mut d = draft.write();
                                                match d.channels.iter().position(|x| *x == cid) {
                                                    Some(i) => { d.channels.remove(i); }
                                                    None => d.channels.push(cid),
                                                }
                                            },
                                        }
                                        span { class: "truncate",
                                            if matches!(c.kind, ChannelKind::Voice) { "🔊 " } else { "# " }
                                            "{c.name}"
                                        }
                                    }
                                }
                            }
                        }
                    }
                    div { class: HINT,
                        if draft.read().channels.is_empty() {
                            "Nothing ticked means every channel earns."
                        } else {
                            "Only the ticked channels earn."
                        }
                    }
                }

                TierEditor { draft }

                div { class: "mt-3",
                    div { class: HEADING, "Member list order" }
                    select {
                        class: "w-full bg-[var(--panel-solid)] border border-[var(--border)] focus:border-[var(--accent)] rounded px-2 py-1 text-xs text-[var(--text)] outline-none",
                        value: "{sort_value}",
                        onchange: move |e| {
                            draft.write().member_sort = match e.value().as_str() {
                                "level" => MemberSort::Level,
                                _ => MemberSort::Name,
                            };
                        },
                        option { value: "name", "By name" }
                        option { value: "level", "Most experienced first" }
                    }
                    div { class: HINT,
                        "Applies inside each group. Whoever is online or in voice still comes first."
                    }
                }
            }
        }
    }
}

#[component]
fn AmountRow(label: &'static str, value: u32, onset: EventHandler<u32>) -> Element {
    rsx! {
        div { class: "flex items-center gap-2 mt-1.5",
            span { class: "text-xs text-[var(--text-muted)] flex-1", "{label}" }
            input {
                class: FIELD,
                r#type: "number",
                min: "0",
                max: "{crate::protocol::MAX_XP_PER_ACTION}",
                value: "{value}",
                oninput: move |e| {
                    let v = e.value().trim().parse::<u32>().unwrap_or(0);
                    onset.call(v.min(crate::protocol::MAX_XP_PER_ACTION));
                },
            }
            span { class: "text-xs text-[var(--text-muted)] w-16", "xp" }
        }
    }
}

/// Rows are held in the order they were added and only sorted on save — sorting
/// as someone types turns a half-entered threshold into a jump up the list.
#[component]
fn TierEditor(draft: Signal<Leveling>) -> Element {
    let tiers = draft.read().tiers.clone();
    let full = tiers.len() >= crate::protocol::MAX_TIERS;

    rsx! {
        div { class: "mt-3",
            div { class: "flex items-center gap-2 mb-1.5",
                div { class: "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)] flex-1",
                    "Rank names"
                }
                div { class: "text-[10px] text-[var(--text-dim)]",
                    "{tiers.len()}/{crate::protocol::MAX_TIERS}"
                }
            }
            if tiers.is_empty() {
                div { class: "text-xs text-[var(--text-dim)] mb-1.5",
                    "No names yet — members show as Lv1, Lv2 and so on."
                }
            }
            for (i, t) in tiers.iter().cloned().enumerate() {
                div { key: "{i}", class: "flex items-center gap-2 py-0.5",
                    input {
                        class: FIELD,
                        r#type: "number",
                        min: "0",
                        value: "{t.xp}",
                        oninput: move |e| {
                            let v = e.value().trim().parse::<u64>().unwrap_or(0);
                            if let Some(row) = draft.write().tiers.get_mut(i) {
                                row.xp = v;
                            }
                        },
                    }
                    span { class: "text-[10px] text-[var(--text-dim)] shrink-0", "xp" }
                    input {
                        class: "flex-1 min-w-0 bg-transparent border border-[var(--edge)] focus:border-[var(--accent)] rounded px-2 py-1 text-xs text-[var(--text)] outline-none transition-colors",
                        placeholder: "Rank name",
                        maxlength: crate::protocol::MAX_TIER_NAME as i64,
                        value: "{t.name}",
                        oninput: move |e| {
                            let v = e.value();
                            if let Some(row) = draft.write().tiers.get_mut(i) {
                                row.name = v;
                            }
                        },
                    }
                    input {
                        r#type: "color",
                        class: "w-7 h-7 rounded border border-[var(--border)] bg-transparent cursor-pointer shrink-0",
                        value: "{t.color.clone().unwrap_or_else(|| \"#e0a06a\".into())}",
                        oninput: move |e| {
                            let v = e.value();
                            if let Some(row) = draft.write().tiers.get_mut(i) {
                                row.color = Some(v);
                            }
                        },
                    }
                    button {
                        class: "text-[10px] uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--danger)] transition-colors shrink-0",
                        onclick: move |_| {
                            let mut d = draft.write();
                            if i < d.tiers.len() {
                                d.tiers.remove(i);
                            }
                        },
                        "Remove"
                    }
                }
            }
            if !full {
                button {
                    class: "mt-1.5 rounded px-3 py-1 text-[10px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors",
                    onclick: move |_| {
                        let next_xp = draft
                            .read()
                            .tiers
                            .iter()
                            .map(|t| t.xp)
                            .max()
                            .map(|m| m + 50)
                            .unwrap_or(0);
                        draft.write().tiers.push(LevelTier {
                            xp: next_xp,
                            name: String::new(),
                            color: None,
                        });
                    },
                    "+ Rank"
                }
            }
            div { class: HINT,
                "A member wears the highest rank they have reached. Rows are sorted by threshold when you save, and one without a name is dropped."
            }
        }
    }
}
