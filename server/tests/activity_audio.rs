#![cfg(feature = "activity-audio")]

use discordia_bot::{Bot, BotIdentity};
use discordia_server::{
    ServerConfig,
    livekit::LiveKitConfig,
    protocol::{
        ActivityAudioRole, ActivityCapability, ActivityInstance, ActivityManifest, ActivityRole,
        AppAccess, AppManifest, ChannelKind, ClientMessage, Id, ServerMessage,
        activity_audio_epoch, activity_audio_room,
    },
};
use sha2::{Digest, Sha256};
use std::time::Duration;

async fn next(bot: &mut Bot) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(5), bot.next_event())
        .await
        .unwrap()
        .unwrap()
}

async fn user(url: &str, name: &str) -> (Bot, BotIdentity) {
    let identity = BotIdentity::generate();
    let mut bot = Bot::connect_as_user(url, &identity, name).await.unwrap();
    while !matches!(next(&mut bot).await, ServerMessage::Ready { .. }) {}
    (bot, identity)
}

struct World {
    url: String,
    handle: discordia_server::ServerHandle,
    owner: Bot,
    owner_id: BotIdentity,
    guest: Bot,
    outsider: Bot,
    service: Bot,
    app: BotIdentity,
    guild: Id,
    instance: ActivityInstance,
}

async fn setup(audio: bool) -> World {
    let dir = std::env::temp_dir().join(format!("discordia-audio-grants-{}", Id::new_v4()));
    let config = ServerConfig {
        livekit: LiveKitConfig::new(
            &dir,
            Some("ws://127.0.0.1:17880".into()),
            Some((
                "audio-test".into(),
                "isolated-audio-test-signing-secret-32-chars".into(),
            )),
            vec![],
        ),
        operators: Default::default(),
        data_dir: dir,
        identities: Default::default(),
        launch: Default::default(),
        admission: Default::default(),
    };
    let handle = discordia_server::spawn("127.0.0.1:0".parse().unwrap(), 1, config)
        .await
        .unwrap();
    let url = format!("ws://{}", handle.addr);
    let (mut owner, owner_id) = user(&url, "Audio owner").await;
    owner
        .send(&ClientMessage::CreateGuild {
            name: "Audio grants".into(),
            template: None,
        })
        .await
        .unwrap();
    let (guild, channel) = loop {
        if let ServerMessage::GuildJoined {
            guild, channels, ..
        } = next(&mut owner).await
        {
            break (
                guild.id,
                channels
                    .iter()
                    .find(|c| c.kind == ChannelKind::Text)
                    .unwrap()
                    .id,
            );
        }
    };
    let (mut guest, _) = user(&url, "Audio guest").await;
    guest
        .send(&ClientMessage::JoinGuild {
            guild_id: guild,
            accept: true,
            pow_nonce: None,
        })
        .await
        .unwrap();
    while !matches!(next(&mut guest).await, ServerMessage::GuildJoined { .. }) {}
    let (outsider, _) = user(&url, "Audio outsider").await;
    let app = BotIdentity::generate();
    let mut service = Bot::connect(&url, &app, "Audio service").await.unwrap();
    while !matches!(next(&mut service).await, ServerMessage::Ready { .. }) {}
    let page = b"<!doctype html><p>Music grants test</p>";
    let mut capabilities = vec![ActivityCapability::Instance];
    if audio {
        capabilities.push(ActivityCapability::Audio);
    }
    let manifest = AppManifest {
        name: "Audio test".into(),
        description: String::new(),
        icon: "music".into(),
        version: 1,
        permissions: vec![],
        intents: vec![],
        commands: vec![],
        agent: None,
        activity: Some(ActivityManifest {
            sha256: hex::encode(Sha256::digest(page)),
            capabilities,
            max_players: 16,
            url: None,
        }),
    };
    owner
        .send(&ClientMessage::InstallBot {
            guild_id: guild,
            bot_pubkey: app.pubkey().into(),
            name: String::new(),
            permissions: vec![],
            intents: vec![],
            manifest: Some(app.sign_manifest(&manifest)),
        })
        .await
        .unwrap();
    while !matches!(next(&mut service).await, ServerMessage::BundleWanted { .. }) {}
    service.offer_bundle(page).await.unwrap();
    loop {
        if let ServerMessage::GuildActivities { activities, .. } = next(&mut owner).await
            && !activities.is_empty()
        {
            break;
        }
    }
    owner
        .send(&ClientMessage::ActivityStart {
            channel_id: channel,
            app: app.pubkey().into(),
        })
        .await
        .unwrap();
    let instance = loop {
        if let ServerMessage::ActivityStarted { instance } = next(&mut owner).await {
            break instance;
        }
    };
    World {
        url,
        handle,
        owner,
        owner_id,
        guest,
        outsider,
        service,
        app,
        guild,
        instance,
    }
}

async fn request(
    bot: &mut Bot,
    instance: &ActivityInstance,
    role: ActivityAudioRole,
) -> Result<(String, String), String> {
    let request = bot.request_activity_audio(instance, role).await.unwrap();
    loop {
        match next(bot).await {
            ServerMessage::ActivityAudioToken {
                request_id,
                instance_id,
                epoch,
                role: granted_role,
                token,
                expires_in_secs,
                ..
            } if request_id == request => {
                assert_eq!(instance_id, instance.id);
                assert_eq!(epoch, activity_audio_epoch(instance));
                assert_eq!(granted_role, role);
                assert_eq!(expires_in_secs, 60);
                return Ok((token.into_inner(), epoch));
            }
            ServerMessage::ActivityAudioError {
                request_id,
                message,
                ..
            } if request_id == request => return Err(message),
            ServerMessage::Error { message } => return Err(message),
            _ => {}
        }
    }
}

#[cfg(feature = "activity-audio")]
fn claims(token: &str) -> serde_json::Value {
    use base64::Engine;
    serde_json::from_slice(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(token.split('.').nth(1).unwrap())
            .unwrap(),
    )
    .unwrap()
}

#[cfg(feature = "activity-audio")]
async fn updated(bot: &mut Bot, id: Id, participants: usize) -> ActivityInstance {
    loop {
        if let ServerMessage::ActivityInstances { instances, .. } = next(bot).await
            && let Some(instance) = instances
                .into_iter()
                .find(|i| i.id == id && i.participants.len() == participants)
        {
            return instance;
        }
    }
}

#[cfg(feature = "activity-audio")]
#[tokio::test]
async fn grants_are_role_scoped_connection_local_and_change_with_membership() {
    let mut w = setup(true).await;
    let (token, epoch) = request(&mut w.service, &w.instance, ActivityAudioRole::Publisher)
        .await
        .unwrap();
    let publisher = claims(&token);
    let lifetime = publisher["exp"].as_u64().unwrap() - publisher["nbf"].as_u64().unwrap();
    assert!((59..=61).contains(&lifetime));
    assert_eq!(
        publisher["video"]["room"],
        activity_audio_room(w.instance.id, &epoch).unwrap()
    );
    assert_eq!(publisher["video"]["canPublish"], true);
    assert_eq!(publisher["video"]["canSubscribe"], false);
    assert_eq!(publisher["video"]["canPublishData"], false);
    assert_eq!(
        publisher["video"]["canPublishSources"],
        serde_json::json!(["microphone"])
    );
    assert!(
        request(&mut w.owner, &w.instance, ActivityAudioRole::Publisher)
            .await
            .is_err()
    );
    assert!(
        request(&mut w.outsider, &w.instance, ActivityAudioRole::Listener)
            .await
            .is_err()
    );
    assert!(
        request(&mut w.service, &w.instance, ActivityAudioRole::Listener)
            .await
            .is_err()
    );
    let listener = claims(
        &request(&mut w.owner, &w.instance, ActivityAudioRole::Listener)
            .await
            .unwrap()
            .0,
    );
    assert_eq!(listener["video"]["canPublish"], false);
    assert_eq!(listener["video"]["canSubscribe"], true);
    assert_eq!(listener["video"]["canPublishData"], false);
    let mut sibling = Bot::connect_as_user(&w.url, &w.owner_id, "Second audio device")
        .await
        .unwrap();
    while !matches!(next(&mut sibling).await, ServerMessage::Ready { .. }) {}
    let sibling_claims = claims(
        &request(&mut sibling, &w.instance, ActivityAudioRole::Listener)
            .await
            .unwrap()
            .0,
    );
    assert_ne!(listener["sub"], sibling_claims["sub"]);
    drop(sibling);
    w.guest
        .send(&ClientMessage::ActivityJoin {
            instance_id: w.instance.id,
            role: ActivityRole::Spectator,
        })
        .await
        .unwrap();
    let current = updated(&mut w.service, w.instance.id, 2).await;
    assert!(
        request(&mut w.service, &w.instance, ActivityAudioRole::Publisher)
            .await
            .unwrap_err()
            .contains("participants changed")
    );
    let fresh = claims(
        &request(&mut w.service, &current, ActivityAudioRole::Publisher)
            .await
            .unwrap()
            .0,
    );
    assert_ne!(fresh["video"]["room"], publisher["video"]["room"]);
    assert!(
        request(&mut w.guest, &current, ActivityAudioRole::Listener)
            .await
            .is_ok()
    );
    w.guest
        .send(&ClientMessage::ActivityLeave {
            instance_id: current.id,
        })
        .await
        .unwrap();
    updated(&mut w.service, current.id, 1).await;
    assert!(
        request(&mut w.guest, &current, ActivityAudioRole::Listener)
            .await
            .is_err()
    );
    w.handle.abort();
}

#[cfg(feature = "activity-audio")]
#[tokio::test]
async fn audio_requires_explicit_manifest_approval() {
    let mut w = setup(false).await;
    assert!(
        request(&mut w.service, &w.instance, ActivityAudioRole::Publisher)
            .await
            .unwrap_err()
            .contains("not been approved")
    );
    assert!(
        request(&mut w.owner, &w.instance, ActivityAudioRole::Listener)
            .await
            .is_err()
    );
    w.handle.abort();
}

#[cfg(feature = "activity-audio")]
#[tokio::test]
async fn changing_app_access_removes_ineligible_participants_and_blocks_rejoining() {
    let mut w = setup(true).await;
    w.guest
        .send(&ClientMessage::ActivityJoin {
            instance_id: w.instance.id,
            role: ActivityRole::Player,
        })
        .await
        .unwrap();
    let before = updated(&mut w.service, w.instance.id, 2).await;
    w.owner
        .send(&ClientMessage::SetAppAccess {
            guild_id: w.guild,
            bot_pubkey: w.app.pubkey().into(),
            access: AppAccess {
                channels: None,
                roles: Some(vec![]),
            },
        })
        .await
        .unwrap();
    let after = updated(&mut w.service, w.instance.id, 1).await;
    assert_ne!(activity_audio_epoch(&before), activity_audio_epoch(&after));
    assert!(
        request(&mut w.guest, &after, ActivityAudioRole::Listener)
            .await
            .is_err()
    );
    w.guest
        .send(&ClientMessage::ActivityJoin {
            instance_id: after.id,
            role: ActivityRole::Player,
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::Error { message } = next(&mut w.guest).await {
            assert!(message.contains("not available"));
            break;
        }
    }
    w.owner
        .send(&ClientMessage::SetAppAccess {
            guild_id: w.guild,
            bot_pubkey: w.app.pubkey().into(),
            access: AppAccess {
                channels: Some(vec![]),
                roles: None,
            },
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::ActivityInstances { instances, .. } = next(&mut w.service).await
            && instances.is_empty()
        {
            break;
        }
    }
    assert!(
        request(&mut w.service, &after, ActivityAudioRole::Publisher)
            .await
            .is_err()
    );
    w.handle.abort();
}

#[tokio::test]
async fn spectators_request_audio_keys_without_receiving_game_write_access() {
    let mut w = setup(true).await;
    w.guest
        .send(&ClientMessage::ActivityJoin {
            instance_id: w.instance.id,
            role: ActivityRole::Spectator,
        })
        .await
        .unwrap();
    let current = updated(&mut w.service, w.instance.id, 2).await;
    w.guest
        .send(&ClientMessage::ActivitySend {
            instance_id: current.id,
            to: Some(w.app.pubkey().into()),
            data: r#"{"t":"pretend_game_move"}"#.into(),
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::Error { message } = next(&mut w.guest).await {
            assert!(message.contains("only players"));
            break;
        }
    }
    let request_id = Id::new_v4();
    let epoch = activity_audio_epoch(&current);
    w.guest
        .send(&ClientMessage::RequestActivityAudio {
            instance_id: current.id,
            request_id,
            epoch: epoch.clone(),
            role: ActivityAudioRole::Listener,
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::ActivityAudioToken {
            request_id: received,
            ..
        } = next(&mut w.guest).await
        {
            assert_eq!(received, request_id);
            break;
        }
    }
    loop {
        if let ServerMessage::ActivityEvent {
            instance_id,
            from,
            data,
        } = next(&mut w.service).await
        {
            assert_eq!(instance_id, current.id);
            assert!(current.role_of(&from) == Some(ActivityRole::Spectator));
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&data).unwrap(),
                serde_json::json!({"__dxaudio":"key_request","request_id":request_id,"epoch":epoch})
            );
            break;
        }
    }
    w.handle.abort();
}
