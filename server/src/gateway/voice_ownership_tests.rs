use super::*;

async fn fixture() -> (Arc<AppContext>, User, Id, Id, u64, u64) {
    let dir = std::env::temp_dir().join(format!("discordia-voice-owner-{}", Id::new_v4()));
    let store = crate::store::Store::open_in(&dir).await.unwrap();
    let media =
        crate::media::MediaStore::open(dir.join("media"), crate::media::DEFAULT_MAX_BYTES).unwrap();
    let state = crate::state::AppState::load_or_seed(store, media, Default::default())
        .await
        .unwrap();
    let user = User {
        pubkey: "11".repeat(32),
        username: "owner".into(),
    };
    let (guild, channels, _, _) = state.create_guild("voice", None, &user).await.unwrap();
    let channel = channels
        .iter()
        .find(|c| c.kind == crate::protocol::ChannelKind::Voice)
        .unwrap()
        .id;
    let (owner, _) = state.register_conn(None).unwrap();
    state.identify_conn(owner, &user.pubkey).unwrap();
    let (sibling, _) = state.register_conn(None).unwrap();
    state.identify_conn(sibling, &user.pubkey).unwrap();
    state.set_voice_channel_owned(owner, &user.pubkey, guild.id, channel, None);
    state.update_screen_share(&user.pubkey, true);
    let ctx = Arc::new(AppContext {
        state: Arc::new(state),
        livekit: crate::livekit::LiveKitConfig::from_env(&dir),
        identities: Default::default(),
        shutdown: Arc::new(tokio::sync::watch::Sender::new(false)),
    });
    (ctx, user, guild.id, channel, owner, sibling)
}

#[tokio::test]
async fn closing_a_chat_sibling_preserves_the_owners_call_and_share() {
    let (ctx, user, _, channel, owner, sibling) = fixture().await;
    ctx.state.unregister_conn(sibling, Some(&user.pubkey));
    cleanup_session(&ctx, sibling, user.clone(), false).await;
    assert!(ctx.state.voice_owned_by(&user.pubkey, owner));
    assert_eq!(ctx.state.voice_channel_of(&user.pubkey), Some(channel));
    assert!(
        ctx.state
            .voice_states
            .get(&user.pubkey)
            .unwrap()
            .screen_sharing
    );
}

#[tokio::test]
async fn owner_disconnect_expires_after_grace_even_with_a_chat_sibling() {
    let (ctx, user, _, channel, owner, _) = fixture().await;
    ctx.state.unregister_conn(owner, Some(&user.pubkey));
    tokio::time::pause();
    let cleanup = {
        let ctx = ctx.clone();
        let user = user.clone();
        tokio::spawn(async move { cleanup_session(&ctx, owner, user, false).await })
    };
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(2999)).await;
    assert_eq!(ctx.state.voice_channel_of(&user.pubkey), Some(channel));
    tokio::time::advance(Duration::from_millis(1)).await;
    cleanup.await.unwrap();
    assert!(ctx.state.voice_channel_of(&user.pubkey).is_none());
    assert!(ctx.state.voice_states.get(&user.pubkey).is_none());
    assert!(ctx.state.has_sessions(&user.pubkey));
    assert!(
        ctx.state
            .update_voice_flags(&user.pubkey, true, false)
            .is_none()
    );
}

#[tokio::test]
async fn reconnect_handoff_survives_late_cleanup_from_the_old_owner() {
    let (ctx, user, guild, channel, owner, sibling) = fixture().await;
    ctx.state.unregister_conn(owner, Some(&user.pubkey));
    tokio::time::pause();
    let cleanup = {
        let ctx = ctx.clone();
        let user = user.clone();
        tokio::spawn(async move { cleanup_session(&ctx, owner, user, false).await })
    };
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(1)).await;
    ctx.state
        .set_voice_channel_owned(sibling, &user.pubkey, guild, channel, None);
    ctx.state.update_screen_share(&user.pubkey, true);
    tokio::time::advance(Duration::from_secs(2)).await;
    cleanup.await.unwrap();
    assert!(ctx.state.voice_owned_by(&user.pubkey, sibling));
    assert!(
        ctx.state
            .voice_states
            .get(&user.pubkey)
            .unwrap()
            .screen_sharing
    );
    assert!(ctx.state.clear_voice_owned(&user.pubkey, owner).is_none());
    assert_eq!(ctx.state.voice_channel_of(&user.pubkey), Some(channel));
}

#[tokio::test]
async fn explicit_leave_is_owned_and_releases_the_publication() {
    let (ctx, user, _, channel, owner, sibling) = fixture().await;
    assert!(ctx.state.clear_voice_owned(&user.pubkey, sibling).is_none());
    assert!(
        ctx.state
            .update_owned_voice(&user.pubkey, sibling, |state| state.update_voice_flags(
                &user.pubkey,
                true,
                true
            ))
            .is_none()
    );
    assert!(!ctx.state.voice_states.get(&user.pubkey).unwrap().muted);
    assert_eq!(ctx.state.voice_channel_of(&user.pubkey), Some(channel));
    let (previous, cleared) = ctx.state.clear_voice_owned(&user.pubkey, owner).unwrap();
    assert!(previous.screen_sharing);
    assert_eq!(cleared.channel_id, None);
    assert!(!cleared.screen_sharing);
    assert!(!ctx.state.voice_owned_by(&user.pubkey, owner));
    assert!(ctx.state.clear_voice_owned(&user.pubkey, owner).is_none());
}
