#[cfg(any(target_os = "android", test))]
use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

pub const SCREEN: u16 = 100;
pub const CONTENT: u16 = 200;
pub const SOCIAL_CHAT: u16 = 300;
pub const SETTINGS: u16 = 1000;

#[cfg(any(target_os = "android", test))]
struct BackStack<T>(Rc<RefCell<BTreeMap<(u16, usize), T>>>);

#[cfg(any(target_os = "android", test))]
impl<T> Default for BackStack<T> {
    fn default() -> Self {
        Self(Rc::default())
    }
}

#[cfg(any(target_os = "android", test))]
impl<T: Clone> BackStack<T> {
    fn set(&self, key: (u16, usize), handler: Option<T>) {
        let mut handlers = self.0.borrow_mut();
        if let Some(handler) = handler {
            handlers.insert(key, handler);
        } else {
            handlers.remove(&key);
        }
    }

    fn current(&self) -> Option<T> {
        self.0
            .borrow()
            .last_key_value()
            .map(|(_, handler)| handler.clone())
    }
}

#[cfg(target_os = "android")]
impl<T> Clone for BackStack<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

pub fn use_navigation() {
    #[cfg(target_os = "android")]
    {
        use dioxus::prelude::*;
        let handlers = use_context_provider(BackStack::<Callback<()>>::default);
        use_future(move || {
            let handlers = handlers.clone();
            async move {
                let mut bridge = document::eval(include_str!("../assets/android-back.js"));
                while bridge.recv::<()>().await.is_ok() {
                    if let Some(handler) = handlers.current() {
                        handler.call(());
                    } else if let Err(error) = crate::android::background_app().await {
                        tracing::warn!(%error, "could not background Android task");
                    }
                }
            }
        });
    }
}

pub fn use_handler(priority: u16, enabled: bool, on_back: impl FnMut(()) + 'static) {
    #[cfg(target_os = "android")]
    {
        use dioxus::prelude::*;
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let id = use_hook(|| NEXT.fetch_add(1, Ordering::Relaxed));
        let handler = use_callback(on_back);
        let handlers = use_context::<BackStack<Callback<()>>>();
        let cleanup = handlers.clone();
        use_effect(use_reactive((&enabled,), move |(enabled,)| {
            handlers.set((priority, id), enabled.then_some(handler));
        }));
        use_drop(move || cleanup.set((priority, id), None));
    }
    #[cfg(not(target_os = "android"))]
    let _ = (priority, enabled, on_back);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_then_social_chat_then_home_then_system_back() {
        let stack = BackStack::default();
        stack.set((SCREEN, 1), Some("home"));
        stack.set((SOCIAL_CHAT, 2), Some("social list"));
        stack.set((SETTINGS, 3), Some("close settings"));
        assert_eq!(stack.current(), Some("close settings"));
        stack.set((SETTINGS, 3), None);
        assert_eq!(stack.current(), Some("social list"));
        stack.set((SOCIAL_CHAT, 2), None);
        assert_eq!(stack.current(), Some("home"));
        stack.set((SCREEN, 1), None);
        assert_eq!(stack.current(), None);
    }

    #[test]
    fn server_content_returns_to_channels_before_ordered_disconnect() {
        let stack = BackStack::default();
        stack.set((SCREEN, 4), Some("ordered disconnect"));
        stack.set((CONTENT, 5), Some("channels"));
        assert_eq!(stack.current(), Some("channels"));
        stack.set((CONTENT, 5), None);
        assert_eq!(stack.current(), Some("ordered disconnect"));
        stack.set((SCREEN, 4), None);
        assert_eq!(stack.current(), None);
    }
}
