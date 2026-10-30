use discordia_server::media::{DEFAULT_MAX_BYTES, MediaStore};
use discordia_server::protocol::{ChannelAccess, ChannelKind, Leveling, User, XpAction};
use discordia_server::state::AppState;
use discordia_server::store::Store;

fn person(n: u32, name: &str) -> User {
    User {
        pubkey: format!("{n:064x}"),
        username: name.into(),
    }
}

/// The minute sweep sends each award to the whole guild, so an award from a
/// hidden room would say who is in it; such a room earns nothing, by any action.
#[tokio::test]
async fn a_hidden_room_earns_no_experience_even_when_the_guild_lists_it() {
    let dir = std::env::temp_dir().join(format!("discordia-hidden-xp-{}", uuid::Uuid::new_v4()));
    let store = Store::open_in(&dir).await.unwrap();
    let media = MediaStore::open(dir.join("media"), DEFAULT_MAX_BYTES).unwrap();
    let state = AppState::load_or_seed(store, media, Default::default())
        .await
        .unwrap();
    let owner = person(1, "Owner");
    let insider = person(2, "Insider");
    let (guild, ..) = state.create_guild("Rooms", None, &owner).await.unwrap();
    let gid = guild.id;
    state.add_member(gid, &insider).await.unwrap();
    let by = owner.pubkey.as_str();
    let room = |name: &'static str| {
        let state = &state;
        async move {
            state
                .create_channel(gid, name, ChannelKind::Voice, None, Default::default(), by)
                .await
                .unwrap()
                .id
        }
    };
    let lounge = room("Lounge").await;
    let staff = room("Staff").await;
    state
        .set_channel_access(
            staff,
            Some(ChannelAccess {
                roles: vec![],
                users: vec![insider.pubkey.clone()],
            }),
            by,
        )
        .await
        .unwrap();
    let leveling = Leveling {
        per_message: 1,
        per_voice_minute: 5,
        cooldown_secs: 0,
        channels: vec![lounge, staff],
        ..Default::default()
    };
    state.set_guild_leveling(gid, leveling, by).await.unwrap();

    state.set_voice_channel(&insider.pubkey, gid, Some(staff), None);
    for sweep in 1..=2 {
        let awarded = state.award_voice_minute().await;
        assert!(
            awarded.is_empty(),
            "sweep {sweep} sent the guild an award from inside a hidden room"
        );
    }
    assert!(
        state
            .award_xp(gid, Some(staff), &insider.pubkey, XpAction::Message)
            .await
            .is_none(),
        "no action earns there"
    );
    assert_eq!(state.xp_of(gid, &insider.pubkey), 0);

    state.set_voice_channel(&insider.pubkey, gid, Some(lounge), None);
    let awarded = state.award_voice_minute().await;
    assert_eq!(awarded.len(), 1, "an open room still earns");
    assert_eq!(state.xp_of(gid, &insider.pubkey), 5);
}
