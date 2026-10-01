use dioxus::prelude::*;

use crate::state::use_app_state;

pub(crate) fn sfx(name: &str) {
    crate::native_sounds::play(name);
}

// Home also receives DMs while offline, so it needs the same notification listener as the workspace.
#[component]
pub fn MessageSounds() -> Element {
    let state = use_app_state();
    let settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let output = use_memo(move || {
        let settings = settings.read();
        (settings.sfx_volume, settings.selected_output_device.clone())
    });
    use_effect(move || {
        let (volume, device) = output();
        crate::native_sounds::configure(volume, device);
    });

    let notify = use_memo(move || state.read().notify_tick);
    let mut last_notify = use_signal(|| 0u64);
    use_effect(move || {
        let now = notify();
        if now != 0 && now != *last_notify.peek() {
            sfx("notify");
        }
        last_notify.set(now);
    });

    let dm_notify = use_memo(move || state.read().dm_notify_tick);
    let mut last_dm = use_signal(|| 0u64);
    use_effect(move || {
        let now = dm_notify();
        if now != 0 && now != *last_dm.peek() {
            sfx("dm");
        }
        last_dm.set(now);
    });

    rsx! { Fragment {} }
}
