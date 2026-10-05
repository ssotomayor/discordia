use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use dioxus::prelude::*;
use tokio::sync::mpsc;

use crate::{
    identity::Identity,
    nostr::service::NostrTx,
    settings::ClientSettings,
    state::{AppState, GatewayTx},
};

pub(super) fn use_social_window(state: Signal<AppState>) -> (Signal<bool>, EventHandler<()>) {
    let mut open = use_signal(|| false);
    let mut creating = use_signal(|| false);
    let mut generation = use_signal(|| 0_u64);
    let settings = use_context::<Signal<ClientSettings>>();
    let nostr = use_context::<NostrTx>();
    let identity = use_context::<Identity>();
    let gateway = use_context::<GatewayTx>();
    let parent = dioxus::desktop::use_window();
    let handle = use_hook(|| Rc::new(RefCell::new(None::<dioxus::desktop::WeakDesktopContext>)));
    let alive = use_hook(|| Rc::new(Cell::new(true)));
    let (closed, receiver) = use_hook(|| {
        let (tx, rx) = mpsc::unbounded_channel::<u64>();
        (tx, Rc::new(RefCell::new(Some(rx))))
    });
    let closed_handle = handle.clone();
    use_future(move || {
        let receiver = receiver.borrow_mut().take();
        let closed_handle = closed_handle.clone();
        async move {
            let Some(mut receiver) = receiver else { return };
            while let Some(id) = receiver.recv().await {
                if id == *generation.peek() {
                    closed_handle.borrow_mut().take();
                    open.set(false);
                }
            }
        }
    });
    let drop_handle = handle.clone();
    let drop_alive = alive.clone();
    use_drop(move || {
        drop_alive.set(false);
        if let Some(window) = drop_handle.borrow().as_ref().and_then(|h| h.upgrade()) {
            window.close();
        }
    });
    let show = EventHandler::new(move |_| {
        if let Some(window) = handle.borrow().as_ref().and_then(|h| h.upgrade()) {
            window.set_visible(true);
            window.window.set_focus();
            return;
        }
        if *creating.peek() {
            return;
        }
        creating.set(true);
        let id = *generation.peek() + 1;
        generation.set(id);
        let dom = VirtualDom::new_with_props(
            SocialWindow,
            SocialWindowProps {
                alive: alive.clone(),
                closed: closed.clone(),
                generation: id,
            },
        )
        .with_root_context(state)
        .with_root_context(settings)
        .with_root_context(nostr.clone())
        .with_root_context(identity.clone())
        .with_root_context(gateway.clone());
        let config = dioxus::desktop::Config::new()
            .with_window(
                crate::desktop_window_builder()
                    .with_title("Discordia — Social")
                    .with_window_icon(crate::load_window_icon())
                    .with_inner_size(dioxus::desktop::tao::dpi::LogicalSize::new(1050.0, 720.0))
                    .with_min_inner_size(dioxus::desktop::tao::dpi::LogicalSize::new(720.0, 460.0)),
            )
            .with_close_behaviour(dioxus::desktop::WindowCloseBehaviour::WindowCloses)
            .with_menu(None);
        let pending = parent.new_window(dom, config);
        let handle = handle.clone();
        let alive = alive.clone();
        // Window creation must finish even if Home unmounts, so the orphan can be closed.
        dioxus::core::spawn_forever(async move {
            let popup = pending.await;
            if !alive.get() {
                popup.close();
                return;
            }
            *handle.borrow_mut() = Some(Rc::downgrade(&popup));
            creating.set(false);
            open.set(true);
        });
    });
    (open, show)
}

#[cfg(test)]
mod tests {
    use super::*;
    type ExportedState = Rc<RefCell<Option<Signal<AppState>>>>;

    /// Counts the signal-hoisting lint, which `tracing::warn!`s from its own
    /// module, while `f` runs.
    fn hoist_warnings(f: impl FnOnce()) -> usize {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tracing_subscriber::layer::SubscriberExt;

        struct Count(Arc<AtomicUsize>);
        impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Count {
            fn on_event(
                &self,
                event: &tracing::Event<'_>,
                _: tracing_subscriber::layer::Context<'_, S>,
            ) {
                if event
                    .metadata()
                    .target()
                    .starts_with("dioxus_signals::warnings")
                {
                    self.0.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        let count = Arc::new(AtomicUsize::new(0));
        let subscriber = tracing_subscriber::registry().with(Count(count.clone()));
        tracing::subscriber::with_default(subscriber, f);
        count.load(Ordering::Relaxed)
    }

    /// Where the shared signal is made. As `WorkspaceView` does: owned by
    /// ROOT, so the popup's scopes descend from the owner in their own tree.
    #[component]
    fn Holder(capture: ExportedState, observed: Rc<Cell<bool>>) -> Element {
        let state = use_hook(|| Signal::new_in_scope(AppState::empty(), ScopeId::ROOT));
        *capture.borrow_mut() = Some(state);
        observed.set(state.read().voice.muted);
        rsx! {}
    }

    #[component]
    fn Spacer() -> Element {
        rsx! {}
    }

    #[component]
    fn Reader(observed: Rc<Cell<bool>>) -> Element {
        let state = crate::state::use_app_state();
        observed.set(state.read().voice.muted);
        rsx! { div { "{state.read().voice.muted}" } }
    }

    #[tokio::test]
    async fn separate_windows_share_state_and_repaint_each_other() {
        let exported = Rc::new(RefCell::new(None::<Signal<AppState>>));
        let owner_muted = Rc::new(Cell::new(false));
        // Two children so the holder's scope id differs from the reader's:
        // equal ids would pass as "the owning scope" by coincidence.
        let mut owner = VirtualDom::new_with_props(
            |(capture, observed): (ExportedState, Rc<Cell<bool>>)| {
                rsx! {
                    Spacer {}
                    Holder { capture, observed }
                }
            },
            (exported.clone(), owner_muted.clone()),
        );
        owner.rebuild_in_place();
        let mut state = exported.borrow().unwrap();
        let popup_muted = Rc::new(Cell::new(false));
        let mut popup = VirtualDom::new_with_props(
            |observed: Rc<Cell<bool>>| rsx! { Reader { observed } },
            popup_muted.clone(),
        )
        .with_root_context(state);
        assert_eq!(hoist_warnings(|| popup.rebuild_in_place()), 0);
        state.write().voice.muted = true;
        tokio::time::timeout(std::time::Duration::from_secs(1), popup.wait_for_work())
            .await
            .unwrap();
        assert_eq!(
            hoist_warnings(|| {
                popup.render_immediate_to_vec();
            }),
            0
        );
        owner.render_immediate_to_vec();
        assert!(popup_muted.get() && owner_muted.get());
        drop(popup);
        state.write().voice.muted = false;
        owner.render_immediate_to_vec();
        assert!(!owner_muted.get());
        state.manually_drop();
    }
}

#[derive(Clone, Props)]
struct SocialWindowProps {
    alive: Rc<Cell<bool>>,
    closed: mpsc::UnboundedSender<u64>,
    generation: u64,
}

impl PartialEq for SocialWindowProps {
    fn eq(&self, other: &Self) -> bool {
        self.generation == other.generation
            && Rc::ptr_eq(&self.alive, &other.alive)
            && self.closed.same_channel(&other.closed)
    }
}

#[allow(non_snake_case)]
fn SocialWindow(props: SocialWindowProps) -> Element {
    let SocialWindowProps {
        alive,
        closed,
        generation,
    } = props;
    use_drop(move || {
        if let Err(error) = closed.send(generation) {
            tracing::debug!(%error, "social owner closed");
        }
    });
    let window = dioxus::desktop::use_window();
    let settings = use_context::<Signal<ClientSettings>>();
    let appearance = settings.read();
    let mut theme = crate::app::theme_vars(&appearance.theme).to_owned();
    if let Some(accent) = &appearance.accent {
        theme.push_str(&crate::app::accent_vars(accent));
    }
    let size_css = crate::ui_size::text_css(appearance.text_size_percent);
    drop(appearance);
    if !alive.get() {
        return rsx! {};
    }
    rsx! {
        crate::app::AppHead {}
        style { "{size_css}" }
        div { class: "dxf-ui h-screen w-screen text-[var(--text)] bg-[var(--bg)] overflow-hidden", style: "{theme}",
            super::chat::ImageViewer {}
            super::profiles::ProfileCard {}
            super::workspace::ErrorToast {}
            div { style: "display: grid; grid-template-columns: clamp(240px, 28vw, 300px) minmax(0, 1fr); height: 100%; min-height: 0;",
                aside { style: "display: flex; flex-direction: column; min-height: 0; border-right: 1px solid var(--edge); background-color: var(--panel);",
                    super::home::SocialPanel { on_close: move |_| window.close() }
                    div { style: "flex-shrink: 0; max-height: 48%; overflow-y: auto;",
                        super::dm_call::CallPanel { embedded: true }
                    }
                }
                super::home::TalkPane {}
            }
        }
    }
}
