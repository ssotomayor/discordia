use crate::features::voice::{VoiceCmd, VoiceTx};
use crate::protocol::ClientMessage;
use crate::state::{AppState, GatewayTx};
use dioxus::prelude::*;
use std::{cell::RefCell, future::Future, rc::Rc, time::Duration};
use tokio::sync::oneshot;
use uuid::Uuid;

#[derive(Clone, Default)]
pub(crate) struct Bridge(Rc<RefCell<Option<Route>>>);

#[derive(Clone)]
struct Route {
    id: Uuid,
    gateway: GatewayTx,
    voice: VoiceTx,
}

struct Binding {
    bridge: Bridge,
    id: Uuid,
}

impl Drop for Binding {
    fn drop(&mut self) {
        let current = self
            .bridge
            .0
            .borrow()
            .as_ref()
            .is_some_and(|route| route.id == self.id);
        if current {
            self.bridge.0.borrow_mut().take();
        }
    }
}

pub(crate) fn use_bridge() {
    let bridge = use_hook(Bridge::default);
    provide_context(bridge);
}

pub(crate) fn use_route(gateway: GatewayTx, voice: VoiceTx) {
    let bridge = use_context::<Bridge>();
    use_hook(move || {
        let id = Uuid::new_v4();
        bridge.0.replace(Some(Route { id, gateway, voice }));
        Rc::new(Binding { bridge, id })
    });
}

impl Bridge {
    pub async fn leave(&self, state: Signal<AppState>) -> Result<(), String> {
        self.leave_with(state, async {
            let mut stop = document::eval(crate::features::workspace::STOP_WEBVIEW_MEDIA_JS);
            stop.recv::<bool>()
                .await
                .map(|_| ())
                .map_err(|_| "Could not stop the voice channel media.".to_string())
        })
        .await
    }

    async fn leave_with(
        &self,
        mut state: Signal<AppState>,
        stop: impl Future<Output = Result<(), String>>,
    ) -> Result<(), String> {
        let route = self
            .0
            .borrow()
            .clone()
            .ok_or("The voice channel is no longer available.")?;
        let channel = state.peek().voice.channel_id;
        route
            .gateway
            .0
            .send(ClientMessage::LeaveVoice)
            .map_err(|_| "Could not leave the voice channel.")?;
        let (done, stopped) = oneshot::channel();
        route
            .voice
            .0
            .send(VoiceCmd::Disconnect { done: Some(done) })
            .map_err(|_| "Could not stop the voice channel audio.")?;
        let teardown = async {
            tokio::try_join!(stop, async {
                stopped
                    .await
                    .map_err(|_| "Could not stop the voice channel audio.".to_string())
            })?;
            loop {
                if self
                    .0
                    .borrow()
                    .as_ref()
                    .is_none_or(|current| current.id != route.id)
                {
                    return Err("The server changed while answering the call.".to_string());
                }
                if state.peek().server_voice_channel().is_none() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            if state
                .peek()
                .voice
                .channel_id
                .is_some_and(|current| Some(current) != channel)
            {
                return Err(
                    "Another voice channel was joined while answering the call.".to_string()
                );
            }
            state.write().end_voice_locally();
            Ok(())
        };
        tokio::time::timeout(Duration::from_secs(10), teardown)
            .await
            .map_err(|_| "Leaving the voice channel timed out.".to_string())?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{User, VoiceState};
    use crate::state::VoicePhase;
    use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

    type Fixture = (
        VirtualDom,
        Signal<AppState>,
        Bridge,
        UnboundedReceiver<ClientMessage>,
        UnboundedReceiver<VoiceCmd>,
    );

    fn setup() -> Fixture {
        let mut dom = VirtualDom::new(|| rsx! {});
        dom.rebuild_in_place();
        let state = dom.in_scope(ScopeId::ROOT, || {
            let mut state = AppState::empty();
            let channel = Uuid::new_v4();
            state.self_user = Some(User {
                pubkey: "me".into(),
                username: "me".into(),
            });
            state.voice_states.push(VoiceState {
                user_pubkey: "me".into(),
                guild_id: Uuid::new_v4(),
                channel_id: Some(channel),
                muted: false,
                deafened: false,
                speaking: false,
                camera_on: true,
                screen_sharing: true,
                screen_watching: vec![],
            });
            state.voice.phase = VoicePhase::Connected;
            state.voice.channel_id = Some(channel);
            state.screen_sharing = true;
            state.camera_on = true;
            Signal::new(state)
        });
        let (gateway, messages) = unbounded_channel();
        let (voice, commands) = unbounded_channel();
        let bridge = Bridge::default();
        bridge.0.replace(Some(Route {
            id: Uuid::new_v4(),
            gateway: GatewayTx(gateway),
            voice: VoiceTx(voice),
        }));
        (dom, state, bridge, messages, commands)
    }

    #[tokio::test]
    async fn handoff_waits_for_native_webview_and_server_before_clearing_media() {
        let (_dom, mut state, bridge, mut messages, mut commands) = setup();
        let (media, media_done) = oneshot::channel();
        let mut handoff = Box::pin(bridge.leave_with(state, async {
            media_done.await.map_err(|_| "media failed".into())
        }));
        assert!(futures_util::poll!(&mut handoff).is_pending());
        assert!(matches!(messages.try_recv(), Ok(ClientMessage::LeaveVoice)));
        let VoiceCmd::Disconnect { done: Some(done) } = commands.try_recv().unwrap() else {
            panic!("native teardown missing");
        };
        done.send(()).unwrap();
        assert!(futures_util::poll!(&mut handoff).is_pending());
        assert!(state.peek().camera_on);
        media.send(()).unwrap();
        assert!(futures_util::poll!(&mut handoff).is_pending());
        assert!(state.peek().screen_sharing);
        state.write().voice_states.clear();
        handoff.await.unwrap();
        assert_eq!(state.peek().voice.phase, VoicePhase::Idle);
        assert!(!state.peek().screen_sharing && !state.peek().camera_on);
    }

    #[tokio::test]
    async fn replaced_route_or_failed_media_never_complete_the_handoff() {
        for replaced in [false, true] {
            let (_dom, state, bridge, _messages, mut commands) = setup();
            let (media, media_done) = oneshot::channel();
            let mut handoff =
                Box::pin(bridge.leave_with(state, async { media_done.await.unwrap() }));
            assert!(futures_util::poll!(&mut handoff).is_pending());
            let VoiceCmd::Disconnect { done: Some(done) } = commands.try_recv().unwrap() else {
                panic!("native teardown missing");
            };
            done.send(()).unwrap();
            if replaced {
                bridge.0.borrow_mut().as_mut().unwrap().id = Uuid::new_v4();
                media.send(Ok(())).unwrap();
            } else {
                media.send(Err("media failed".into())).unwrap();
            }
            assert!(handoff.await.is_err());
            assert_eq!(state.peek().voice.phase, VoicePhase::Connected);
        }
    }
}
