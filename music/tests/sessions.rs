#![cfg(feature = "activity")]

use discordia_bot::protocol::{
    ActivityInstance, ActivityParticipant, ActivityRole, Id, MAX_ACTIVITY_DATA,
};
use discordia_music::{
    activity::{
        runtime::sync,
        session::{Command, Effect, MAX_QUEUE, Session},
    },
    probe::{Report, Status},
    source::Source,
};
use std::collections::HashMap;

fn instance(channel: Id) -> ActivityInstance {
    ActivityInstance {
        id: Id::new_v4(),
        channel_id: channel,
        app: "music-app".into(),
        max_players: 16,
        participants: vec![
            ActivityParticipant {
                pubkey: "host".into(),
                role: ActivityRole::Player,
            },
            ActivityParticipant {
                pubkey: "guest".into(),
                role: ActivityRole::Player,
            },
            ActivityParticipant {
                pubkey: "spectator".into(),
                role: ActivityRole::Spectator,
            },
        ],
    }
}

fn add(session: &mut Session, from: &str) -> (Id, Source) {
    match session
        .apply(
            from,
            Command::Add {
                request_id: Id::new_v4(),
                url: "https://youtu.be/dQw4w9WgXcQ".into(),
            },
            true,
        )
        .unwrap()
    {
        Effect::Probe { entry_id, source } => (entry_id, source),
        _ => panic!("expected a metadata lookup"),
    }
}

fn report(source: Source) -> Report {
    Report {
        source,
        status: Status::MetadataAvailable,
        title: Some("A track".into()),
        http_status: Some(200),
        elapsed_ms: 1,
        audio_verified: false,
        playlist_entries_verified: false,
    }
}

#[test]
fn duplicate_commands_do_not_add_or_query_twice() {
    let mut session = Session::new(instance(Id::new_v4()));
    let request_id = Id::new_v4();
    let command = || Command::Add {
        request_id,
        url: "https://youtu.be/dQw4w9WgXcQ".into(),
    };
    assert!(matches!(
        session.apply("guest", command(), true).unwrap(),
        Effect::Probe { .. }
    ));
    assert!(matches!(
        session.apply("guest", command(), false).unwrap(),
        Effect::Snapshot
    ));
    let snapshot = serde_json::to_value(session.snapshot(Some(request_id))).unwrap();
    assert_eq!(snapshot["queue"].as_array().unwrap().len(), 1);
    assert_eq!(snapshot["ack"], request_id.to_string());
    assert_eq!(snapshot["playback"], "not_connected");
}

#[test]
fn outsiders_and_spectators_cannot_request_metadata_or_mutate() {
    let mut session = Session::new(instance(Id::new_v4()));
    for from in ["outsider", "spectator"] {
        assert!(
            session
                .apply(
                    from,
                    Command::Hello {
                        request_id: Id::new_v4()
                    },
                    true
                )
                .is_err()
        );
        assert!(
            session
                .apply(
                    from,
                    Command::Add {
                        request_id: Id::new_v4(),
                        url: "https://youtu.be/dQw4w9WgXcQ".into()
                    },
                    true
                )
                .is_err()
        );
    }
    assert_eq!(
        serde_json::to_value(session.snapshot(None)).unwrap()["revision"],
        0
    );
}

#[test]
fn owner_controls_are_rechecked_after_handoff() {
    let mut session = Session::new(instance(Id::new_v4()));
    let (entry_id, _) = add(&mut session, "host");
    assert!(
        session
            .apply(
                "guest",
                Command::Remove {
                    request_id: Id::new_v4(),
                    entry_id
                },
                true
            )
            .is_err()
    );
    assert!(
        session
            .apply(
                "guest",
                Command::Clear {
                    request_id: Id::new_v4()
                },
                true
            )
            .is_err()
    );
    let mut next = session.instance.clone();
    next.participants.remove(0);
    session.update(next);
    assert!(
        session
            .apply(
                "host",
                Command::Clear {
                    request_id: Id::new_v4()
                },
                true
            )
            .is_err()
    );
    assert!(
        session
            .apply(
                "guest",
                Command::Clear {
                    request_id: Id::new_v4()
                },
                true
            )
            .is_ok()
    );
}

#[test]
fn a_guest_can_remove_their_own_song_and_late_result_is_ignored() {
    let mut session = Session::new(instance(Id::new_v4()));
    let (entry_id, source) = add(&mut session, "guest");
    session
        .apply(
            "guest",
            Command::Remove {
                request_id: Id::new_v4(),
                entry_id,
            },
            true,
        )
        .unwrap();
    assert!(!session.pending(entry_id));
    assert!(!session.complete(entry_id, report(source)));
}

#[test]
fn clear_and_departure_cancel_pending_entries() {
    let mut session = Session::new(instance(Id::new_v4()));
    let (first, first_source) = add(&mut session, "guest");
    let (second, second_source) = add(&mut session, "guest");
    assert!(session.complete(first, report(first_source)));
    let mut next = session.instance.clone();
    next.participants.retain(|p| p.pubkey != "guest");
    session.update(next);
    assert!(!session.complete(second, report(second_source)));
    let state = serde_json::to_value(session.snapshot(None)).unwrap();
    assert_eq!(state["queue"].as_array().unwrap().len(), 1);
    session
        .apply(
            "host",
            Command::Clear {
                request_id: Id::new_v4(),
            },
            true,
        )
        .unwrap();
    assert_eq!(
        serde_json::to_value(session.snapshot(None)).unwrap()["queue"],
        serde_json::json!([])
    );
}

#[test]
fn failed_move_does_not_remove_or_reorder_anything() {
    let mut session = Session::new(instance(Id::new_v4()));
    let (first, _) = add(&mut session, "host");
    let (second, _) = add(&mut session, "host");
    let before = serde_json::to_value(session.snapshot(None)).unwrap();
    assert!(
        session
            .apply(
                "host",
                Command::Move {
                    request_id: Id::new_v4(),
                    entry_id: first,
                    before: Some(Id::new_v4())
                },
                true
            )
            .is_err()
    );
    assert_eq!(
        before,
        serde_json::to_value(session.snapshot(None)).unwrap()
    );
    session
        .apply(
            "host",
            Command::Move {
                request_id: Id::new_v4(),
                entry_id: second,
                before: Some(first),
            },
            true,
        )
        .unwrap();
    assert_eq!(
        serde_json::to_value(session.snapshot(None)).unwrap()["queue"][0]["id"],
        second.to_string()
    );
}

#[test]
fn capacity_and_worker_limits_reject_before_reserving_slots() {
    let mut session = Session::new(instance(Id::new_v4()));
    let command = || Command::Add {
        request_id: Id::new_v4(),
        url: "https://youtu.be/dQw4w9WgXcQ".into(),
    };
    assert!(session.apply("host", command(), false).is_err());
    add(&mut session, "host");
    add(&mut session, "host");
    assert!(session.apply("host", command(), true).is_err());
    assert_eq!(
        serde_json::to_value(session.snapshot(None)).unwrap()["queue"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn maximum_snapshot_fits_the_activity_frame() {
    let mut session = Session::new(instance(Id::new_v4()));
    let url = format!(
        "https://soundcloud.com/{}/{}",
        "a".repeat(255),
        "b".repeat(255)
    );
    for _ in 0..MAX_QUEUE {
        let Effect::Probe { entry_id, source } = session
            .apply(
                "host",
                Command::Add {
                    request_id: Id::new_v4(),
                    url: url.clone(),
                },
                true,
            )
            .unwrap()
        else {
            panic!("probe expected")
        };
        let mut result = report(source);
        result.title = Some("🎵".repeat(512));
        assert!(session.complete(entry_id, result));
    }
    assert!(
        serde_json::to_vec(&session.snapshot(Some(Id::new_v4())))
            .unwrap()
            .len()
            < MAX_ACTIVITY_DATA
    );
    assert!(
        session
            .apply(
                "host",
                Command::Add {
                    request_id: Id::new_v4(),
                    url
                },
                true
            )
            .is_err()
    );
}

#[test]
fn channel_sync_is_scoped_to_the_app_and_ends_missing_sessions() {
    let channel = Id::new_v4();
    let first = instance(channel);
    let mut second = instance(channel);
    second.app = "other-app".into();
    let mut sessions = HashMap::new();
    assert_eq!(
        sync(
            &mut sessions,
            channel,
            vec![first.clone(), second],
            "music-app"
        ),
        vec![first.id]
    );
    let (entry_id, source) = add(sessions.get_mut(&first.id).unwrap(), "host");
    sync(&mut sessions, Id::new_v4(), vec![], "music-app");
    assert!(
        sessions
            .get_mut(&first.id)
            .unwrap()
            .complete(entry_id, report(source))
    );
    sync(&mut sessions, channel, vec![], "music-app");
    assert!(sessions.is_empty());
}

#[test]
fn only_one_session_per_channel_is_admitted() {
    let channel = Id::new_v4();
    let mut sessions = HashMap::new();
    let changed = sync(
        &mut sessions,
        channel,
        vec![instance(channel), instance(channel)],
        "music-app",
    );
    assert_eq!(changed.len(), 1);
    assert_eq!(sessions.len(), 1);
}

#[test]
fn a_spectator_does_not_block_the_next_player_from_hosting() {
    let mut current = instance(Id::new_v4());
    current.participants.remove(0);
    current.participants.swap(0, 1);
    let mut session = Session::new(current);
    assert_eq!(
        serde_json::to_value(session.snapshot(None)).unwrap()["host"],
        "guest"
    );
    assert!(
        session
            .apply(
                "guest",
                Command::Clear {
                    request_id: Id::new_v4()
                },
                true
            )
            .is_ok()
    );
    assert!(
        session
            .apply(
                "spectator",
                Command::Clear {
                    request_id: Id::new_v4()
                },
                true
            )
            .is_err()
    );
}

#[test]
fn collection_imports_are_not_pretended_to_work() {
    let mut session = Session::new(instance(Id::new_v4()));
    assert!(
        session
            .apply(
                "host",
                Command::Add {
                    request_id: Id::new_v4(),
                    url: "https://www.youtube.com/playlist?list=PL1234567890".into()
                },
                true
            )
            .unwrap_err()
            .contains("imports are not connected")
    );
}
