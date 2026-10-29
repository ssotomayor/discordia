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

    let dm_notify = use_memo(move || {
        let current = state.read();
        (
            current.dm_notify_tick,
            !cfg!(target_os = "android") || current.app_foreground,
        )
    });
    let mut last_dm = use_signal(|| 0u64);
    use_effect(move || {
        let (now, foreground) = dm_notify();
        if foreground && now != 0 && now != *last_dm.peek() {
            sfx("dm");
        }
        last_dm.set(now);
    });

    let voice_controls = use_memo(move || {
        let s = state.read();
        (s.voice.muted, s.voice.deafened)
    });
    let mut last_controls = use_signal(|| {
        let s = state.peek();
        (s.voice.muted, s.voice.deafened)
    });
    use_effect(move || {
        let (muted, deafened) = voice_controls();
        let (last_m, last_d) = *last_controls.peek();
        if (muted, deafened) == (last_m, last_d) {
            return;
        }
        last_controls.set((muted, deafened));
        if deafened != last_d {
            if deafened {
                sfx("deafen");
            } else {
                sfx("undeafen");
            }
        } else if muted != last_m {
            if muted {
                sfx("mute");
            } else {
                sfx("unmute");
            }
        }
    });

    rsx! { Fragment {} }
}
