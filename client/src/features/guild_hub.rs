use dioxus::prelude::*;

use crate::protocol::{ClientMessage, Id};
use crate::state::{ConnectionStatus, use_app_state, use_gateway};

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Create,
    Explore,
    Discord,
}

#[derive(Clone, PartialEq)]
enum Action {
    Create(String),
    Join(Id),
    Invite,
}

#[derive(Clone, PartialEq)]
struct Pending {
    serial: u64,
    before: Vec<Id>,
    action: Action,
    confirmation: Option<(u64, Id)>,
}

impl Pending {
    fn completed(&self, id: Id, name: &str, owned: bool) -> bool {
        match &self.action {
            Action::Create(expected) => !self.before.contains(&id) && owned && name == expected,
            Action::Join(expected) => id == *expected,
            Action::Invite => true,
        }
    }
}

#[component]
pub fn AddGuildDialog(on_close: EventHandler<()>) -> Element {
    let mut state = use_app_state();
    let gateway = use_gateway();
    let mut tab = use_signal(|| Tab::Create);
    let mut name = use_signal(String::new);
    let mut invite = use_signal(String::new);
    let mut filter = use_signal(String::new);
    let import_locked = use_signal(|| false);
    let mut pending = use_signal(|| None::<Pending>);
    let mut serial = use_signal(|| 0_u64);
    let mut error = use_signal(|| None::<String>);
    let mut catalog_loading = use_signal(|| None::<(u64, u64)>);
    let mut catalog_loaded = use_signal(|| false);

    let observations = use_memo(move || {
        let s = state.read();
        (
            s.guilds
                .iter()
                .map(|g| (g.id, g.name.clone(), s.is_owner(g.id)))
                .collect::<Vec<_>>(),
            s.error_toast.clone(),
            s.rules_prompt.clone(),
            s.status,
            s.catalog_revision,
            s.guild_join_confirmation,
        )
    });
    use_effect(move || {
        let (guilds, toast, rules, status, revision, confirmation) = observations();
        if let Some(request) = pending() {
            if confirmation != request.confirmation
                && confirmation.is_some_and(|(_, joined)| {
                    guilds.iter().any(|(id, name, owned)| {
                        *id == joined && request.completed(*id, name, *owned)
                    })
                })
            {
                pending.set(None);
                on_close.call(());
            } else if rules.as_ref().is_some_and(|p| match request.action {
                Action::Join(id) => p.guild_id == id,
                Action::Invite => p.invite_code.as_deref() == Some(invite.peek().trim()),
                Action::Create(_) => false,
            }) {
                // The root rules dialog owns the next step of a gated join.
                pending.set(None);
                on_close.call(());
            } else if let Some(message) = toast.clone() {
                error.set(Some(message));
                pending.set(None);
            } else if status != ConnectionStatus::Ready {
                error.set(Some(
                    "Connection interrupted. Check your guild list before trying again.".into(),
                ));
                pending.set(None);
            }
        }
        if let Some((_, before)) = catalog_loading() {
            if revision != before {
                catalog_loading.set(None);
                catalog_loaded.set(true);
            } else if let Some(message) = toast {
                catalog_loading.set(None);
                error.set(Some(message));
            } else if status != ConnectionStatus::Ready {
                catalog_loading.set(None);
                error.set(Some("Reconnect to browse this host's guilds.".into()));
            }
        }
    });

    let begin = move |action: Action| {
        if pending.peek().is_some() || *import_locked.peek() {
            return false;
        }
        if state.peek().status != ConnectionStatus::Ready {
            error.set(Some("Connect to a server first.".into()));
            return false;
        }
        let next = serial().wrapping_add(1);
        serial.set(next);
        let before = state.peek().guilds.iter().map(|g| g.id).collect();
        state.write().error_toast = None;
        error.set(None);
        pending.set(Some(Pending {
            serial: next,
            before,
            action,
            confirmation: state.peek().guild_join_confirmation,
        }));
        spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            if pending.peek().as_ref().is_some_and(|p| p.serial == next) {
                pending.set(None);
                error.set(Some(
                    "No confirmation received. Check your guild list before trying again.".into(),
                ));
            }
        });
        true
    };

    let fetch_catalog = {
        let gateway = gateway.clone();
        move |offset: u32| {
            if catalog_loading.peek().is_some() {
                return;
            }
            if state.peek().status != ConnectionStatus::Ready {
                error.set(Some("Connect to a server first.".into()));
                return;
            }
            let revision = state.peek().catalog_revision;
            let next = serial().wrapping_add(1);
            serial.set(next);
            catalog_loading.set(Some((next, revision)));
            state.write().error_toast = None;
            error.set(None);
            gateway.send(ClientMessage::FetchCatalog { offset, limit: 0 });
            spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                if *catalog_loading.peek() == Some((next, revision)) {
                    catalog_loading.set(None);
                    error.set(Some(
                        "Couldn't load the guild catalog. Try refreshing.".into(),
                    ));
                }
            });
        }
    };
    let close = move |_| {
        if !*import_locked.peek() && pending.peek().is_none() {
            on_close.call(());
        }
    };
    let locked = import_locked() || pending().is_some();
    let ready = state.read().status == ConnectionStatus::Ready;
    let available = {
        let s = state.read();
        let query = filter().trim().to_lowercase();
        s.catalog
            .iter()
            .filter(|g| {
                !s.guilds.iter().any(|joined| joined.id == g.id)
                    && g.name.to_lowercase().contains(&query)
            })
            .cloned()
            .collect::<Vec<_>>()
    };
    let catalog_len = state.read().catalog.len();
    let catalog_total = state.read().catalog_total as usize;

    rsx! {
        document::Style { {include_str!("../../assets/guild-hub.css")} }
        div {
            class: "dxf-backdrop-in guild-hub-backdrop",
            onclick: close,
            onkeydown: move |e| {
                if e.key() == Key::Escape {
                    e.stop_propagation();
                    if !*import_locked.peek() && pending.peek().is_none() { on_close.call(()); }
                }
            },
            div {
                class: "dxf-modal-in guild-hub",
                role: "dialog",
                aria_modal: "true",
                aria_labelledby: "guild-hub-title",
                onclick: move |e| e.stop_propagation(),
                header { class: "guild-hub-header",
                    div {
                        h3 { id: "guild-hub-title", "Add guild" }
                        p { "Create a home, join one here, or bring yours from Discord." }
                    }
                    button { class: "guild-hub-close", title: "Close", aria_label: "Close", disabled: locked, onclick: close, "×" }
                }
                div { class: "guild-hub-tabs", role: "tablist", aria_label: "Add guild options",
                    for (choice, label, suffix) in [(Tab::Create, "Create", "create"), (Tab::Explore, "Explore", "explore"), (Tab::Discord, "Discord", "discord")] {
                        button {
                            key: "{suffix}",
                            id: "guild-tab-{suffix}",
                            role: "tab",
                            aria_selected: tab() == choice,
                            aria_controls: "guild-panel-{suffix}",
                            disabled: locked,
                            onclick: {
                                let mut fetch_catalog = fetch_catalog.clone();
                                move |_| {
                                    tab.set(choice);
                                    error.set(None);
                                    if choice == Tab::Explore && !catalog_loaded() { fetch_catalog(0); }
                                }
                            },
                            "{label}"
                        }
                    }
                }
                div { class: "guild-hub-content",
                    if let Some(message) = error() { div { class: "guild-hub-error", role: "alert", "{message}" } }
                    div { id: "guild-panel-create", role: "tabpanel", aria_labelledby: "guild-tab-create", hidden: tab() != Tab::Create,
                        h4 { "Your own guild" }
                        p { class: "guild-hub-description", "Create a new space on this server. You can set its picture, channels and permissions afterwards." }
                        form { class: "guild-hub-form",
                            onsubmit: {
                                let gateway = gateway.clone();
                                let mut begin = begin;
                                move |_| {
                                    let value = match crate::protocol::sanitize_name("guild", &name(), 64) {
                                        Ok(value) => value,
                                        Err(message) => { error.set(Some(message)); return; }
                                    };
                                    if begin(Action::Create(value.clone())) {
                                        gateway.send(ClientMessage::CreateGuild { name: value, template: None });
                                    }
                                }
                            },
                            label { r#for: "guild-hub-name", "Guild name" }
                            input { id: "guild-hub-name", autofocus: true, maxlength: 64, placeholder: "Give your guild a name", value: "{name}", disabled: locked, oninput: move |e| name.set(e.value()) }
                            div { class: "guild-hub-actions", button { class: "guild-hub-primary", r#type: "submit", disabled: locked || !ready || name().trim().is_empty(), if pending().is_some() { "Creating…" } else { "Create guild" } } }
                        }
                    }
                    div { id: "guild-panel-explore", role: "tabpanel", aria_labelledby: "guild-tab-explore", hidden: tab() != Tab::Explore,
                        h4 { "Find your next guild" }
                        p { class: "guild-hub-description", "Browse public guilds on this host, or join with an invite code." }
                        div { class: "guild-hub-catalog-tools",
                            input { aria_label: "Search guilds", placeholder: "Search guilds…", value: "{filter}", oninput: move |e| filter.set(e.value()) }
                            button { disabled: locked || catalog_loading().is_some() || !ready, onclick: { let mut fetch_catalog = fetch_catalog.clone(); move |_| fetch_catalog(0) }, "Refresh" }
                        }
                        div { class: "guild-hub-catalog", aria_busy: catalog_loading().is_some(),
                            if catalog_loading().is_some() { p { class: "guild-hub-description", role: "status", "Loading guilds…" } }
                            if catalog_loaded() && available.is_empty() && catalog_loading().is_none() {
                                p { class: "guild-hub-description", if filter().trim().is_empty() { "No more public guilds here. Try an invite or create your own." } else { "No guilds match your search." } }
                            }
                            for g in available {
                                {
                                    let gateway = gateway.clone();
                                    let mut begin = begin;
                                    let gid = g.id;
                                    let label = g.icon.clone().unwrap_or_else(|| g.name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_else(|| "?".into()));
                                    rsx! {
                                        div { key: "{gid}", class: "guild-hub-guild",
                                            span { class: "guild-hub-icon", "{label}" }
                                            div { class: "guild-hub-guild-info", strong { "{g.name}" } span { "{g.member_count} members" } }
                                            button { disabled: locked || !ready, onclick: move |_| {
                                                if begin(Action::Join(gid)) { gateway.send(ClientMessage::JoinGuild { guild_id: gid, accept: false, pow_nonce: None }); }
                                            }, "Join" }
                                        }
                                    }
                                }
                            }
                            if catalog_len < catalog_total { button { disabled: locked || catalog_loading().is_some() || !ready, onclick: { let mut fetch_catalog = fetch_catalog.clone(); move |_| fetch_catalog(catalog_len as u32) }, "Load more" } }
                        }
                        form { class: "guild-hub-invite",
                            onsubmit: {
                                let gateway = gateway.clone();
                                let mut begin = begin;
                                move |_| {
                                    let code = invite().trim().to_string();
                                    if !code.is_empty() && begin(Action::Invite) { gateway.send(ClientMessage::JoinByInvite { code, accept: false, pow_nonce: None }); }
                                }
                            },
                            label { r#for: "guild-hub-invite", "Have an invite code?" }
                            div { class: "guild-hub-catalog-tools",
                                input { id: "guild-hub-invite", placeholder: "Paste an invite code", value: "{invite}", disabled: locked, oninput: move |e| invite.set(e.value()) }
                                button { class: "guild-hub-primary", r#type: "submit", disabled: locked || !ready || invite().trim().is_empty(), "Join" }
                            }
                        }
                        if pending().is_some() { p { class: "guild-hub-description", role: "status", "Waiting for the server…" } }
                    }
                    div { id: "guild-panel-discord", role: "tabpanel", aria_labelledby: "guild-tab-discord", hidden: tab() != Tab::Discord,
                        h4 { "Bring your guild from Discord" }
                        super::discord_import::DiscordImportPanel { locked: import_locked, on_close }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirmation_matches_the_pending_action() {
        let one = Id::from_u128(1);
        let two = Id::from_u128(2);
        let three = Id::from_u128(3);
        let mut p = Pending {
            serial: 1,
            before: vec![one],
            action: Action::Create("Home".into()),
            confirmation: None,
        };
        assert!(!p.completed(one, "Home", true));
        assert!(!p.completed(two, "Home", false));
        assert!(!p.completed(two, "Other", true));
        assert!(p.completed(two, "Home", true));
        p.action = Action::Join(three);
        assert!(!p.completed(two, "Home", false));
        assert!(p.completed(three, "Other", false));
        p.action = Action::Invite;
        assert!(p.completed(one, "Home", false));
        assert!(p.completed(two, "Other", false));
    }
}
