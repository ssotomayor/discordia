use super::*;
use crate::protocol::{ChannelKind, User};

struct Fixture {
    state: AppState,
    pubkey: String,
    guild: Id,
    first: Id,
    second: Id,
    a: u64,
    b: u64,
}

async fn fixture() -> Fixture {
    let dir = std::env::temp_dir().join(format!("discordia-voice-session-{}", Id::new_v4()));
    let store = crate::store::Store::open_in(&dir).await.unwrap();
    let media =
        crate::media::MediaStore::open(dir.join("media"), crate::media::DEFAULT_MAX_BYTES).unwrap();
    let state = AppState::load_or_seed(store, media, Default::default())
        .await
        .unwrap();
    let user = User {
        pubkey: "44".repeat(32),
        username: "caller".into(),
    };
    let (guild, channels, _, _) = state.create_guild("voice", None, &user).await.unwrap();
    let first = channels
        .iter()
        .find(|c| c.kind == ChannelKind::Voice)
        .unwrap()
        .id;
    let second = state
        .create_channel(
            guild.id,
            "Second",
            ChannelKind::Voice,
            None,
            Default::default(),
            &user.pubkey,
        )
        .await
        .unwrap()
        .id;
    let (a, _) = state.register_conn(None).unwrap();
    state.identify_conn(a, &user.pubkey).unwrap();
    let (b, _) = state.register_conn(None).unwrap();
    state.identify_conn(b, &user.pubkey).unwrap();
    Fixture {
        state,
        pubkey: user.pubkey,
        guild: guild.id,
        first,
        second,
        a,
        b,
    }
}

#[derive(Debug, Clone, Copy)]
enum Step {
    Join(u64, Room),
    Leave(u64),
    End(Room),
    Move(Room, Room),
    OwnerLost(u64),
    HandOff(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Room {
    First,
    Second,
}

/// Where the call stands after a step: channel, owner, owner lost, and whether
/// the step began a new join.
type After = Option<(Room, u64, bool)>;

impl Fixture {
    fn room(&self, room: Room) -> Id {
        match room {
            Room::First => self.first,
            Room::Second => self.second,
        }
    }

    fn apply(&self, step: Step) {
        let pk = self.pubkey.as_str();
        match step {
            Step::Join(conn, room) => {
                self.state
                    .join_voice(conn, pk, self.guild, self.room(room), None);
            }
            Step::Leave(conn) => {
                self.state.leave_voice(pk, conn);
            }
            Step::End(room) => {
                let id = self.room(room);
                self.state.end_voice(pk, |v| v.channel_id == Some(id));
            }
            Step::Move(from, to) => {
                let _ =
                    self.state
                        .move_voice_session(pk, self.guild, self.room(from), self.room(to));
            }
            Step::OwnerLost(conn) => self.state.voice_owner_lost(pk, conn),
            Step::HandOff(conn) => self.state.hand_off_voice(pk, conn),
        }
    }

    fn observed(&self) -> After {
        let session = self.state.voice_session(&self.pubkey);
        let shown = self.state.voice_channel_of(&self.pubkey);
        assert_eq!(
            session.map(|s| s.channel_id),
            shown,
            "the session and the voice state it mirrors came apart"
        );
        session.map(|s| {
            let room = if s.channel_id == self.first {
                Room::First
            } else {
                Room::Second
            };
            (room, s.owner, s.owner_lost)
        })
    }
}

#[tokio::test]
async fn every_transition_moves_membership_and_ownership_together() {
    use Room::*;
    use Step::*;
    let f = fixture().await;
    let (a, b) = (f.a, f.b);
    let table: Vec<(Step, After, bool)> = vec![
        (Join(a, First), Some((First, a, false)), true),
        (Join(a, First), Some((First, a, false)), true),
        (Leave(b), Some((First, a, false)), false),
        (HandOff(b), Some((First, a, false)), false),
        (OwnerLost(b), Some((First, a, false)), false),
        (Move(First, Second), Some((Second, a, false)), true),
        (Move(First, Second), Some((Second, a, false)), false),
        (End(First), Some((Second, a, false)), false),
        (OwnerLost(a), Some((Second, a, true)), false),
        (Move(Second, First), Some((Second, a, true)), false),
        (HandOff(b), Some((Second, b, false)), false),
        (Leave(a), Some((Second, b, false)), false),
        (End(Second), None, false),
        (Join(b, First), Some((First, b, false)), true),
        (Leave(b), None, false),
        (Leave(b), None, false),
    ];
    let mut join = None;
    for (i, (step, expected, fresh)) in table.into_iter().enumerate() {
        f.apply(step);
        assert_eq!(f.observed(), expected, "row {i}: {step:?}");
        let now = f.state.voice_session(&f.pubkey).map(|s| s.join);
        if let (Some(before), Some(now)) = (join, now) {
            assert_eq!(now > before, fresh, "row {i}: {step:?} began a new join");
        }
        join = now;
    }
}

#[tokio::test]
async fn a_grant_is_current_only_for_the_join_it_was_minted_for() {
    let f = fixture().await;
    let pk = f.pubkey.as_str();
    let first = f.state.join_voice(f.a, pk, f.guild, f.first, None).call;
    assert!(f.state.is_current_call(&first));
    f.state.leave_voice(pk, f.a);
    let again = f.state.join_voice(f.a, pk, f.guild, f.first, None).call;
    assert!(
        !f.state.is_current_call(&first),
        "a leave and a rejoin of the same room"
    );
    assert!(f.state.is_current_call(&again));
    let (_, moved) = f
        .state
        .move_voice_session(pk, f.guild, f.first, f.second)
        .unwrap();
    assert!(!f.state.is_current_call(&again), "moved out of that room");
    assert!(f.state.is_current_call(&moved.call));
    assert_eq!(f.state.live_calls(), vec![moved.call.clone()]);
    f.state.unregister_conn(f.a, Some(pk));
    assert!(!f.state.is_current_call(&moved.call), "its socket is gone");
    assert!(f.state.live_calls().is_empty());
    f.state.hand_off_voice(pk, f.b);
    assert!(
        !f.state.is_current_call(&moved.call),
        "minted for the socket that left"
    );
    let adopted = f.state.live_calls();
    assert_eq!(adopted.len(), 1);
    assert_eq!((adopted[0].owner, adopted[0].join), (f.b, moved.call.join));
}

#[tokio::test]
async fn an_ending_decided_about_one_call_spares_the_next() {
    let f = fixture().await;
    let pk = f.pubkey.as_str();
    f.state.join_voice(f.a, pk, f.guild, f.first, None);
    f.state.join_voice(f.a, pk, f.guild, f.second, None);
    assert!(
        f.state
            .end_voice(pk, |v| v.channel_id == Some(f.first))
            .is_none()
    );
    assert_eq!(f.state.voice_channel_of(pk), Some(f.second));
    let ended = f.state.end_voice(pk, |v| v.guild_id == f.guild).unwrap();
    assert_eq!(ended.previous.channel_id, Some(f.second));
    assert_eq!(ended.cleared.channel_id, None);
    assert!(f.state.voice_session(pk).is_none());
}
