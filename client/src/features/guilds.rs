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
        nav { class: "w-[72px] shrink-0 bg-[#1e1f22] flex flex-col items-center py-3 gap-2 overflow-y-auto",
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
    let pill_visibility = if selected { "h-10" } else { "h-2 group-hover:h-5" };
    let icon_classes = if selected {
        "bg-indigo-500 rounded-2xl"
    } else {
        "bg-[#313338] rounded-[24px] hover:bg-indigo-500 hover:rounded-2xl"
    };

    rsx! {
        div { class: "group relative w-full flex justify-center",
            div { class: "absolute left-0 top-1/2 -translate-y-1/2 w-1 rounded-r bg-white transition-all {pill_visibility}" }
            button {
                class: "w-12 h-12 flex items-center justify-center text-white font-bold transition-all {icon_classes}",
                title: "{name}",
                onclick: move |_| on_select.call(id),
                "{label}"
            }
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
