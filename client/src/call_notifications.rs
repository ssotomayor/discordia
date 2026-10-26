use crate::features::dm_call::Phase;
use crate::protocol::Id;
use crate::state::{AppState, VoicePhase};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ControlAction {
    Mute,
    Unmute,
    Deafen,
    Undeafen,
    Hangup,
}

impl ControlAction {
    fn from_code(code: i32) -> Option<Self> {
        match code {
            1 => Some(Self::Mute),
            2 => Some(Self::Unmute),
            3 => Some(Self::Deafen),
            4 => Some(Self::Undeafen),
            5 => Some(Self::Hangup),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Call {
    Guild { channel: Id, epoch: u64 },
    Dm(Uuid),
}

fn current_call(state: &AppState) -> Option<Call> {
    if let Some(call) = &state.dm_call {
        match call.phase {
            Phase::Incoming => {}
            Phase::Switching => return None,
            _ => return Some(Call::Dm(call.call_id)),
        }
    }
    if matches!(
        state.voice.phase,
        VoicePhase::Connecting | VoicePhase::Connected
    ) {
        state.voice.channel_id.map(|channel| Call::Guild {
            channel,
            epoch: state.voice_session_epoch,
        })
    } else {
        None
    }
}

fn token(owner: Uuid, route: Option<Uuid>, state: &AppState) -> Option<String> {
    let call = match current_call(state)? {
        Call::Guild { channel, epoch } => format!("guild:{}:{channel}:{epoch}", route?),
        Call::Dm(id) => format!("dm:{id}"),
    };
    Some(format!(
        "{owner}:{call}:{}:{}",
        u8::from(state.voice.muted),
        u8::from(state.voice.deafened)
    ))
}

fn apply(
    state: &mut AppState,
    owner: Uuid,
    route: Option<Uuid>,
    expected: &str,
    action: ControlAction,
) -> Option<Call> {
    if token(owner, route, state).as_deref() != Some(expected) {
        return None;
    }
    let call = current_call(state)?;
    match action {
        ControlAction::Hangup => {}
        ControlAction::Mute | ControlAction::Unmute => {
            let muted = action == ControlAction::Mute;
            if state.voice.deafened || state.voice.muted == muted {
                return None;
            }
            state.voice.muted = muted;
        }
        ControlAction::Deafen | ControlAction::Undeafen => {
            let deafened = action == ControlAction::Deafen;
            if state.voice.deafened == deafened {
                return None;
            }
            let (muted, deafened) = state.voice.toggle_deafen();
            state.voice.muted = muted;
            state.voice.deafened = deafened;
        }
    }
    Some(call)
}

#[cfg(target_os = "android")]
pub(crate) use native::{use_controls, use_guild_route, wake};

#[cfg(target_os = "android")]
mod native {
    use super::*;
    use crate::features::voice::{VoiceCmd, VoiceTx};
    use crate::nostr::service::{NostrCmd, NostrTx};
    use crate::protocol::ClientMessage;
    use crate::state::GatewayTx;
    use dioxus::prelude::*;
    use jni::{
        JNIEnv,
        objects::{JObject, JString, JValue},
    };
    use std::{cell::RefCell, rc::Rc, sync::OnceLock, time::Duration};
    use tokio::sync::mpsc;

    enum Event {
        Refresh,
        Action {
            token: String,
            action: ControlAction,
        },
    }

    struct Registration {
        owner: Uuid,
        events: mpsc::Sender<Event>,
    }

    fn registration() -> &'static parking_lot::Mutex<Option<Registration>> {
        static REGISTRATION: OnceLock<parking_lot::Mutex<Option<Registration>>> = OnceLock::new();
        REGISTRATION.get_or_init(Default::default)
    }

    pub fn wake() {
        if let Some(registration) = registration().lock().as_ref() {
            match registration.events.try_send(Event::Refresh) {
                Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => {}
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    tracing::debug!("Call notification owner stopped")
                }
            }
        }
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_com_discordia_app_DioxusForegroundService_onCallAction(
        mut env: JNIEnv<'_>,
        _service: JObject<'_>,
        key: JString<'_>,
        code: jni::sys::jint,
    ) {
        let Some(action) = ControlAction::from_code(code) else {
            return;
        };
        let key = env.get_string(&key).map(String::from);
        if env.exception_check().unwrap_or(false)
            && let Err(error) = env.exception_clear()
        {
            tracing::warn!(%error, "Could not clear call action JNI exception");
        }
        if let Ok(token) = key
            && token.len() <= 256
            && let Some(current) = registration().lock().as_ref()
        {
            match current.events.try_send(Event::Action { token, action }) {
                Ok(()) => {}
                Err(error) => tracing::debug!(%error, "Call notification action queue unavailable"),
            }
        }
    }

    #[derive(Clone)]
    struct Route {
        id: Uuid,
        gateway: GatewayTx,
        voice: VoiceTx,
    }

    #[derive(Clone)]
    struct Bridge {
        routes: Rc<RefCell<Option<Route>>>,
        events: mpsc::Sender<Event>,
    }

    struct GuildBinding {
        id: Uuid,
        bridge: Bridge,
    }

    impl Drop for GuildBinding {
        fn drop(&mut self) {
            if self
                .bridge
                .routes
                .borrow()
                .as_ref()
                .is_some_and(|route| route.id == self.id)
            {
                self.bridge.routes.borrow_mut().take();
                if let Err(error) = self.bridge.events.try_send(Event::Refresh) {
                    tracing::debug!(%error, "Call route owner stopped");
                }
            }
        }
    }

    pub fn use_guild_route(gateway: GatewayTx, voice: VoiceTx) {
        let bridge = use_context::<Bridge>();
        use_hook(move || {
            let id = Uuid::new_v4();
            bridge.routes.replace(Some(Route { id, gateway, voice }));
            if let Err(error) = bridge.events.try_send(Event::Refresh) {
                tracing::debug!(%error, "Could not announce call route");
            }
            Rc::new(GuildBinding { id, bridge })
        });
    }

    #[derive(serde::Serialize, PartialEq)]
    struct Notice {
        token: Option<String>,
        muted: bool,
        deafened: bool,
        channel: String,
        voice: String,
        camera: String,
        screen: String,
        mute: String,
        sound: String,
        hangup: String,
        muted_status: String,
        deafened_status: String,
    }

    fn notice(state: &AppState, owner: Uuid, route: Option<Uuid>, ending: Option<&str>) -> Notice {
        let token = token(owner, route, state).filter(|key| ending != Some(key.as_str()));
        let text = |key| crate::i18n::text(key, &[]);
        Notice {
            token,
            muted: state.voice.muted || state.voice.deafened,
            deafened: state.voice.deafened,
            channel: text("Calls and sharing"),
            voice: text("Voice call"),
            camera: text("Camera active"),
            screen: text("Sharing your screen"),
            mute: text(if state.voice.muted || state.voice.deafened {
                "Unmute mic"
            } else {
                "Mute mic"
            }),
            sound: text(if state.voice.deafened {
                "Unmute call audio"
            } else {
                "Mute call audio"
            }),
            hangup: text("End call"),
            muted_status: text("Microphone muted"),
            deafened_status: text("Call audio muted"),
        }
    }

    fn publish(notice: Option<&Notice>) {
        let payload = match serde_json::to_string(&notice) {
            Ok(payload) => payload,
            Err(error) => {
                tracing::warn!(%error, "Could not prepare call notification");
                return;
            }
        };
        crate::platform::wry::prelude::dispatch(move |env, activity, _| {
            let result = env.new_string(payload).and_then(|value| {
                env.call_method(
                    activity,
                    "updateCallNotification",
                    "(Ljava/lang/String;)V",
                    &[JValue::Object(&value)],
                )
                .map(|_| ())
            });
            if let Err(error) = result {
                tracing::warn!(%error, "Could not update call notification");
            }
            if env.exception_check().unwrap_or(false)
                && let Err(error) = env.exception_clear()
            {
                tracing::warn!(%error, "Could not clear call notification exception");
            }
        });
    }

    pub fn use_controls(mut state: Signal<AppState>, nostr: NostrTx) {
        let (owner, bridge) = use_hook(move || {
            let owner = Uuid::new_v4();
            let (events, mut incoming) = mpsc::channel::<Event>(32);
            *registration().lock() = Some(Registration {
                owner,
                events: events.clone(),
            });
            let bridge = Bridge {
                routes: Rc::new(RefCell::new(None)),
                events,
            };
            let routes = bridge.routes.clone();
            crate::service_task::spawn(async move {
                let mut ending: Option<String> = None;
                let mut sent = None;
                let mut tick = tokio::time::interval(Duration::from_millis(500));
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    let route = routes
                        .borrow()
                        .clone()
                        .filter(|route| !route.gateway.0.is_closed() && !route.voice.0.is_closed());
                    let current = notice(
                        &state.peek(),
                        owner,
                        route.as_ref().map(|route| route.id),
                        ending.as_deref(),
                    );
                    let active = current_call(&state.peek()).is_some();
                    if sent.as_ref() != Some(&current) {
                        publish(Some(&current));
                        sent = Some(current);
                    }
                    let event = tokio::select! {
                        event = incoming.recv() => match event { Some(event) => event, None => break },
                        _ = tick.tick(), if active => Event::Refresh,
                    };
                    if let Event::Action { token, action } = event {
                        if ending.as_deref() == Some(&token) {
                            continue;
                        }
                        let route = routes.borrow().clone().filter(|route| {
                            !route.gateway.0.is_closed() && !route.voice.0.is_closed()
                        });
                        let Some(call) = apply(
                            &mut state.write(),
                            owner,
                            route.as_ref().map(|route| route.id),
                            &token,
                            action,
                        ) else {
                            continue;
                        };
                        match call {
                            Call::Dm(_) if action == ControlAction::Hangup => {
                                if nostr
                                    .try_send(NostrCmd::Call(crate::features::dm_call::Action::End))
                                {
                                    ending = Some(token);
                                }
                            }
                            Call::Dm(_) => {}
                            Call::Guild { .. } => {
                                if let Some(route) = route {
                                    if action == ControlAction::Hangup {
                                        route.gateway.send(ClientMessage::LeaveVoice);
                                        route.voice.send(VoiceCmd::Disconnect { done: None });
                                        ending = Some(token);
                                    } else {
                                        let voice = state.peek().voice.clone();
                                        route.gateway.send(ClientMessage::SetVoiceMute {
                                            muted: voice.muted,
                                            deafened: voice.deafened,
                                        });
                                        route.voice.send(VoiceCmd::SetDeafen {
                                            deafened: voice.deafened,
                                        });
                                        route.voice.send(VoiceCmd::SetMute { muted: voice.muted });
                                    }
                                }
                            }
                        }
                    }
                }
            });
            (owner, bridge)
        });
        provide_context(bridge.clone());
        use_effect(move || {
            let _state = state.read();
            if let Err(error) = bridge.events.try_send(Event::Refresh) {
                tracing::debug!(%error, "Could not refresh call controls");
            }
        });
        use_drop(move || {
            let mut current = registration().lock();
            if current
                .as_ref()
                .is_some_and(|current| current.owner == owner)
            {
                current.take();
                publish(None);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::dm_call::CallView;

    fn guild() -> AppState {
        let mut state = AppState::empty();
        state.voice.phase = VoicePhase::Connected;
        state.voice.channel_id = Some(Uuid::new_v4());
        state
    }

    #[test]
    fn codes_are_explicit_and_unknown_actions_are_rejected() {
        for (code, action) in [
            (1, ControlAction::Mute),
            (2, ControlAction::Unmute),
            (3, ControlAction::Deafen),
            (4, ControlAction::Undeafen),
            (5, ControlAction::Hangup),
        ] {
            assert_eq!(ControlAction::from_code(code), Some(action));
        }
        assert_eq!(ControlAction::from_code(0), None);
        assert_eq!(ControlAction::from_code(6), None);
    }

    #[test]
    fn stale_owner_route_channel_epoch_or_status_cannot_control_current_call() {
        let state = guild();
        let owner = Uuid::new_v4();
        let route = Some(Uuid::new_v4());
        let key = token(owner, route, &state).unwrap();
        assert!(
            apply(
                &mut state.clone(),
                Uuid::new_v4(),
                route,
                &key,
                ControlAction::Hangup
            )
            .is_none()
        );
        assert!(
            apply(
                &mut state.clone(),
                owner,
                Some(Uuid::new_v4()),
                &key,
                ControlAction::Hangup
            )
            .is_none()
        );
        assert!(apply(&mut state.clone(), owner, None, &key, ControlAction::Hangup).is_none());
        let mut changed = state.clone();
        changed.voice_session_epoch += 1;
        assert!(apply(&mut changed, owner, route, &key, ControlAction::Hangup).is_none());
        changed = state.clone();
        changed.voice.channel_id = Some(Uuid::new_v4());
        assert!(apply(&mut changed, owner, route, &key, ControlAction::Hangup).is_none());
        changed = state.clone();
        changed.voice.muted = true;
        assert!(apply(&mut changed, owner, route, &key, ControlAction::Unmute).is_none());
    }

    #[test]
    fn deafen_restores_prior_mic_state_and_disallows_unmute_while_deafened() {
        for muted in [false, true] {
            let mut state = guild();
            state.voice.muted = muted;
            let owner = Uuid::new_v4();
            let route = Some(Uuid::new_v4());
            let key = token(owner, route, &state).unwrap();
            assert!(apply(&mut state, owner, route, &key, ControlAction::Deafen).is_some());
            assert!(state.voice.muted && state.voice.deafened);
            let key = token(owner, route, &state).unwrap();
            assert!(apply(&mut state, owner, route, &key, ControlAction::Unmute).is_none());
            assert!(apply(&mut state, owner, route, &key, ControlAction::Undeafen).is_some());
            assert_eq!(state.voice.muted, muted);
            assert!(!state.voice.deafened);
        }
    }

    #[test]
    fn mic_actions_are_idempotent_and_hangup_preserves_state_until_teardown() {
        let mut state = guild();
        let owner = Uuid::new_v4();
        let route = Some(Uuid::new_v4());
        for action in [ControlAction::Mute, ControlAction::Unmute] {
            let key = token(owner, route, &state).unwrap();
            assert!(apply(&mut state, owner, route, &key, action).is_some());
            let key = token(owner, route, &state).unwrap();
            assert!(apply(&mut state, owner, route, &key, action).is_none());
        }
        let key = token(owner, route, &state).unwrap();
        assert!(matches!(
            apply(&mut state, owner, route, &key, ControlAction::Hangup),
            Some(Call::Guild { .. })
        ));
        assert_eq!(state.voice.phase, VoicePhase::Connected);
        state.voice.phase = VoicePhase::Idle;
        assert!(apply(&mut state, owner, route, &key, ControlAction::Hangup).is_none());
    }

    #[test]
    fn dm_controls_follow_call_identity_and_survive_guild_navigation() {
        let mut state = AppState::empty();
        state.dm_call = Some(CallView {
            call_id: Uuid::new_v4(),
            peer: "peer".into(),
            phase: Phase::Connected,
            microphone_error: None,
        });
        let owner = Uuid::new_v4();
        let key = token(owner, None, &state).unwrap();
        assert!(matches!(
            apply(
                &mut state,
                owner,
                Some(Uuid::new_v4()),
                &key,
                ControlAction::Hangup
            ),
            Some(Call::Dm(_))
        ));
        state.dm_call.as_mut().unwrap().call_id = Uuid::new_v4();
        assert!(apply(&mut state, owner, None, &key, ControlAction::Hangup).is_none());
        state.dm_call.as_mut().unwrap().phase = Phase::Incoming;
        assert!(token(owner, None, &state).is_none());
        assert!(token(owner, None, &AppState::empty()).is_none());
    }
    #[test]
    fn incoming_preserves_guild_controls_but_switching_rejects_them() {
        let mut state = guild();
        let owner = Uuid::new_v4();
        let route = Some(Uuid::new_v4());
        let key = token(owner, route, &state).unwrap();
        state.dm_call = Some(CallView {
            call_id: Uuid::new_v4(),
            peer: "peer".into(),
            phase: Phase::Incoming,
            microphone_error: None,
        });
        assert_eq!(token(owner, route, &state).as_deref(), Some(key.as_str()));
        state.dm_call.as_mut().unwrap().phase = Phase::Switching;
        assert!(token(owner, route, &state).is_none());
        assert!(apply(&mut state, owner, route, &key, ControlAction::Hangup).is_none());
    }
}
