//! Mounts `crate::push` in the running app: the account's device and taps in
//! `AccountView`, a session's handle and attention in `WorkspaceView`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use dioxus::prelude::*;
use tokio::sync::watch;
use tokio::time::Instant;

use crate::identity::Identity;
use crate::protocol::{ChannelKind, ClientMessage, Id};
use crate::push::attention::{Gate, Poll};
use crate::push::gateway::Live;
use crate::push::registration::RelayClient;
use crate::push::tap::{Here, PendingTap, Route};
use crate::settings::ClientSettings;
use crate::state::{AppState, ConnectionStatus, GatewayTx, SessionParams};

fn replace<T: PartialEq>(sender: &watch::Sender<T>, next: T) {
    sender.send_if_modified(|current| {
        let changed = *current != next;
        *current = next;
        changed
    });
}

fn relay() -> Option<RelayClient> {
    RelayClient::from_env()
        .inspect_err(|error| tracing::warn!(%error, "push: no usable relay; push stays off"))
        .ok()
}

/// The relay's view of this install, kept by the mounted account.
pub(crate) fn use_device(identity: &Identity, settings: Signal<ClientSettings>) {
    let relays = use_memo(move || super::account::dm_relays(&settings.read()));
    let identity = identity.clone();
    let sender = use_hook(move || {
        let (sender, receiver) = watch::channel(relays.peek().clone());
        if let Some(relay) = relay() {
            crate::service_task::spawn(crate::push::device::run(
                crate::identity::config_dir(),
                relay,
                identity,
                receiver,
            ));
        }
        Rc::new(sender)
    });
    use_effect(move || replace(&sender, relays()));
}

/// Called with the outgoing identity before it is dropped; the call outlives
/// the account's view, which unmounts as the sign-out lands.
pub(crate) fn sign_out(identity: Identity) {
    crate::push::device::retire();
    dioxus::core::spawn_forever(async move {
        crate::push::device::sign_out(&crate::identity::config_dir(), &identity).await;
    });
}

/// A saved server taken off the rail: its handle is revoked at the relay and
/// dropped from `push.json`, so that server can no longer wake this phone.
pub(crate) fn forget_server(identity: Identity, origin: String) {
    let Some(relay) = relay() else { return };
    let dir = crate::identity::config_dir();
    crate::service_task::spawn(async move {
        if let Err(error) =
            crate::push::registration::forget_origin(&dir, &relay, &identity, &origin).await
        {
            tracing::warn!(%error, "push: a removed server's handle was not revoked");
        }
    });
}

/// Gives this server the install's handle on every Ready (`push::gateway`).
pub(crate) fn use_mention_push(state: Signal<AppState>, gateway: GatewayTx, identity: Identity) {
    let session = use_memo(move || {
        let s = state.read();
        if s.status != ConnectionStatus::Ready {
            return None;
        }
        Some(Live {
            ready: s.readies,
            origin: s.server_origin.clone()?,
            advertised: s.server_info.push.clone(),
        })
    });
    let sender = use_hook(move || {
        let (sender, receiver) = watch::channel(None);
        if let Some(relay) = relay() {
            crate::service_task::spawn(crate::push::gateway::run(
                crate::identity::config_dir(),
                relay,
                identity,
                receiver,
                move |message| gateway.send(message),
            ));
        }
        Rc::new(sender)
    });
    use_effect(move || replace(&sender, session()));
}

/// Tells the server whether anyone is looking, so an unfocused desktop lets
/// the phone ring. Every platform: `app_foreground` covers all our windows.
pub(crate) fn use_attention(state: Signal<AppState>, gateway: GatewayTx) {
    let inputs = use_memo(move || {
        let s = state.read();
        // A server without push predates the message and answers it with an error.
        let listens = s.status == ConnectionStatus::Ready && s.server_info.push.is_some();
        (listens.then_some(s.readies), s.app_foreground)
    });
    let gate = use_hook(|| {
        Rc::new(RefCell::new(Gate::new(
            state.peek().app_foreground,
            Instant::now(),
        )))
    });
    let seen = use_hook(|| Rc::new(Cell::new(None::<u64>)));
    let mut timer = use_signal(|| None::<dioxus::core::Task>);
    use_effect(move || {
        let (ready, focused) = inputs();
        let now = Instant::now();
        let send = {
            let gateway = gateway.clone();
            move |attended| gateway.send(ClientMessage::SetAttention { attended })
        };
        {
            let mut gate = gate.borrow_mut();
            gate.focus(focused, now);
            if ready.is_none() {
                gate.lost();
            } else if ready != seen.get()
                && let Some(attended) = gate.ready()
            {
                send(attended);
            }
        }
        seen.set(ready);
        if let Some(task) = timer.write().take() {
            task.cancel();
        }
        let gate = gate.clone();
        let next = gate.borrow_mut().poll(now);
        match next {
            Poll::Send(attended) => send(attended),
            Poll::At(at) => timer.set(Some(spawn(async move {
                tokio::time::sleep_until(at).await;
                if let Poll::Send(attended) = gate.borrow_mut().poll(Instant::now()) {
                    send(attended);
                }
            }))),
            Poll::Idle => {}
        }
    });
}

/// Takes taps for the mounted account, and goes to their server from Home.
pub(crate) fn use_mention_taps(
    mut state: Signal<AppState>,
    account: String,
    identity: Memo<Identity>,
    at_home: bool,
    on_connect: EventHandler<SessionParams>,
) {
    let taker = account.clone();
    use_hook(move || {
        crate::service_task::spawn(async move {
            let mut taps = crate::push::tap::taps().subscribe();
            loop {
                let tap = taps.borrow_and_update().clone();
                if let Some(tap) = tap {
                    crate::push::tap::taps().send_if_modified(|held| {
                        let taken = held.as_ref() == Some(&tap);
                        if taken {
                            *held = None;
                        }
                        taken
                    });
                    if tap.account == taker {
                        state.write().mention_tap = Some(PendingTap {
                            tap,
                            launched: false,
                        });
                    }
                }
                if taps.changed().await.is_err() {
                    break;
                }
            }
        })
    });
    let pending = use_memo(move || state.read().mention_tap.clone());
    use_effect(use_reactive((&at_home,), move |(at_home,)| {
        let Some(pending) = pending() else { return };
        if !at_home {
            return;
        }
        let known = crate::known_servers::load();
        let here = Here {
            account: &account,
            at_home,
            live_origin: None,
            ready: false,
            in_call: false,
            switching: false,
            known: crate::push::tap::known_at(&known, &pending.tap.origin),
        };
        match crate::push::tap::route(&pending, &here) {
            Route::Connect(server) => {
                {
                    let mut s = state.write();
                    s.open_guild_on_ready = Some(pending.tap.guild);
                    s.mention_tap = Some(PendingTap {
                        launched: true,
                        ..pending
                    });
                }
                on_connect.call(SessionParams {
                    mode: server.mode,
                    identity: identity.peek().clone(),
                });
            }
            Route::Drop => state.write().mention_tap = None,
            _ => {}
        }
    }));
}

/// Lands a tap in this session, or hands it to the server rail.
pub(crate) fn use_mention_open(
    mut state: Signal<AppState>,
    gateway: GatewayTx,
    rail: super::server_rail::ServerRail,
    account: String,
) {
    let inputs = use_memo(move || {
        let s = state.read();
        let in_call = s.voice.channel_id.or(s.voice.joining_channel_id).is_some();
        s.mention_tap.clone().map(|pending| {
            (
                pending,
                s.status == ConnectionStatus::Ready,
                s.server_origin.clone(),
                in_call,
            )
        })
    });
    use_effect(move || {
        let Some((pending, ready, origin, in_call)) = inputs() else {
            return;
        };
        let switching = rail.switch.read().is_some();
        let known = rail.known.read().clone();
        let here = Here {
            account: &account,
            at_home: false,
            live_origin: origin.as_deref(),
            ready,
            in_call,
            switching,
            known: crate::push::tap::known_at(&known, &pending.tap.origin),
        };
        match crate::push::tap::route(&pending, &here) {
            Route::Select => {
                state.write().mention_tap = None;
                open_channel(&mut state, &gateway, pending.tap.guild, pending.tap.channel);
            }
            Route::Switch(server) => {
                state.write().mention_tap = Some(PendingTap {
                    launched: true,
                    ..pending.clone()
                });
                rail.request_switch(server, Some(pending.tap.guild));
            }
            Route::StayInCall => {
                let mut s = state.write();
                s.mention_tap = None;
                s.error_toast = Some(crate::i18n::text(
                    "Leave the voice call to open that mention.",
                    &[],
                ));
            }
            Route::Drop => state.write().mention_tap = None,
            Route::Wait | Route::Connect(_) => {}
        }
    });
}

/// As a click on the guild and then the channel would.
fn open_channel(state: &mut Signal<AppState>, gateway: &GatewayTx, guild: Id, channel: Id) {
    let target = {
        let mut s = state.write();
        if !s.guilds.iter().any(|g| g.id == guild) {
            return;
        }
        crate::channel_selection::remember_current(&mut s);
        s.dm_mode = false;
        s.selected_guild = Some(guild);
        s.selected_channel = None;
        let text = s
            .channels
            .iter()
            .any(|c| c.id == channel && c.guild_id == guild && c.kind == ChannelKind::Text);
        if text {
            Some(channel)
        } else {
            s.default_channel_of(guild)
        }
    };
    if let Some(channel) = target {
        super::channels::select_text_channel(state, gateway, channel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Channel, Guild};
    use crate::push::tap::MentionTap;
    use crate::state::SessionMode;

    type Export = Rc<RefCell<Option<Signal<AppState>>>>;

    const ORIGIN: &str = "chat.example:443";

    fn identity() -> Identity {
        Identity::restore_from_private_key(hex::encode([0x41; 32]), "me").unwrap()
    }

    fn channel(id: Id, guild_id: Id, kind: ChannelKind, position: u32) -> Channel {
        Channel {
            id,
            guild_id,
            name: format!("channel-{position}"),
            kind,
            topic: None,
            read_only: false,
            slowmode_secs: 0,
            position,
            access: None,
            divider: Default::default(),
            markdown: false,
        }
    }

    fn tap(channel: Id) -> PendingTap {
        PendingTap {
            tap: MentionTap {
                account: identity().pubkey,
                origin: ORIGIN.into(),
                guild: Id::from_u128(1),
                channel,
                message: Id::from_u128(9),
            },
            launched: false,
        }
    }

    fn ready_here() -> AppState {
        let guild = Id::from_u128(1);
        let mut state = AppState::empty();
        state.status = ConnectionStatus::Ready;
        state.readies = 1;
        state.server_origin = Some(ORIGIN.into());
        state.guilds = vec![
            serde_json::from_value::<Guild>(serde_json::json!({
                "id": guild, "name": "Guild", "icon": null,
            }))
            .unwrap(),
        ];
        state.channels = vec![
            channel(Id::from_u128(2), guild, ChannelKind::Text, 0),
            channel(Id::from_u128(3), guild, ChannelKind::Text, 1),
            channel(Id::from_u128(4), guild, ChannelKind::Voice, 2),
        ];
        state.dm_mode = true;
        state
    }

    fn harness(export: Export) -> Element {
        let state = use_signal(ready_here);
        *export.borrow_mut() = Some(state);
        let gateway = use_context::<GatewayTx>();
        let params = SessionParams {
            mode: SessionMode::Remote {
                server_url: "wss://chat.example".into(),
            },
            identity: identity(),
        };
        let rail = super::super::server_rail::use_server_rail(&params, state);
        use_attention(state, gateway.clone());
        use_mention_open(state, gateway, rail, identity().pubkey);
        rsx! {}
    }

    fn mount(
        known: &[crate::known_servers::KnownServer],
    ) -> (
        VirtualDom,
        Signal<AppState>,
        tokio::sync::mpsc::UnboundedReceiver<ClientMessage>,
    ) {
        crate::identity::test_config_dir();
        crate::known_servers::save(known);
        let export = Export::default();
        let (tx, messages) = tokio::sync::mpsc::unbounded_channel();
        let mut dom =
            VirtualDom::new_with_props(harness, export.clone()).with_root_context(GatewayTx(tx));
        dom.rebuild_in_place();
        dom.render_immediate_to_vec();
        let state = export.borrow().unwrap();
        (dom, state, messages)
    }

    fn drain(messages: &mut tokio::sync::mpsc::UnboundedReceiver<ClientMessage>) -> Vec<String> {
        std::iter::from_fn(|| messages.try_recv().ok())
            .map(|message| serde_json::to_value(message).unwrap()["op"].to_string())
            .collect()
    }

    #[tokio::test]
    async fn a_tap_on_the_live_server_opens_its_channel_and_leaves_dms() {
        let (mut dom, mut state, mut messages) = mount(&[]);
        drain(&mut messages);
        state.write().mention_tap = Some(tap(Id::from_u128(3)));
        dom.render_immediate_to_vec();
        let s = state.peek();
        assert!(s.mention_tap.is_none());
        assert!(!s.dm_mode);
        assert_eq!(s.selected_guild, Some(Id::from_u128(1)));
        assert_eq!(s.selected_channel, Some(Id::from_u128(3)));
        drop(s);
        assert_eq!(drain(&mut messages), vec!["\"fetch_messages\""]);

        state.write().mention_tap = Some(tap(Id::from_u128(4)));
        dom.render_immediate_to_vec();
        assert_eq!(
            state.peek().selected_channel,
            Some(Id::from_u128(3)),
            "a voice channel is not a chat to open; the guild's own choice stands"
        );
    }

    #[tokio::test]
    async fn a_tap_for_an_unknown_server_or_during_a_call_leaves_everything_alone() {
        const THERE: &str = "other.example:443";
        let known = crate::known_servers::KnownServer {
            origin: Some(THERE.into()),
            ..crate::known_servers::KnownServer::new(SessionMode::Remote {
                server_url: "wss://other.example".into(),
            })
        };
        let (mut dom, mut state, mut messages) = mount(&[known]);
        drain(&mut messages);
        let mut unknown = tap(Id::from_u128(3));
        unknown.tap.origin = "unknown.example:443".into();
        state.write().mention_tap = Some(unknown);
        dom.render_immediate_to_vec();
        assert!(state.peek().mention_tap.is_none());
        assert!(state.peek().dm_mode);

        let mut there = tap(Id::from_u128(3));
        there.tap.origin = THERE.into();
        state.write().voice.channel_id = Some(Id::from_u128(4));
        state.write().mention_tap = Some(there);
        dom.render_immediate_to_vec();
        let s = state.peek();
        assert!(s.mention_tap.is_none());
        assert!(s.error_toast.is_some(), "the call stays, and says why");
        assert!(s.dm_mode);
        drop(s);
        assert!(drain(&mut messages).is_empty());

        state.write().voice.channel_id = None;
        let mut there = tap(Id::from_u128(3));
        there.tap.origin = THERE.into();
        state.write().mention_tap = Some(there);
        dom.render_immediate_to_vec();
        assert!(
            state
                .peek()
                .mention_tap
                .as_ref()
                .is_some_and(|p| p.launched),
            "out of a call, the rail is asked to switch and the tap waits for it"
        );
        assert!(drain(&mut messages).is_empty(), "nothing is torn down here");
    }

    #[tokio::test]
    async fn a_server_without_push_is_never_told_about_attention() {
        let (mut dom, mut state, mut messages) = mount(&[]);
        state.write().server_info.push = None;
        dom.render_immediate_to_vec();
        state.write().app_foreground = false;
        dom.render_immediate_to_vec();
        tokio::time::sleep(crate::push::attention::DEBOUNCE * 2).await;
        dom.render_immediate_to_vec();
        assert!(drain(&mut messages).is_empty());
    }

    #[tokio::test]
    async fn attention_follows_focus_after_ready_without_a_message_per_flap() {
        let (mut dom, mut state, mut messages) = mount(&[]);
        state.write().server_info.push = Some("https://push.example".into());
        dom.render_immediate_to_vec();
        assert!(
            drain(&mut messages).is_empty(),
            "focused: the default holds"
        );
        state.write().app_foreground = false;
        dom.render_immediate_to_vec();
        state.write().app_foreground = true;
        dom.render_immediate_to_vec();
        tokio::time::sleep(crate::push::attention::DEBOUNCE * 2).await;
        dom.render_immediate_to_vec();
        assert_eq!(drain(&mut messages), Vec::<String>::new());

        state.write().app_foreground = false;
        state.write().status = ConnectionStatus::Reconnecting;
        dom.render_immediate_to_vec();
        {
            let mut s = state.write();
            s.status = ConnectionStatus::Ready;
            s.readies += 1;
        }
        dom.render_immediate_to_vec();
        assert_eq!(drain(&mut messages), vec!["\"set_attention\""]);
    }
}
