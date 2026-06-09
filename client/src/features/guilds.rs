use dioxus::prelude::*;

use crate::protocol::Id;
use crate::state::use_app_state;

#[component]
pub fn GuildsSidebar() -> Element {
    let mut state = use_app_state();

    let snapshot = state.read();
    let guilds = snapshot.guilds.clone();
    let selected = snapshot.selected_guild;
    drop(snapshot);

    rsx! {
        nav { class: "w-full h-full bg-[var(--panel)] border border-[var(--border)] rounded-lg flex flex-col items-center py-3 gap-2 overflow-y-auto",
            for guild in guilds.iter().cloned() {
                GuildIcon {
                    key: "{guild.id}",
                    id: guild.id,
                    label: guild.icon.unwrap_or_else(|| initials(&guild.name)),
                    name: guild.name.clone(),
                    selected: selected == Some(guild.id),
                    on_select: move |gid: Id| {
                        let mut s = state.write();
                        s.selected_guild = Some(gid);
                        s.selected_channel = s
                            .channels
                            .iter()
                            .find(|c| c.guild_id == gid)
                            .map(|c| c.id);
                    },
                }
            }
        }
    }
}

#[component]
fn GuildIcon(
    id: Id,
    label: String,
    name: String,
    selected: bool,
    on_select: EventHandler<Id>,
) -> Element {
    let cls = if selected {
        "border-[var(--accent)] text-[var(--accent)]"
    } else {
        "border-[var(--border)] text-[var(--text-muted)] hover:text-[var(--text)] hover:border-[var(--border-strong)]"
    };

    rsx! {
        button {
            class: "w-10 h-10 rounded-md border flex items-center justify-center text-xs font-medium transition-colors {cls}",
            title: "{name}",
            onclick: move |_| on_select.call(id),
            "{label}"
        }
    }
}

fn initials(name: &str) -> String {
    name.split_whitespace()
        .filter_map(|w| w.chars().next())
        .take(2)
        .collect::<String>()
        .to_uppercase()
}
