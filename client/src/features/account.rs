use dioxus::prelude::*;

use crate::identity::Identity;
use crate::session::SavedSession;
use crate::settings::ClientSettings;
use crate::state::{AppState, ConnectionStatus, SessionParams};

pub(super) fn use_account_state(settings: Signal<ClientSettings>) -> Signal<AppState> {
    // Social reads this from another VirtualDom; ROOT ownership avoids hoisting warnings.
    let state =
        use_hook(move || Signal::new_in_scope(initial_state(&settings.peek()), ScopeId::ROOT));
    use_drop(move || state.manually_drop());
    state
}

fn initial_state(saved: &ClientSettings) -> AppState {
    let mut s = AppState::empty();
    s.dm_cleared_at = saved.dm_cleared_at.iter().cloned().collect();
    s.dm_read_at = saved.dm_read_at.iter().cloned().collect();
    s.dm_clock_offset = saved.dm_clock_offset.iter().cloned().collect();
    s.muted_channels = saved.muted_channels.iter().copied().collect();
    s.muted_guilds = saved.muted_guilds.iter().copied().collect();
    s.mic_sensitivity = saved.mic_sensitivity.clamp(1, 1000);
    s.mic_volume = saved.mic_volume.min(200);
    s.auto_gain_control = saved.auto_gain_control;
    s.noise_cancellation = saved.noise_cancellation;
    s.bypass_system_audio_processing =
        saved.bypass_system_audio_processing && crate::rawmic::supported();
    s.denoise_atten_lim_db = saved.denoise_atten_lim_db.clamp(
        crate::features::voice::DENOISE_ATTEN_LIM_DB_MIN,
        crate::features::voice::DENOISE_ATTEN_LIM_DB_MAX,
    );
    s.selected_input_device = saved.selected_input_device.clone();
    s.selected_output_device = saved.selected_output_device.clone();
    s.user_volumes = saved.user_volumes.iter().cloned().collect();
    s.user_muted = saved.user_muted.iter().cloned().collect();
    s.stream_volumes = saved.stream_volumes.iter().cloned().collect();
    s.stream_muted = saved.stream_muted.iter().cloned().collect();
    s.voice_bitrate_kbps = match saved.voice_bitrate_kbps {
        24 => 24,
        _ => 48,
    };
    s.status = ConnectionStatus::Disconnected;
    s.soundboard_volume = saved.soundboard_volume.min(100) as u32;
    s
}

#[component]
pub fn AccountView(
    identity: Identity,
    session: Option<SessionParams>,
    error: Option<String>,
    last_session: Option<SavedSession>,
    on_connect: EventHandler<SessionParams>,
    on_disconnect: EventHandler<String>,
    on_rename: EventHandler<String>,
    on_sign_out: EventHandler<()>,
) -> Element {
    let settings = use_context::<Signal<ClientSettings>>();
    let mut state = use_account_state(settings);
    let nostr = use_hook(|| {
        let saved = settings.peek();
        let relays = if saved.dm_relays.is_empty() {
            crate::nostr::relay::DEFAULT_RELAYS
                .iter()
                .map(|url| (*url).to_string())
                .collect()
        } else {
            saved.dm_relays.clone()
        };
        crate::nostr::service::spawn_nostr(identity.clone(), relays, state)
    });
    provide_context(state);
    provide_context(nostr);
    provide_context(identity.clone());
    crate::state::use_dm_read_persistence(state);
    crate::state::use_dm_clock_persistence(state);
    crate::state::use_volume_persistence(state);
    rsx! {
        super::sounds::MessageSounds {}
        super::dm_call::CallAlert {}
        if let Some(params) = session {
            for params in std::iter::once(params) {
                super::workspace::WorkspaceView {
                    key: "{crate::app::session_key(&params)}",
                    params,
                    on_disconnect: move |reason| {
                        state.write().clear_server_session();
                        on_disconnect.call(reason);
                    },
                }
            }
        } else {
            super::home::HomeView { identity, error, last_session, on_connect, on_rename, on_sign_out }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::dm_call::{CallView, Phase};
    use crate::state::DmInfo;
    use std::{cell::RefCell, rc::Rc};

    #[test]
    fn preferences_are_restored_before_account_services_start() {
        let saved = ClientSettings {
            mic_volume: 160,
            mic_sensitivity: 42,
            selected_input_device: Some("Microphone".into()),
            user_volumes: vec![("peer".into(), 180)],
            dm_read_at: vec![("peer".into(), 123)],
            ..Default::default()
        };
        let state = initial_state(&saved);
        assert_eq!(state.mic_volume, 160);
        assert_eq!(state.mic_sensitivity, 42);
        assert_eq!(state.selected_input_device, saved.selected_input_device);
        assert_eq!(state.user_volumes["peer"], 180);
        assert_eq!(state.dm_read_at["peer"], 123);
    }

    type Export = Rc<RefCell<Option<(Signal<AppState>, Signal<u8>)>>>;

    #[allow(non_snake_case)]
    fn Child() -> Element {
        let state = crate::state::use_app_state();
        assert!(state.read().dm_call.is_some());
        rsx! { div {} }
    }

    #[test]
    fn account_state_survives_keyed_view_changes_and_server_cleanup() {
        let exported: Export = Rc::new(RefCell::new(None));
        let mut dom = VirtualDom::new_with_props(
            |export: Export| {
                let settings = use_signal(ClientSettings::default);
                let mut state = use_account_state(settings);
                let stage = use_signal(|| 0u8);
                use_hook(|| {
                    let mut s = state.write();
                    s.dm_call = Some(CallView {
                        peer: "peer".into(),
                        phase: Phase::Connected,
                        microphone_error: None,
                    });
                    s.voice.muted = true;
                    s.voice.deafened = true;
                    s.mic_volume = 170;
                    let id = crate::protocol::Id::new_v4();
                    s.dms.push(DmInfo {
                        channel_id: id,
                        other_pubkey: "peer".into(),
                    });
                    s.selected_channel = Some(id);
                    s.messages.insert(id, Vec::new());
                });
                provide_context(state);
                *export.borrow_mut() = Some((state, stage));
                rsx! { for key in std::iter::once(stage()) { Child { key: "{key}" } } }
            },
            exported.clone(),
        );
        dom.rebuild_in_place();
        let (mut state, mut stage) = exported.borrow().unwrap();
        for next in 1..=3 {
            state.write().status = ConnectionStatus::Ready;
            state
                .write()
                .messages
                .insert(crate::protocol::Id::new_v4(), Vec::new());
            state.write().clear_server_session();
            stage.set(next);
            dom.render_immediate_to_vec();
            assert!(exported.borrow().unwrap().0 == state);
            let s = state.read();
            assert_eq!(s.dm_call.as_ref().unwrap().phase, Phase::Connected);
            assert!(s.voice.muted && s.voice.deafened);
            assert_eq!(s.mic_volume, 170);
            assert_eq!(s.messages.len(), 1);
            assert!(s.dm_mode);
            assert_eq!(s.status, ConnectionStatus::Disconnected);
        }
    }
}
