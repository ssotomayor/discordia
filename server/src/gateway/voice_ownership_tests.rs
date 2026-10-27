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
    ctx.state.resume_voice_session(&user.pubkey, sibling);
    assert!(ctx.state.voice_owned_by(&user.pubkey, owner));
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
    let (ctx, user, _, channel, owner, _) = fixture().await;
    ctx.state.unregister_conn(owner, Some(&user.pubkey));
    tokio::time::pause();
    let cleanup = {
        let ctx = ctx.clone();
        let user = user.clone();
        tokio::spawn(async move { cleanup_session(&ctx, owner, user, false).await })
    };
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(1)).await;
    let (replacement, _) = ctx.state.register_conn(None).unwrap();
    ctx.state.identify_conn(replacement, &user.pubkey).unwrap();
    ctx.state.resume_voice_session(&user.pubkey, replacement);
    tokio::time::advance(Duration::from_secs(2)).await;
    cleanup.await.unwrap();
    assert!(ctx.state.voice_owned_by(&user.pubkey, replacement));
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

async fn movable_member(ctx: &AppContext, owner: &User, guild: Id, source: Id) -> (User, u64, Id) {
    let member = User {
        pubkey: "22".repeat(32),
        username: "Target".into(),
    };
    ctx.state.join_guild(guild, &member).await.unwrap();
    let (conn, _) = ctx
        .state
        .register_conn(Some("198.51.100.2".parse().unwrap()))
        .unwrap();
    ctx.state.identify_conn(conn, &member.pubkey).unwrap();
    ctx.state.set_voice_channel_owned(
        conn,
        &member.pubkey,
        guild,
        source,
        Some(crate::protocol::VoicePreferences {
            muted: true,
            deafened: true,
        }),
    );
    ctx.state.update_screen_share(&member.pubkey, true);
    ctx.state.update_camera(&member.pubkey, true);
    let destination = ctx
        .state
        .create_channel(
            guild,
            "Destination",
            crate::protocol::ChannelKind::Voice,
            None,
            Default::default(),
            &owner.pubkey,
        )
        .await
        .unwrap();
    (member, conn, destination.id)
}

#[tokio::test]
async fn owner_and_move_role_preserve_voice_ownership_and_mute_when_moving_a_member() {
    let (ctx, owner, guild, source, _, _) = fixture().await;
    let (target, target_conn, destination) = movable_member(&ctx, &owner, guild, source).await;
    let manager = User {
        pubkey: "33".repeat(32),
        username: "Manager".into(),
    };
    ctx.state.join_guild(guild, &manager).await.unwrap();
    assert!(
        ctx.state
            .move_voice_member(&target.pubkey, &manager.pubkey, source, destination)
            .unwrap_err()
            .contains("move_members")
    );
    let role = ctx
        .state
        .create_role(
            guild,
            "Mover",
            None,
            vec![Permission::MoveMembers],
            &owner.pubkey,
        )
        .await
        .unwrap();
    ctx.state
        .set_member_role(guild, role.id, &manager.pubkey, true, &owner.pubkey)
        .await
        .unwrap();
    let (previous, joined, conn) = ctx
        .state
        .move_voice_member(&target.pubkey, &manager.pubkey, source, destination)
        .unwrap();
    assert_eq!(conn, target_conn);
    assert_eq!(previous.channel_id, Some(source));
    assert!(previous.camera_on && previous.screen_sharing);
    assert_eq!(joined.channel_id, Some(destination));
    assert!(joined.muted && joined.deafened);
    assert!(!joined.speaking && !joined.camera_on && !joined.screen_sharing);
    assert!(joined.screen_watching.is_empty());
    assert!(ctx.state.voice_owned_by(&target.pubkey, target_conn));
    let (sibling, _) = ctx.state.register_conn(None).unwrap();
    ctx.state.identify_conn(sibling, &target.pubkey).unwrap();
    assert!(
        ctx.state
            .clear_voice_owned(&target.pubkey, sibling)
            .is_none()
    );
    assert_eq!(
        ctx.state.connection_voice_address(target_conn).1,
        Some("198.51.100.2".parse().unwrap())
    );
    assert!(
        ctx.state
            .move_voice_member(&owner.pubkey, &manager.pubkey, source, destination)
            .unwrap_err()
            .contains("owner")
    );
    ctx.state
        .move_voice_member(&target.pubkey, &owner.pubkey, destination, source)
        .unwrap();
    assert_eq!(ctx.state.voice_channel_of(&target.pubkey), Some(source));
}

#[tokio::test]
async fn invalid_moderator_moves_leave_the_original_call_untouched() {
    let (ctx, owner, guild, source, _, _) = fixture().await;
    let (target, target_conn, destination) = movable_member(&ctx, &owner, guild, source).await;
    let text = ctx
        .state
        .create_channel(
            guild,
            "Text",
            crate::protocol::ChannelKind::Text,
            None,
            Default::default(),
            &owner.pubkey,
        )
        .await
        .unwrap();
    let (other_guild, other_channels, _, _) = ctx
        .state
        .create_guild("Elsewhere", None, &owner)
        .await
        .unwrap();
    ctx.state.join_guild(other_guild.id, &target).await.unwrap();
    let foreign = other_channels
        .iter()
        .find(|c| c.kind == crate::protocol::ChannelKind::Voice)
        .unwrap()
        .id;
    for (from, to) in [
        (source, source),
        (source, text.id),
        (source, foreign),
        (Id::new_v4(), destination),
    ] {
        assert!(
            ctx.state
                .move_voice_member(&target.pubkey, &owner.pubkey, from, to)
                .is_err()
        );
        assert_eq!(ctx.state.voice_channel_of(&target.pubkey), Some(source));
        assert!(ctx.state.voice_owned_by(&target.pubkey, target_conn));
    }
    ctx.state
        .set_channel_access(
            destination,
            Some(crate::protocol::ChannelAccess::default()),
            &owner.pubkey,
        )
        .await
        .unwrap();
    assert!(
        ctx.state
            .move_voice_member(&target.pubkey, &owner.pubkey, source, destination)
            .is_err()
    );
    assert!(
        ctx.state
            .voice_states
            .get(&target.pubkey)
            .unwrap()
            .screen_sharing
    );
    ctx.state
        .set_channel_access(destination, None, &owner.pubkey)
        .await
        .unwrap();
    ctx.state
        .move_voice_member(&target.pubkey, &owner.pubkey, source, destination)
        .unwrap();
    assert!(
        ctx.state
            .move_voice_member(&target.pubkey, &owner.pubkey, source, destination)
            .is_err()
    );
    assert_eq!(
        ctx.state.voice_channel_of(&target.pubkey),
        Some(destination)
    );
    ctx.state.unregister_conn(target_conn, Some(&target.pubkey));
    assert!(
        ctx.state
            .move_voice_member(&target.pubkey, &owner.pubkey, destination, source)
            .is_err()
    );
    assert_eq!(
        ctx.state.voice_channel_of(&target.pubkey),
        Some(destination)
    );
}
