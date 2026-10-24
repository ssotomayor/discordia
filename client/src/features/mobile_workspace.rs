use dioxus::prelude::*;

use super::workspace::MobileTab;
use crate::state::use_app_state;

#[cfg(any(target_os = "android", test))]
fn media_guild(s: &crate::state::AppState) -> Option<crate::protocol::Id> {
    let channel = s.voice.channel_id?;
    s.channels
        .iter()
        .find(|ch| ch.id == channel)
        .map(|ch| ch.guild_id)
}

#[cfg(any(target_os = "android", test))]
fn media_chat_visible(s: &crate::state::AppState) -> bool {
    media_guild(s).is_some_and(|guild| {
        s.selected_guild == Some(guild)
            && s.channels
                .iter()
                .any(|ch| Some(ch.id) == s.selected_channel && ch.guild_id == guild)
    })
}

#[cfg(any(target_os = "android", test))]
fn open_media_chat(s: &mut crate::state::AppState) {
    if let Some(guild) = media_guild(s) {
        if !media_chat_visible(s) {
            s.selected_channel = s
                .channels
                .iter()
                .find(|ch| ch.guild_id == guild && ch.kind == crate::protocol::ChannelKind::Text)
                .map(|ch| ch.id)
                .or(s.voice.channel_id);
        }
        s.selected_guild = Some(guild);
    }
}

#[component]
fn MobilePanel(active: bool, name: &'static str, children: Element) -> Element {
    // Retaining layout keeps the chat virtualizer's viewport and scroll anchor valid.
    let visibility = if active { "visible" } else { "hidden" };
    rsx! {
        div {
            class: "absolute inset-0 min-w-0 min-h-0 overflow-hidden",
            style: "visibility: {visibility};",
            "data-mobile-panel": name,
            "inert": (!active).then_some(""),
            aria_hidden: "{!active}",
            {children}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dioxus::core::{AttributeValue, Mutation};
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
    };

    #[derive(Default, PartialEq)]
    struct Counts {
        mounts: Cell<usize>,
        drops: Cell<usize>,
    }

    type Export<T> = Rc<RefCell<Option<Signal<T>>>>;

    #[test]
    fn media_stays_in_its_guild_chat_and_opening_it_returns_to_that_guild() {
        use crate::protocol::{Channel, ChannelKind, Id};
        let mut s = crate::state::AppState::empty();
        let guild = Id::new_v4();
        let other = Id::new_v4();
        let voice = Id::new_v4();
        let text = Id::new_v4();
        let elsewhere = Id::new_v4();
        for (id, guild_id, kind) in [
            (voice, guild, ChannelKind::Voice),
            (text, guild, ChannelKind::Text),
            (elsewhere, other, ChannelKind::Text),
        ] {
            s.channels.push(Channel {
                id,
                guild_id,
                kind,
                name: "test".into(),
                topic: None,
                read_only: false,
                slowmode_secs: 0,
                position: 0,
                access: None,
                divider: Default::default(),
                markdown: false,
            });
        }
        s.voice.channel_id = Some(voice);
        s.selected_guild = Some(other);
        s.selected_channel = Some(elsewhere);
        assert!(!media_chat_visible(&s));
        open_media_chat(&mut s);
        assert_eq!(
            (s.selected_guild, s.selected_channel),
            (Some(guild), Some(text))
        );
        assert!(media_chat_visible(&s));
        s.selected_guild = None;
        assert!(!media_chat_visible(&s));
        s.selected_guild = Some(guild);
        s.selected_channel = Some(elsewhere);
        assert!(!media_chat_visible(&s));
        s.voice.channel_id = None;
        assert!(!media_chat_visible(&s));
    }

    #[component]
    fn DraftEditor(counts: Rc<Counts>, export: Export<String>) -> Element {
        let draft = use_signal(|| String::from("unsent draft"));
        *export.borrow_mut() = Some(draft);
        use_hook({
            let counts = counts.clone();
            move || counts.mounts.set(counts.mounts.get() + 1)
        });
        use_drop(move || counts.drops.set(counts.drops.get() + 1));
        rsx! { div { "{draft}" } }
    }

    fn panel_fixture(
        (active_export, draft_export, counts): (Export<bool>, Export<String>, Rc<Counts>),
    ) -> Element {
        let active = use_signal(|| true);
        *active_export.borrow_mut() = Some(active);
        rsx! { MobilePanel { active: active(), name: "chat",
            DraftEditor { counts, export: draft_export }
        } }
    }

    #[test]
    fn tab_switches_retain_editor_state_and_disable_the_hidden_panel() {
        let active_export = Export::default();
        let draft_export = Export::default();
        let counts = Rc::new(Counts::default());
        let mut dom = VirtualDom::new_with_props(
            panel_fixture,
            (active_export.clone(), draft_export.clone(), counts.clone()),
        );
        dom.rebuild_in_place();
        let mut active = active_export.borrow().unwrap();
        let mut draft = draft_export.borrow().unwrap();
        draft.set("keep this draft".into());
        dom.render_immediate_to_vec();
        for _ in 0..10 {
            active.set(false);
            let hidden = dom.render_immediate_to_vec();
            assert!(hidden.edits.iter().any(|edit| matches!(edit,
                Mutation::SetAttribute { name: "inert", value: AttributeValue::Text(value), .. } if value.is_empty()
            )));
            active.set(true);
            dom.render_immediate_to_vec();
            assert_eq!(counts.mounts.get(), 1);
            assert_eq!(counts.drops.get(), 0);
            assert_eq!(&*draft.read(), "keep this draft");
        }
        drop(dom);
        assert_eq!(counts.drops.get(), 1);
    }
}

#[component]
pub(super) fn MobileWorkspace(mut activities_open: Signal<bool>) -> Element {
    #[allow(unused_mut)]
    let mut state = use_app_state();
    let mut tab = use_signal(|| MobileTab::Channels);
    use_context_provider(move || tab);
    #[cfg(target_os = "android")]
    {
        let media = use_memo(move || {
            let s = state.read();
            (
                s.screen_sharing,
                s.camera_on,
                s.screen_viewing.clone(),
                s.cameras_watching.clone(),
            )
        });
        let mut previous = use_signal(move || media.peek().clone());
        use_effect(move || {
            let current = media();
            let old = previous.peek().clone();
            let started = (current.0 && !old.0)
                || (current.1 && !old.1)
                || !current.2.is_subset(&old.2)
                || !current.3.is_subset(&old.3);
            if current != old {
                previous.set(current);
            }
            if started {
                open_media_chat(&mut state.write());
                tab.set(MobileTab::Chat);
            }
        });
        let media_visible =
            use_memo(move || tab() == MobileTab::Chat && media_chat_visible(&state.read()));
        use_effect(move || {
            let visible = media_visible();
            let _ = document::eval(&format!(
                "window.__dxMediaPanelVisible={visible};window.dxScreen?.setMediaPanelVisibility({visible});"
            ));
        });
        use_drop(move || {
            let _ = document::eval(
                "window.__dxMediaPanelVisible=true;window.dxScreen?.setMediaPanelVisibility(true);",
            );
        });
    }
    let selected_channel = use_memo(move || state.read().selected_channel);
    let mut previous_channel = use_signal(|| *selected_channel.peek());
    use_effect(move || {
        let current = selected_channel();
        if current != *previous_channel.peek() {
            // Ready selects a default channel before the person has chosen to open its chat.
            let had_channel = previous_channel.peek().is_some();
            previous_channel.set(current);
            if had_channel && current.is_some() {
                tab.set(MobileTab::Chat);
            }
        }
    });
    let current = tab();
    rsx! {
        div { class: "dxf-mobile-workspace flex-1 min-h-0 min-w-0 flex flex-col h-full w-full overflow-hidden",
            div { class: "flex-1 min-h-0 w-full overflow-hidden relative",
                MobilePanel { active: current == MobileTab::Channels, name: "channels",
                    div { class: "flex h-full w-full overflow-hidden bg-[var(--bg)]",
                        div { class: "dxf-mobile-server-column w-16 shrink-0 h-full border-r border-[var(--border)] overflow-y-auto flex flex-col items-center py-2",
                            super::guilds::GuildsSidebar {}
                        }
                        div { class: "flex-1 min-w-0 h-full overflow-hidden bg-[var(--panel-solid)]",
                            super::channels::ChannelsColumn {}
                        }
                    }
                }
                MobilePanel { active: current == MobileTab::Chat, name: "chat",
                    div { class: "flex flex-col h-full w-full overflow-hidden bg-[var(--bg)]",
                        if cfg!(target_os = "android") {
                            MobileMedia { active: current == MobileTab::Chat }
                        } else if current == MobileTab::Chat {
                            super::screenshare::ScreenWatchWindow {}
                        }
                        div { class: "flex-1 min-h-0 h-full flex flex-col overflow-hidden",
                            super::chat::ChatView {}
                        }
                    }
                }
                if current == MobileTab::Members {
                    div { class: "absolute inset-0 flex flex-col overflow-hidden bg-[var(--panel-solid)]",
                        div { class: "h-11 px-3 flex items-center border-b border-[var(--border)] shrink-0",
                            button { class: "text-xs text-[var(--accent)] dxf-touch-target",
                                onclick: move |_| tab.set(MobileTab::Chat),
                                "← ", {crate::i18n::text("Back to chat", &[])}
                            }
                        }
                        div { class: "flex-1 min-h-0 overflow-y-auto", super::members::MembersPanel {} }
                    }
                }
            }
            div {
                class: "dxf-mobile-nav shrink-0 flex items-center justify-around border-t border-[var(--border)] bg-[var(--panel2)] z-30 select-none",
                style: "padding-bottom: max(var(--safe-bottom), 6px); height: calc(52px + var(--safe-bottom));",
                for (next, icon, label) in [(MobileTab::Channels, super::icons::CHANNEL_LIST, "Channels"), (MobileTab::Chat, super::icons::CHAT, "Chat"), (MobileTab::Members, super::icons::USERS, "Members")] {
                    button {
                        key: "{next:?}",
                        class: if current == next {
                            "flex-1 flex flex-col items-center justify-center min-h-[44px] py-1 text-xs text-[var(--accent)] font-semibold"
                        } else {
                            "flex-1 flex flex-col items-center justify-center min-h-[44px] py-1 text-xs text-[var(--text-muted)]"
                        },
                        aria_pressed: "{current == next}",
                        onclick: move |_| { if *tab.peek() != next { tab.set(next); } },
                        span { class: "dxf-mobile-nav-icon", aria_hidden: "true", dangerous_inner_html: icon }
                        span { class: "text-[10px] tracking-tight mt-0.5", {crate::i18n::text(label, &[])} }
                    }
                }
                if !cfg!(target_os = "android") {
                button {
                    class: "flex-1 flex flex-col items-center justify-center min-h-[44px] py-1 text-xs text-[var(--text-muted)]",
                    onclick: move |_| activities_open.set(true),
                    aria_expanded: activities_open(),
                    span { class: "dxf-mobile-nav-icon", aria_hidden: "true", dangerous_inner_html: super::icons::DICES }
                    span { class: "text-[10px] tracking-tight mt-0.5", {crate::i18n::text("Activities", &[])} }
                }
                }
            }
        }
    }
}

#[component]
fn MobileMedia(active: bool) -> Element {
    #[cfg(target_os = "android")]
    {
        let state = use_app_state();
        let visible = use_memo(use_reactive((&active,), move |(active,)| {
            active && media_chat_visible(&state.read())
        }));
        rsx! {
            div { class: "dxf-mobile-media", style: if visible() { "display:flex;" } else { "display:none;" },
                super::screenshare::ScreenSelfPreview { visible: visible() }
                super::camera::CameraSelfPreview {}
                if visible() {
                    super::camera::CameraGridWindow {}
                    super::screenshare::ScreenWatchWindow {}
                }
            }
        }
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = active;
        rsx! { Fragment {} }
    }
}
