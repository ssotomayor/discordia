use dioxus::prelude::*;

use crate::state::use_app_state;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Id, User, VoiceState};
    use crate::state::{AppState, ConnectionStatus, VoicePhase};

    #[test]
    fn viewer_events_follow_identities_without_join_stop_or_reconnect_noise() {
        let mut state = AppState::empty();
        let channel = Id::new_v4();
        state.status = ConnectionStatus::Ready;
        state.voice.phase = VoicePhase::Connected;
        state.voice.channel_id = Some(channel);
        state.screen_sharing = true;
        state.self_user = Some(User {
            pubkey: "me".into(),
            username: "Me".into(),
        });
        let empty = ViewerSnapshot::from_state(&state);
        state.voice_states.push(VoiceState {
            user_pubkey: "alice".into(),
            guild_id: Id::new_v4(),
            channel_id: Some(channel),
            muted: false,
            deafened: false,
            speaking: false,
            camera_on: false,
            screen_sharing: false,
            screen_watching: vec!["me".into()],
        });
        let alice = ViewerSnapshot::from_state(&state);
        assert_eq!(alice.changes_from(&empty), (true, false));
        assert_eq!(
            alice.changes_from(&ViewerSnapshot::default()),
            (false, false)
        );
        state.voice_states.push(state.voice_states[0].clone());
        assert_eq!(
            ViewerSnapshot::from_state(&state).changes_from(&alice),
            (false, false)
        );
        state.voice_states.truncate(1);
        state.voice_states[0].user_pubkey = "bob".into();
        let bob = ViewerSnapshot::from_state(&state);
        assert_eq!(bob.changes_from(&alice), (true, true));
        state.voice_states[0].screen_watching = vec!["someone-else".into()];
        assert_eq!(
            ViewerSnapshot::from_state(&state).changes_from(&bob),
            (false, true)
        );
        state.screen_sharing = false;
        assert_eq!(
            ViewerSnapshot::from_state(&state).changes_from(&bob),
            (false, false)
        );
        state.screen_sharing = true;
        state.voice_session_epoch += 1;
        assert_eq!(
            ViewerSnapshot::from_state(&state).changes_from(&bob),
            (false, false)
        );
        state.status = ConnectionStatus::Reconnecting;
        assert_eq!(
            ViewerSnapshot::from_state(&state).changes_from(&bob),
            (false, false)
        );
    }
}

pub(crate) fn sfx(name: &str) {
    crate::native_sounds::play(name);
}

#[derive(Default, Clone, PartialEq)]
struct ViewerSnapshot {
    stream: Option<(crate::protocol::Id, u64, String)>,
    viewers: std::collections::BTreeSet<String>,
}

impl ViewerSnapshot {
    fn from_state(state: &crate::state::AppState) -> Self {
        if !state.screen_sharing
            || state.status != crate::state::ConnectionStatus::Ready
            || state.voice.phase != crate::state::VoicePhase::Connected
        {
            return Self::default();
        }
        let (Some(channel), Some(me)) = (state.voice.channel_id, state.self_user.as_ref()) else {
            return Self::default();
        };
        Self {
            stream: Some((channel, state.voice_session_epoch, me.pubkey.clone())),
            viewers: state
                .voice_states
                .iter()
                .filter(|vs| {
                    vs.channel_id == Some(channel)
                        && vs.user_pubkey != me.pubkey
                        && vs.screen_watching.contains(&me.pubkey)
                })
                .map(|vs| vs.user_pubkey.clone())
                .collect(),
        }
    }

    fn changes_from(&self, previous: &Self) -> (bool, bool) {
        if self.stream.is_none() || self.stream != previous.stream {
            return (false, false);
        }
        (
            self.viewers.difference(&previous.viewers).next().is_some(),
            previous.viewers.difference(&self.viewers).next().is_some(),
        )
    }
}

// Home also receives DMs while offline, so it needs the same notification listener as the workspace.
#[component]
pub fn MessageSounds() -> Element {
    let mut state = use_app_state();
    let settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let identity = use_context::<crate::identity::Identity>();
    let policy = use_memo(move || {
        (
            settings.read().disabled_sound_categories.clone(),
            state.read().presence_of(&identity.pubkey) == "dnd",
        )
    });
    use_effect(move || {
        let (disabled, dnd) = policy();
        crate::native_sounds::configure_policy(&disabled, dnd);
    });
    use_drop(|| crate::native_sounds::configure_policy(&[], false));
    use_future(move || async move {
        loop {
            let now = chrono::Utc::now().timestamp();
            if state
                .peek()
                .messages
                .values()
                .flatten()
                .any(|m| super::voice_invite::is_expired_message(m, now))
            {
                for messages in state.write().messages.values_mut() {
                    messages.retain(|m| !super::voice_invite::is_expired_message(m, now));
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        }
    });
    let viewers = use_memo(move || ViewerSnapshot::from_state(&state.read()));
    let mut previous_viewers = use_signal(|| ViewerSnapshot::from_state(&state.peek()));
    use_effect(move || {
        let now = viewers();
        let (joined, left) = now.changes_from(&previous_viewers.peek());
        previous_viewers.set(now);
        if joined {
            sfx("viewer-join");
        }
        if left {
            sfx("viewer-leave");
        }
    });
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
