use std::collections::BTreeSet;

use dioxus::prelude::*;

use crate::connection_routes::MediaRoute;
use crate::protocol::{ClientMessage, Id};
use crate::state::{AppState, ConnectionStatus, GatewayTx, Transport, VoicePhase};

#[derive(Clone, PartialEq)]
struct RouteReport {
    channel: Id,
    gateway: String,
    sent: Option<MediaRoute>,
    received: Option<MediaRoute>,
    phase: VoicePhase,
    endpoint: Option<String>,
    revision: u32,
}

#[derive(Clone, PartialEq)]
struct Snapshot {
    report: RouteReport,
    peers: BTreeSet<String>,
}

impl Snapshot {
    fn from_state(state: &AppState) -> Option<Self> {
        if state.status != ConnectionStatus::Ready {
            return None;
        }
        let channel = state.server_voice_channel()?;
        Some(Self {
            report: RouteReport {
                channel,
                gateway: state.gateway_route.clone().unwrap_or_else(|| {
                    match state.transport {
                        Transport::Loopback => "local",
                        Transport::Quic => "direct · IP unknown",
                        Transport::QuicRelayed => "relay",
                        Transport::Proxied => "proxied",
                    }
                    .into()
                }),
                sent: state.voice_send_route.clone(),
                received: state.voice_receive_route.clone(),
                phase: state.voice.phase,
                endpoint: state.voice_endpoint.clone(),
                revision: state.voice_route_revision,
            },
            peers: state
                .voice_states
                .iter()
                .filter(|peer| peer.channel_id == Some(channel))
                .map(|peer| peer.user_pubkey.clone())
                .collect(),
        })
    }
}

#[derive(Default)]
struct Reporter {
    previous: Option<Snapshot>,
}

impl Reporter {
    fn update(&mut self, next: Option<Snapshot>) -> Option<ClientMessage> {
        let changed = next.as_ref().is_some_and(|next| {
            self.previous.as_ref().is_none_or(|previous| {
                next.report != previous.report || !next.peers.is_subset(&previous.peers)
            })
        });
        self.previous = next;
        let report = &self.previous.as_ref().filter(|_| changed)?.report;
        Some(ClientMessage::ConnectionRouteReport {
            channel_id: report.channel,
            gateway: report.gateway.clone(),
            voice_send: report.sent.as_ref().map(MediaRoute::label),
            voice_receive: report.received.as_ref().map(MediaRoute::label),
        })
    }
}

pub fn use_route_reports(state: Signal<AppState>, gateway: GatewayTx) {
    let snapshot = use_memo(move || Snapshot::from_state(&state.read()));
    let reporter = use_hook(|| std::rc::Rc::new(std::cell::RefCell::new(Reporter::default())));
    use_effect(move || {
        if let Some(report) = reporter.borrow_mut().update(snapshot()) {
            gateway.send(report);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> Snapshot {
        let route = MediaRoute {
            family: "IPv4",
            relayed: false,
            protocol: "udp".into(),
            endpoint: "203.0.113.9:7882".into(),
        };
        Snapshot {
            report: RouteReport {
                channel: Id::new_v4(),
                gateway: "direct · IPv4".into(),
                sent: Some(route.clone()),
                received: Some(route),
                phase: VoicePhase::Connected,
                endpoint: Some("wss://voice.example".into()),
                revision: 0,
            },
            peers: BTreeSet::from(["self".into()]),
        }
    }

    #[test]
    fn stable_routes_stay_quiet_and_new_callers_receive_a_fresh_report() {
        let mut reporter = Reporter::default();
        let mut current = snapshot();
        assert!(reporter.update(Some(current.clone())).is_some());
        for _ in 0..20 {
            assert!(reporter.update(Some(current.clone())).is_none());
        }
        current.peers.insert("new caller".into());
        assert!(reporter.update(Some(current.clone())).is_some());
        current.peers.remove("new caller");
        assert!(reporter.update(Some(current.clone())).is_none());
        current.peers.insert("new caller".into());
        assert!(reporter.update(Some(current)).is_some());
    }

    #[test]
    fn route_changes_reconnections_and_channel_moves_resend_without_addresses() {
        let mut reporter = Reporter::default();
        let mut current = snapshot();
        assert!(reporter.update(Some(current.clone())).is_some());
        current.report.sent.as_mut().unwrap().relayed = true;
        let message = reporter.update(Some(current.clone())).unwrap();
        let encoded = serde_json::to_string(&message).unwrap();
        assert!(encoded.contains("TURN relay"));
        assert!(!encoded.contains("203.0.113.9"));
        assert!(!encoded.contains("voice.example"));
        current.report.gateway = "relay".into();
        assert!(reporter.update(Some(current.clone())).is_some());
        current.report.received = None;
        current.report.phase = VoicePhase::Connecting;
        assert!(reporter.update(Some(current.clone())).is_some());
        current.report.phase = VoicePhase::Connected;
        assert!(reporter.update(Some(current.clone())).is_some());
        assert!(reporter.update(None).is_none());
        assert!(reporter.update(Some(current.clone())).is_some());
        current.report.channel = Id::new_v4();
        assert!(reporter.update(Some(current.clone())).is_some());
        current.report.revision += 1;
        assert!(reporter.update(Some(current)).is_some());
    }

    #[tokio::test]
    async fn reactive_reports_ignore_chat_and_voice_flags_but_follow_routes_and_membership() {
        type Export = std::rc::Rc<std::cell::RefCell<Option<Signal<AppState>>>>;
        fn harness(export: Export) -> Element {
            let state = use_signal(|| {
                let mut state = AppState::empty();
                state.status = ConnectionStatus::Ready;
                state.self_user = Some(crate::protocol::User {
                    pubkey: "self".into(),
                    username: "Self".into(),
                });
                state.voice_states.push(crate::protocol::VoiceState {
                    user_pubkey: "self".into(),
                    guild_id: Id::nil(),
                    channel_id: Some(Id::nil()),
                    muted: false,
                    deafened: false,
                    speaking: false,
                    camera_on: false,
                    screen_sharing: false,
                    screen_watching: Vec::new(),
                });
                state
            });
            *export.borrow_mut() = Some(state);
            use_route_reports(state, use_context());
            rsx! {}
        }
        let export = Export::default();
        let (tx, mut messages) = tokio::sync::mpsc::unbounded_channel();
        let mut dom =
            VirtualDom::new_with_props(harness, export.clone()).with_root_context(GatewayTx(tx));
        dom.rebuild_in_place();
        dom.render_immediate_to_vec();
        let mut state = export.borrow().unwrap();
        assert!(matches!(
            messages.try_recv().unwrap(),
            ClientMessage::ConnectionRouteReport { channel_id, .. } if channel_id == Id::nil()
        ));
        for _ in 0..5 {
            {
                let mut state = state.write();
                state.error_toast = Some("Unrelated chat update".into());
                state.voice_states[0].muted = !state.voice_states[0].muted;
                state.voice_states[0].speaking = !state.voice_states[0].speaking;
            }
            dom.render_immediate_to_vec();
            assert!(messages.try_recv().is_err());
        }
        let mut peer = state.peek().voice_states[0].clone();
        peer.user_pubkey = "new caller".into();
        state.write().voice_states.push(peer);
        dom.render_immediate_to_vec();
        assert!(messages.try_recv().is_ok());
        assert!(messages.try_recv().is_err());
        state.write().voice_states.pop();
        dom.render_immediate_to_vec();
        assert!(messages.try_recv().is_err());
        state.write().gateway_route = Some("relay".into());
        dom.render_immediate_to_vec();
        assert!(matches!(
            messages.try_recv().unwrap(),
            ClientMessage::ConnectionRouteReport { gateway, .. } if gateway == "relay"
        ));
        state.write().status = ConnectionStatus::Reconnecting;
        dom.render_immediate_to_vec();
        assert!(messages.try_recv().is_err());
        state.write().status = ConnectionStatus::Ready;
        dom.render_immediate_to_vec();
        assert!(messages.try_recv().is_ok());
        state.write().voice_states[0].channel_id = None;
        dom.render_immediate_to_vec();
        assert!(messages.try_recv().is_err());
        assert!(Snapshot::from_state(&state.peek()).is_none());
    }
}
