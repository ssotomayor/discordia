use super::*;
use crate::nostr::nip02::Contact;
use futures_util::StreamExt;
use libwebrtc::{
    audio_frame::AudioFrame,
    audio_source::{AudioSourceOptions, native::NativeAudioSource},
    audio_stream::native::NativeAudioStream,
    peer_connection_factory::native::PeerConnectionFactoryExt,
};

fn harness(
    test: impl FnOnce(&mut Actor, &mut tokio::sync::mpsc::UnboundedReceiver<(String, CallSignal)>),
) {
    let mut dom = VirtualDom::new(|| rsx! {});
    dom.rebuild_in_place();
    dom.in_scope(ScopeId::ROOT, || {
        let mut initial = AppState::empty();
        initial.contacts.contacts.push(Contact {
            pubkey: "22".repeat(32),
            relay: None,
            petname: None,
        });
        let state = Signal::new(initial);
        let (outgoing, mut output) = unbounded_channel();
        let (tx, _rx) = unbounded_channel();
        let mut actor = Actor {
            our_key: "11".repeat(32),
            device: Uuid::new_v4(),
            state,
            outgoing,
            tx,
            session: None,
            finished: HashMap::new(),
        };
        test(&mut actor, &mut output);
    });
}

fn incoming(id: Uuid, device: Uuid, body: Body) -> Incoming {
    Incoming {
        author: "22".repeat(32),
        signal: CallSignal {
            version: 1,
            call_id: id,
            device,
            target: None,
            sent_at: chrono::Utc::now().timestamp(),
            body,
        },
    }
}

#[test]
fn incoming_requires_consent_and_duplicate_invites_do_not_extend_deadline() {
    harness(|actor, output| {
        let id = Uuid::new_v4();
        let device = Uuid::new_v4();
        actor.incoming(incoming(id, device, Body::Invite));
        assert_eq!(actor.session.as_ref().unwrap().phase, Phase::Incoming);
        assert!(actor.session.as_ref().unwrap().audio.is_none());
        assert!(output.try_recv().is_err());
        let deadline = actor.session.as_ref().unwrap().deadline;
        actor.incoming(incoming(id, device, Body::Invite));
        assert_eq!(actor.session.as_ref().unwrap().deadline, deadline);
        actor.action(Action::Accept);
        assert_eq!(output.try_recv().unwrap().1.body, Body::Accept);
        assert!(actor.session.as_ref().unwrap().audio.is_none());
    });
}

#[test]
fn hangup_before_invite_and_replayed_invites_cannot_ring_again() {
    harness(|actor, _| {
        let id = Uuid::new_v4();
        let device = Uuid::new_v4();
        actor.incoming(incoming(
            id,
            device,
            Body::End {
                reason: EndReason::Hangup,
            },
        ));
        actor.incoming(incoming(id, device, Body::Invite));
        assert!(actor.session.is_none());
        let id = Uuid::new_v4();
        actor.incoming(incoming(id, device, Body::Invite));
        actor.action(Action::End);
        actor.incoming(incoming(id, device, Body::Invite));
        assert!(actor.session.is_none());
    });
}

#[test]
fn strangers_and_wrong_devices_cannot_take_over_or_end_a_call() {
    harness(|actor, _| {
        let id = Uuid::new_v4();
        let device = Uuid::new_v4();
        let mut stranger = incoming(id, device, Body::Invite);
        stranger.author = "33".repeat(32);
        actor.incoming(stranger);
        assert!(actor.session.is_none());
        actor.incoming(incoming(id, device, Body::Invite));
        actor.incoming(incoming(
            id,
            Uuid::new_v4(),
            Body::End {
                reason: EndReason::Hangup,
            },
        ));
        assert!(actor.session.is_some());
        actor.incoming(incoming(
            id,
            device,
            Body::End {
                reason: EndReason::Hangup,
            },
        ));
        assert!(actor.session.is_none());
    });
}

#[test]
fn voice_channel_is_busy_and_expiration_clears_state() {
    harness(|actor, output| {
        actor.state.write().voice.phase = VoicePhase::Connecting;
        actor.action(Action::Start("22".repeat(32)));
        assert!(actor.session.is_none());
        actor.incoming(incoming(Uuid::new_v4(), Uuid::new_v4(), Body::Invite));
        assert_eq!(
            output.try_recv().unwrap().1.body,
            Body::End {
                reason: EndReason::Busy
            }
        );
        actor.state.write().voice.phase = VoicePhase::Idle;
        actor.action(Action::Start("22".repeat(32)));
        actor.session.as_mut().unwrap().deadline = Some(Instant::now());
        actor.tick();
        assert!(actor.session.is_none());
        assert!(actor.state.peek().dm_call.is_none());
    });
}

#[test]
fn answering_on_another_device_dismisses_the_other_incoming_panel() {
    harness(|actor, _| {
        let id = Uuid::new_v4();
        let device = Uuid::new_v4();
        actor.incoming(incoming(id, device, Body::Invite));
        let mut offer = incoming(
            id,
            device,
            Body::Offer {
                sdp: "v=0\r\nm=audio 9 RTP/SAVPF 111\r\na=fingerprint:sha-256 00\r\n".into(),
            },
        );
        offer.signal.target = Some(Uuid::new_v4());
        actor.incoming(offer);
        assert!(actor.session.is_none());
    });
}

struct TestPeer(PeerConnection);
impl Drop for TestPeer {
    fn drop(&mut self) {
        self.0.close();
    }
}

#[test]
fn simultaneous_calls_have_a_deterministic_winner() {
    harness(|actor, output| {
        actor.our_key = "33".repeat(32);
        actor.action(Action::Start("22".repeat(32)));
        let old_id = actor.session.as_ref().unwrap().id;
        let winning_id = Uuid::new_v4();
        actor.incoming(incoming(winning_id, Uuid::new_v4(), Body::Invite));
        assert_eq!(actor.session.as_ref().unwrap().id, winning_id);
        assert_eq!(actor.session.as_ref().unwrap().phase, Phase::Incoming);
        let messages: Vec<_> = std::iter::from_fn(|| output.try_recv().ok()).collect();
        assert!(
            messages.iter().any(|(_, message)| message.call_id == old_id
                && matches!(message.body, Body::End { .. }))
        );
    });
    harness(|actor, output| {
        actor.action(Action::Start("22".repeat(32)));
        let winning_id = actor.session.as_ref().unwrap().id;
        actor.incoming(incoming(Uuid::new_v4(), Uuid::new_v4(), Body::Invite));
        assert_eq!(actor.session.as_ref().unwrap().id, winning_id);
        assert_eq!(actor.session.as_ref().unwrap().phase, Phase::Ringing);
        assert_eq!(output.try_recv().unwrap().1.body, Body::Invite);
        assert_eq!(
            output.try_recv().unwrap().1.body,
            Body::End {
                reason: EndReason::Busy
            }
        );
    });
}

#[test]
fn turn_configuration_is_bounded_and_relay_only_requires_turn() {
    assert!(parse_ice_configuration("[]", true).is_err());
    assert!(parse_ice_configuration(r#"[{"urls":["https://example.org"]}]"#, false).is_err());
    assert!(parse_ice_configuration(r#"[{"urls":[]}]"#, false).is_err());
    let config = parse_ice_configuration(r#"[{"urls":["turns:turn.example.org:5349?transport=tcp"],"username":"alice","password":"secret"}]"#, true).unwrap();
    assert_eq!(config.ice_transport_type, IceTransportsType::Relay);
    assert_eq!(config.ice_servers[0].username, "alice");
    assert_eq!(config.ice_servers[0].password, "secret");
}

#[test]
fn repeated_terminal_signals_do_not_grow_memory_without_bound() {
    harness(|actor, _| {
        for _ in 0..300 {
            actor.incoming(incoming(
                Uuid::new_v4(),
                Uuid::new_v4(),
                Body::End {
                    reason: EndReason::Hangup,
                },
            ));
        }
        assert_eq!(actor.finished.len(), 256);
    });
}

#[tokio::test(flavor = "multi_thread")]
async fn native_peer_connections_deliver_encoded_audio_without_a_voice_server() {
    let factory = PeerConnectionFactory::default();
    let config = RtcConfiguration {
        continual_gathering_policy: ContinualGatheringPolicy::GatherOnce,
        ..Default::default()
    };
    let alice = TestPeer(factory.create_peer_connection(config.clone()).unwrap());
    let bob = TestPeer(factory.create_peer_connection(config).unwrap());
    let source = NativeAudioSource::new(AudioSourceOptions::default(), 48_000, 1, 100);
    let alice_track = factory.create_audio_track("alice", source.clone());
    alice.0.add_track(alice_track.into(), &["voice"]).unwrap();
    let bob_source = NativeAudioSource::new(AudioSourceOptions::default(), 48_000, 1, 100);
    bob.0
        .add_track(
            factory.create_audio_track("bob", bob_source.clone()).into(),
            &["voice"],
        )
        .unwrap();
    let (tx, mut rx) = unbounded_channel();
    bob.0.on_track(Some(Box::new(move |event| {
        if let MediaStreamTrack::Audio(track) = event.track {
            tx.send(track).unwrap();
        }
    })));
    let (alice_tx, mut alice_rx) = unbounded_channel();
    alice.0.on_track(Some(Box::new(move |event| {
        if let MediaStreamTrack::Audio(track) = event.track {
            alice_tx.send(track).unwrap();
        }
    })));
    tokio::time::timeout(Duration::from_secs(15), async {
        let (_, offer) = local_description(&alice.0, None).await.unwrap();
        let (_, answer) = local_description(&bob.0, Some(offer)).await.unwrap();
        apply_description(&alice.0, &answer, SdpType::Answer)
            .await
            .unwrap();
        let remote = rx.recv().await.unwrap();
        let mut stream = NativeAudioStream::new(remote, 48_000, 1);
        let mut alice_stream = NativeAudioStream::new(alice_rx.recv().await.unwrap(), 48_000, 1);
        let mut samples = [0i16; 480];
        let mut phase = 0f32;
        let mut audible = false;
        let mut alice_audible = false;
        for _ in 0..300 {
            for sample in &mut samples {
                *sample = (phase.sin() * 6_000.0) as i16;
                phase += std::f32::consts::TAU * 440.0 / 48_000.0;
            }
            source
                .capture_frame(&AudioFrame {
                    data: std::borrow::Cow::Borrowed(&samples),
                    sample_rate: 48_000,
                    num_channels: 1,
                    samples_per_channel: 480,
                })
                .await
                .unwrap();
            bob_source
                .capture_frame(&AudioFrame {
                    data: std::borrow::Cow::Borrowed(&samples),
                    sample_rate: 48_000,
                    num_channels: 1,
                    samples_per_channel: 480,
                })
                .await
                .unwrap();
            if let Ok(Some(frame)) =
                tokio::time::timeout(Duration::from_millis(10), stream.next()).await
                && frame.data.iter().any(|value| value.saturating_abs() > 500)
            {
                audible = true;
            }
            if let Ok(Some(frame)) =
                tokio::time::timeout(Duration::from_millis(10), alice_stream.next()).await
                && frame.data.iter().any(|value| value.saturating_abs() > 500)
            {
                alice_audible = true;
            }
            if audible && alice_audible {
                break;
            }
        }
        assert!(
            audible,
            "The remote peer must decode actual audible Opus frames"
        );
        assert!(alice_audible, "The caller must also decode audible frames");
        assert_eq!(alice.0.connection_state(), PeerConnectionState::Connected);
        assert_eq!(bob.0.connection_state(), PeerConnectionState::Connected);
        bob.0.on_track(None);
        alice.0.on_track(None);
    })
    .await
    .unwrap();
}
