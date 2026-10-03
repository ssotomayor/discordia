use std::net::SocketAddr;
use std::time::Duration;

use base64::Engine as _;
use dioxusfun_bot::{Bot, BotIdentity};
use dioxusfun_server::livekit::LiveKitConfig;
use dioxusfun_server::protocol::{ChannelKind, ClientMessage, GuildSound, Id, ServerMessage};

async fn next_timeout(session: &mut Bot) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(5), session.next_event())
        .await
        .expect("timed out waiting for a gateway event")
        .expect("connection closed unexpectedly")
}

fn test_config() -> dioxusfun_server::ServerConfig {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "dioxusfun-sound-test-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    dioxusfun_server::ServerConfig {
        livekit: LiveKitConfig::from_env(&dir),
        operators: Default::default(),
        identities: Default::default(),
        media_max_bytes: dioxusfun_server::media::DEFAULT_MAX_BYTES,
        data_dir: dir,
    }
}

async fn spawn_gateway() -> (String, dioxusfun_server::ServerHandle) {
    let preferred: SocketAddr = "127.0.0.1:19270".parse().unwrap();
    let handle = dioxusfun_server::spawn(preferred, 100, test_config())
        .await
        .expect("spawn server");
    (format!("ws://{}", handle.addr), handle)
}

async fn connect_user(url: &str, id: &BotIdentity, name: &str) -> Bot {
    let mut session = Bot::connect_as_user(url, id, name).await.unwrap();
    loop {
        if let ServerMessage::Ready { .. } = next_timeout(&mut session).await {
            return session;
        }
    }
}

async fn create_guild(owner: &mut Bot) -> Id {
    owner
        .send(&ClientMessage::CreateGuild {
            name: "Loud".into(),
            template: None,
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::GuildJoined { guild, .. } = next_timeout(owner).await {
            return guild.id;
        }
    }
}

async fn join_guild(member: &mut Bot, guild_id: Id) -> Vec<GuildSound> {
    member
        .send(&ClientMessage::JoinGuild {
            guild_id,
            accept: true,
            pow_nonce: None,
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::GuildJoined { sounds, .. } = next_timeout(member).await {
            return sounds;
        }
    }
}

async fn create_voice_channel(owner: &mut Bot, guild_id: Id) -> Id {
    owner
        .send(&ClientMessage::CreateChannel {
            guild_id,
            name: "Voice".into(),
            kind: ChannelKind::Voice,
            topic: None,
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::ChannelCreate(ch) = next_timeout(owner).await
            && ch.kind == ChannelKind::Voice
        {
            return ch.id;
        }
    }
}

async fn next_sounds(session: &mut Bot) -> Vec<GuildSound> {
    loop {
        if let ServerMessage::GuildSounds { sounds, .. } = next_timeout(session).await {
            return sounds;
        }
    }
}

async fn next_error(session: &mut Bot) -> String {
    loop {
        if let ServerMessage::Error { message } = next_timeout(session).await {
            return message;
        }
    }
}

/// A header and a few silent samples: the server checks the magic, not the audio.
fn tiny_wav() -> String {
    let mut wav = b"RIFF\x2c\0\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x80\xbb\0\0\0\x77\x01\0\x02\0\x10\0data\x08\0\0\0".to_vec();
    wav.extend_from_slice(&[0; 8]);
    let b64 = base64::engine::general_purpose::STANDARD.encode(wav);
    format!("data:audio/wav;base64,{b64}")
}

async fn add_sound(owner: &mut Bot, guild_id: Id, name: &str) -> Vec<GuildSound> {
    owner
        .send(&ClientMessage::CreateGuildSound {
            guild_id,
            name: name.into(),
            audio: tiny_wav(),
        })
        .await
        .unwrap();
    next_sounds(owner).await
}

#[tokio::test]
async fn the_owner_manages_the_library_and_members_see_it() {
    let (url, handle) = spawn_gateway().await;
    let owner_id = BotIdentity::generate();
    let mut owner = connect_user(&url, &owner_id, "Owner").await;
    let guild_id = create_guild(&mut owner).await;

    let sounds = add_sound(&mut owner, guild_id, "Air horn").await;
    assert_eq!(sounds.len(), 1);
    assert_eq!(sounds[0].name, "Air horn");
    assert_eq!(sounds[0].added_by, owner_id.pubkey());
    assert!(
        sounds[0].audio.starts_with("media:") && sounds[0].audio.ends_with(".wav"),
        "rows carry the sentinel, not bytes: {}",
        sounds[0].audio
    );

    let mut member = connect_user(&url, &BotIdentity::generate(), "Member").await;
    let seen = join_guild(&mut member, guild_id).await;
    assert_eq!(seen, sounds, "a joiner gets the library with the guild");

    let sound_id = sounds[0].id;
    owner
        .send(&ClientMessage::RenameGuildSound {
            guild_id,
            sound_id,
            name: "Horn".into(),
        })
        .await
        .unwrap();
    assert_eq!(next_sounds(&mut owner).await[0].name, "Horn");
    assert_eq!(next_sounds(&mut member).await[0].name, "Horn");

    owner
        .send(&ClientMessage::DeleteGuildSound { guild_id, sound_id })
        .await
        .unwrap();
    assert!(next_sounds(&mut owner).await.is_empty());
    assert!(next_sounds(&mut member).await.is_empty());

    handle.abort();
}

#[tokio::test]
async fn a_member_without_manage_guild_cannot_upload_or_remove() {
    let (url, handle) = spawn_gateway().await;
    let mut owner = connect_user(&url, &BotIdentity::generate(), "Owner").await;
    let guild_id = create_guild(&mut owner).await;
    let sounds = add_sound(&mut owner, guild_id, "boing").await;

    let mut member = connect_user(&url, &BotIdentity::generate(), "Member").await;
    join_guild(&mut member, guild_id).await;

    member
        .send(&ClientMessage::CreateGuildSound {
            guild_id,
            name: "sneaky".into(),
            audio: tiny_wav(),
        })
        .await
        .unwrap();
    let err = next_error(&mut member).await;
    assert!(err.contains("manage_guild"), "unexpected: {err}");

    member
        .send(&ClientMessage::DeleteGuildSound {
            guild_id,
            sound_id: sounds[0].id,
        })
        .await
        .unwrap();
    let err = next_error(&mut member).await;
    assert!(err.contains("manage_guild"), "unexpected: {err}");

    handle.abort();
}

#[tokio::test]
async fn a_picture_is_not_a_sound() {
    let (url, handle) = spawn_gateway().await;
    let mut owner = connect_user(&url, &BotIdentity::generate(), "Owner").await;
    let guild_id = create_guild(&mut owner).await;

    let png = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
    owner
        .send(&ClientMessage::CreateGuildSound {
            guild_id,
            name: "pixel".into(),
            audio: png.into(),
        })
        .await
        .unwrap();
    assert!(next_error(&mut owner).await.contains("MP3, OGG or WAV"));

    let mislabelled = png.replacen("image/png", "audio/mpeg", 1);
    owner
        .send(&ClientMessage::CreateGuildSound {
            guild_id,
            name: "pixel".into(),
            audio: mislabelled,
        })
        .await
        .unwrap();
    assert!(
        next_error(&mut owner)
            .await
            .contains("unsupported sound format")
    );

    handle.abort();
}

#[tokio::test]
async fn a_play_reaches_the_voice_channel_and_nobody_else() {
    let (url, handle) = spawn_gateway().await;
    let owner_id = BotIdentity::generate();
    let mut owner = connect_user(&url, &owner_id, "Owner").await;
    let guild_id = create_guild(&mut owner).await;
    let voice = create_voice_channel(&mut owner, guild_id).await;
    let sound_id = add_sound(&mut owner, guild_id, "tada").await[0].id;

    let listener_id = BotIdentity::generate();
    let mut listener = connect_user(&url, &listener_id, "Listener").await;
    join_guild(&mut listener, guild_id).await;
    let mut elsewhere = connect_user(&url, &BotIdentity::generate(), "Elsewhere").await;
    join_guild(&mut elsewhere, guild_id).await;

    for s in [&mut owner, &mut listener] {
        s.send(&ClientMessage::JoinVoice {
            channel_id: voice,
            preferences: None,
        })
        .await
        .unwrap();
    }
    // Both joins fan out as voice state updates; wait for the listener's own
    // before playing, or the play can land before the server has them in.
    let listener_key = listener_id.pubkey().to_string();
    loop {
        if let ServerMessage::VoiceStateUpdate(vs) = next_timeout(&mut owner).await
            && vs.user_pubkey == listener_key
            && vs.channel_id == Some(voice)
        {
            break;
        }
    }

    owner
        .send(&ClientMessage::PlaySound { sound_id })
        .await
        .unwrap();
    loop {
        if let ServerMessage::SoundPlayed {
            channel_id,
            user_pubkey,
            sound_id: played,
        } = next_timeout(&mut listener).await
        {
            assert_eq!(channel_id, voice);
            assert_eq!(user_pubkey, owner_id.pubkey());
            assert_eq!(played, sound_id);
            break;
        }
    }

    let quiet = async {
        loop {
            if let Some(ServerMessage::SoundPlayed { .. }) = elsewhere.next_event().await {
                return;
            }
        }
    };
    assert!(
        tokio::time::timeout(Duration::from_millis(700), quiet)
            .await
            .is_err(),
        "a member outside the channel heard it"
    );

    handle.abort();
}
