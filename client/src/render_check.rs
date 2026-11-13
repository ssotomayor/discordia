use std::cell::Cell;
use std::sync::Once;

use dioxus::core::{Mutations, VirtualDom};

thread_local! {
    static PANICKED: Cell<bool> = const { Cell::new(false) };
}

// dioxus-core catches a panicking render and only logs it, so an assert inside
// a component passed its test whatever it found.
fn checked<T>(render: impl FnOnce() -> T) -> T {
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            PANICKED.set(true);
            previous(info)
        }));
    });
    PANICKED.set(false);
    let out = render();
    assert!(
        !PANICKED.replace(false),
        "a component panicked while rendering; its message is above"
    );
    out
}

pub(crate) trait Checked {
    fn rebuild_checked(&mut self);
    fn render_checked(&mut self) -> Mutations;
}

impl Checked for VirtualDom {
    fn rebuild_checked(&mut self) {
        checked(|| self.rebuild_in_place())
    }

    fn render_checked(&mut self) -> Mutations {
        checked(|| self.render_immediate_to_vec())
    }
}
