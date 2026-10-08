use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use dioxusfun_bot::{Bot, BotIdentity};
use dioxusfun_server::livekit::{
    BoxFuture, LiveKitConfig, MintRequest, VoiceTokenMinter, screen_audio_identity,
    screen_room_name, screen_video_identity, screen_viewer_identity,
};
use dioxusfun_server::protocol::{ChannelKind, ClientMessage, Id, ServerMessage};
use livekit_api::access_token::TokenVerifier;

const API_KEY: &str = "devkey";
const API_SECRET: &str = "secret-must-be-at-least-32-chars-long";

fn local_with_fallback() -> LiveKitConfig {
    LiveKitConfig {
        explicit_url: None,
        public_host: Some("203.0.113.5".into()),
        alternate_hosts: vec![
            "203.0.113.5".parse().unwrap(),
            "2800:810::123".parse().unwrap(),
        ],
        fallback: Some(Arc::new(dioxusfun_server::livekit::VoiceFallback::new(
            "wss://last-option".into(),
            Arc::new(ScriptedMinter {
                fail_when: Box::new(|_| false),
            }),
        ))),
        ..local_signing()
    }
}

async fn remote_gateway(cfg: LiveKitConfig) -> (String, dioxusfun_server::ServerHandle) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut config = test_config(cfg);
    config
        .identities
        .extend(dioxusfun_server::local_identities(addr.port()));
    let (router, _shutdown) = dioxusfun_server::build_gateway(config).await.unwrap();
    let router = router.layer(axum::Extension(axum::extract::ConnectInfo(
        "198.51.100.9:45000"
            .parse::<std::net::SocketAddr>()
            .unwrap(),
    )));
    let handle = dioxusfun_server::serve_router(listener, router);
    (format!("ws://{addr}"), handle)
}

async fn shared_voice_token(user: &mut Bot, channel: Id) {
    loop {
        if let ServerMessage::VoiceToken {
            channel_id,
            livekit_url,
            token,
            alternate_urls,
            route_revision,
            ..
        } = next_timeout(user).await
        {
            assert_eq!(channel_id, channel);
            assert_eq!(livekit_url, "wss://last-option");
            assert_eq!(route_revision, 1);
            assert!(alternate_urls.is_empty());
            assert!(token.starts_with("token-for-"));
            break;
        }
    }
}

#[tokio::test]
async fn failing_local_routes_moves_all_callers_to_one_shared_sfu() {
    let cfg = local_with_fallback();
    let (url, _handle) = remote_gateway(cfg.clone()).await;
    let mut owner = connect_user(&url, &BotIdentity::generate(), "owner").await;
    let (guild, channel) = voice_channel(&mut owner).await;
    let mut friend = connect_user(&url, &BotIdentity::generate(), "friend").await;
    join_guild(&mut friend, guild).await;
    for frames in [
        join_voice(&mut owner, channel).await,
        join_voice(&mut friend, channel).await,
    ] {
        assert!(
            frames
                .iter()
                .any(|frame| matches!(frame, ServerMessage::VoiceToken {
            route_revision: 0, alternate_urls, .. } if !alternate_urls.is_empty()))
        );
    }
    friend
        .send(&ClientMessage::VoiceConnectionReport {
            channel_id: Id::new_v4(),
            route_revision: 0,
            connected: false,
        })
        .await
        .unwrap();
    drain_quiet(&mut friend).await;
    assert_eq!(
        cfg.route().1,
        0,
        "a report for another channel cannot move voice"
    );
    friend
        .send(&ClientMessage::VoiceConnectionReport {
            channel_id: channel,
            route_revision: 0,
            connected: false,
        })
        .await
        .unwrap();
    shared_voice_token(&mut owner, channel).await;
    shared_voice_token(&mut friend, channel).await;
    assert_eq!(cfg.route().1, 1);
    let frames = join_voice(&mut friend, channel).await;
    assert!(frames.iter().any(
        |f| matches!(f, ServerMessage::VoiceToken { route_revision: 1, livekit_url, .. }
        if livekit_url == "wss://last-option")
    ));
}

#[tokio::test]
async fn a_working_remote_local_call_is_not_moved_by_another_failure() {
    let cfg = local_with_fallback();
    let (url, _handle) = remote_gateway(cfg.clone()).await;
    let mut owner = connect_user(&url, &BotIdentity::generate(), "owner").await;
    let (_, channel) = voice_channel(&mut owner).await;
    join_voice(&mut owner, channel).await;
    for connected in [true, false] {
        owner
            .send(&ClientMessage::VoiceConnectionReport {
                channel_id: channel,
                route_revision: 0,
                connected,
            })
            .await
            .unwrap();
    }
    let frames = drain_quiet(&mut owner).await;
    assert!(
        !frames
            .iter()
            .any(|f| matches!(f, ServerMessage::VoiceRouteChanged { .. }))
    );
    assert_eq!(cfg.route().1, 0);
}

#[tokio::test]
async fn loopback_report_cannot_choose_a_remote_sfu() {
    let cfg = local_with_fallback();
    let (url, _handle) = spawn_gateway(cfg.clone()).await;
    let mut owner = connect_user(&url, &BotIdentity::generate(), "owner").await;
    let (_, channel) = voice_channel(&mut owner).await;
    join_voice(&mut owner, channel).await;
    owner
        .send(&ClientMessage::VoiceConnectionReport {
            channel_id: channel,
            route_revision: 0,
            connected: false,
        })
        .await
        .unwrap();
    drain_quiet(&mut owner).await;
    assert_eq!(cfg.route().1, 0);
}

async fn next_timeout(session: &mut Bot) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(5), session.next_event())
        .await
        .expect("timed out waiting for a gateway event")
        .expect("connection closed unexpectedly")
}

fn test_config(livekit: LiveKitConfig) -> dioxusfun_server::ServerConfig {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "dioxusfun-voice-test-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    dioxusfun_server::ServerConfig {
        livekit,
        operators: Default::default(),
        identities: Default::default(),
        media_max_bytes: dioxusfun_server::media::DEFAULT_MAX_BYTES,
        data_dir: dir,
    }
}

fn local_signing() -> LiveKitConfig {
    LiveKitConfig {
        explicit_url: Some("ws://127.0.0.1:7880".into()),
        port: 7880,
        lan_host: None,
        public_host: None,
        alternate_hosts: Vec::new(),
        fallback: None,
        api_key: API_KEY.into(),
        api_secret: API_SECRET.into(),
        minter: None,
        ice_servers: Default::default(),
    }
}

struct ScriptedMinter {
    fail_when: Box<dyn Fn(&MintRequest) -> bool + Send + Sync>,
}

impl VoiceTokenMinter for ScriptedMinter {
    fn mint<'a>(&'a self, req: MintRequest) -> BoxFuture<'a, Result<String, String>> {
        let fail = (self.fail_when)(&req);
        Box::pin(async move {
            if fail {
                Err(format!("scripted failure for room {}", req.room))
            } else {
                Ok(format!("token-for-{}-as-{}", req.room, req.identity))
            }
        })
    }
}

struct RecordingMinter {
    seen: Arc<std::sync::Mutex<Vec<(String, String, bool)>>>,
}

impl VoiceTokenMinter for RecordingMinter {
    fn mint<'a>(&'a self, req: MintRequest) -> BoxFuture<'a, Result<String, String>> {
        self.seen.lock().expect("recording minter poisoned").push((
            req.room.clone(),
            req.identity.clone(),
            req.can_publish,
        ));
        Box::pin(async move { Ok(format!("token-for-{}-as-{}", req.room, req.identity)) })
    }
}

/// Answers only once released, so a test can hold the token service hostage.
struct StallingMinter {
    released: tokio::sync::watch::Receiver<bool>,
}

impl VoiceTokenMinter for StallingMinter {
    fn mint<'a>(&'a self, req: MintRequest) -> BoxFuture<'a, Result<String, String>> {
        let mut released = self.released.clone();
        Box::pin(async move {
            released.wait_for(|v| *v).await.map_err(|e| e.to_string())?;
            Ok(format!("token-for-{}-as-{}", req.room, req.identity))
        })
    }
}

fn stalling() -> (LiveKitConfig, tokio::sync::watch::Sender<bool>) {
    let (tx, rx) = tokio::sync::watch::channel(false);
    let cfg = LiveKitConfig {
        minter: Some(Arc::new(StallingMinter { released: rx })),
        ice_servers: Default::default(),
        ..local_signing()
    };
    (cfg, tx)
}

async fn text_channel_of(owner: &mut Bot) -> (Id, Id) {
    owner
        .send(&ClientMessage::CreateGuild {
            name: "Voice Test".into(),
            template: None,
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::GuildJoined {
            guild, channels, ..
        } = next_timeout(owner).await
        {
            let text = channels
                .iter()
                .find(|c| c.kind == ChannelKind::Text)
                .expect("guild has a text channel")
                .id;
            return (guild.id, text);
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

fn delegated(fail_when: impl Fn(&MintRequest) -> bool + Send + Sync + 'static) -> LiveKitConfig {
    LiveKitConfig {
        minter: Some(Arc::new(ScriptedMinter {
            fail_when: Box::new(fail_when),
        })),
        ..local_signing()
    }
}

async fn spawn_gateway(livekit: LiveKitConfig) -> (String, dioxusfun_server::ServerHandle) {
    let preferred: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let handle = dioxusfun_server::spawn(preferred, 100, test_config(livekit))
        .await
        .expect("spawn server");
    let url = format!("ws://{}", handle.addr);
    (url, handle)
}

async fn connect_user(url: &str, id: &BotIdentity, name: &str) -> Bot {
    let mut session = Bot::connect_as_user(url, id, name).await.unwrap();
    loop {
        if let ServerMessage::Ready { .. } = next_timeout(&mut session).await {
            break;
        }
    }
    session
}

async fn voice_channel(owner: &mut Bot) -> (Id, Id) {
    owner
        .send(&ClientMessage::CreateGuild {
            name: "Voice Test".into(),
            template: None,
        })
        .await
        .unwrap();
    let guild_id = loop {
        if let ServerMessage::GuildJoined { guild, .. } = next_timeout(owner).await {
            break guild.id;
        }
    };
    owner
        .send(&ClientMessage::CreateChannel {
            guild_id,
            name: "General".into(),
            kind: ChannelKind::Voice,
            topic: None,
        })
        .await
        .unwrap();
    let channel_id = loop {
        if let ServerMessage::ChannelCreate(ch) = next_timeout(owner).await
            && ch.kind == ChannelKind::Voice
        {
            break ch.id;
        }
    };
    (guild_id, channel_id)
}

async fn join_voice(session: &mut Bot, channel_id: Id) -> Vec<ServerMessage> {
    session
        .send(&ClientMessage::JoinVoice {
            channel_id,
            preferences: None,
        })
        .await
        .unwrap();
    let mut out = Vec::new();
    loop {
        let window = if out.is_empty() {
            Duration::from_secs(5)
        } else {
            Duration::from_millis(700)
        };
        match tokio::time::timeout(window, session.next_event()).await {
            Ok(Some(msg)) => out.push(msg),
            _ => break,
        }
    }
    out
}

fn screen_token(frames: &[ServerMessage]) -> Option<(String, String, String)> {
    frames.iter().find_map(|m| match m {
        ServerMessage::ScreenToken {
            token,
            audio_token,
            video_token,
            ..
        } => Some((token.clone(), audio_token.clone(), video_token.clone())),
        _ => None,
    })
}

fn errors(frames: &[ServerMessage]) -> Vec<String> {
    frames
        .iter()
        .filter_map(|m| match m {
            ServerMessage::Error { message } => Some(message.clone()),
            _ => None,
        })
        .collect()
}

fn has_voice_token(frames: &[ServerMessage]) -> bool {
    frames
        .iter()
        .any(|m| matches!(m, ServerMessage::VoiceToken { .. }))
}

#[tokio::test]
async fn ice_servers_ride_with_both_grants() {
    let ice = vec![dioxusfun_server::protocol::IceServer {
        urls: vec!["turn:relay.example:7702".into()],
        username: "1700000000".into(),
        credential: "hmac".into(),
    }];
    let cfg = LiveKitConfig {
        ice_servers: dioxusfun_server::livekit::shared_ice_servers(ice.clone()),
        ..local_signing()
    };
    let (url, _handle) = spawn_gateway(cfg).await;
    let id = BotIdentity::generate();
    let mut user = connect_user(&url, &id, "relayed").await;
    let (guild_id, _) = text_channel_of(&mut user).await;
    let voice = create_voice_channel(&mut user, guild_id).await;
    user.send(&ClientMessage::JoinVoice {
        channel_id: voice,
        preferences: None,
    })
    .await
    .unwrap();
    let frames = drain_quiet(&mut user).await;
    let on_voice = frames.iter().find_map(|m| match m {
        ServerMessage::VoiceToken { ice_servers, .. } => Some(ice_servers.clone()),
        _ => None,
    });
    let on_screen = frames.iter().find_map(|m| match m {
        ServerMessage::ScreenToken { ice_servers, .. } => Some(ice_servers.clone()),
        _ => None,
    });
    assert_eq!(on_voice.as_deref(), Some(ice.as_slice()), "{frames:?}");
    assert_eq!(on_screen.as_deref(), Some(ice.as_slice()), "{frames:?}");
}

#[tokio::test]
async fn the_first_join_state_respects_preferences_and_legacy_moves_keep_them() {
    let (url, _handle) = spawn_gateway(local_signing()).await;
    let id = BotIdentity::generate();
    let mut user = connect_user(&url, &id, "talker").await;
    let (guild_id, _) = text_channel_of(&mut user).await;
    let voice = create_voice_channel(&mut user, guild_id).await;
    let other_voice = create_voice_channel(&mut user, guild_id).await;

    for muted in [false, true] {
        for deafened in [false, true] {
            user.send(&ClientMessage::JoinVoice {
                channel_id: voice,
                preferences: Some(dioxusfun_server::protocol::VoicePreferences { muted, deafened }),
            })
            .await
            .unwrap();
            let first = next_voice_state(&mut user, id.pubkey()).await;
            assert_eq!(first.channel_id, Some(voice));
            assert_eq!(first.muted, muted || deafened);
            assert_eq!(first.deafened, deafened);

            user.send(&ClientMessage::JoinVoice {
                channel_id: other_voice,
                preferences: None,
            })
            .await
            .unwrap();
            let left = next_voice_state(&mut user, id.pubkey()).await;
            assert_eq!(left.channel_id, None);
            let moved = next_voice_state(&mut user, id.pubkey()).await;
            assert_eq!(moved.channel_id, Some(other_voice));
            assert_eq!(moved.muted, muted || deafened);
            assert_eq!(moved.deafened, deafened);

            user.send(&ClientMessage::LeaveVoice).await.unwrap();
            assert_eq!(
                next_voice_state(&mut user, id.pubkey()).await.channel_id,
                None
            );
        }
    }
}

#[tokio::test]
async fn a_stalled_token_service_does_not_stall_the_socket() {
    let (cfg, _hold) = stalling();
    let (url, _handle) = spawn_gateway(cfg).await;
    let id = BotIdentity::generate();
    let mut user = connect_user(&url, &id, "talker").await;
    let (guild_id, text) = text_channel_of(&mut user).await;
    let voice = create_voice_channel(&mut user, guild_id).await;

    user.send(&ClientMessage::JoinVoice {
        channel_id: voice,
        preferences: None,
    })
    .await
    .unwrap();
    user.send(&ClientMessage::SendMessage {
        channel_id: text,
        content: "still here".into(),
        image: None,
        reply_to: None,
    })
    .await
    .unwrap();

    let mut seen = Vec::new();
    loop {
        let frame = next_timeout(&mut user).await;
        if let ServerMessage::MessageCreate(m) = &frame
            && m.content == "still here"
        {
            break;
        }
        seen.push(frame);
    }
    assert!(
        !has_voice_token(&seen),
        "a token appeared while the minter was held: {seen:?}"
    );
}

#[tokio::test]
async fn a_token_minted_after_leaving_is_not_delivered() {
    let (cfg, release) = stalling();
    let (url, _handle) = spawn_gateway(cfg).await;
    let id = BotIdentity::generate();
    let mut user = connect_user(&url, &id, "leaver").await;
    let (guild_id, _text) = text_channel_of(&mut user).await;
    let voice = create_voice_channel(&mut user, guild_id).await;

    user.send(&ClientMessage::JoinVoice {
        channel_id: voice,
        preferences: None,
    })
    .await
    .unwrap();
    user.send(&ClientMessage::LeaveVoice).await.unwrap();
    let before = drain_quiet(&mut user).await;
    assert!(!has_voice_token(&before), "{before:?}");

    release.send(true).unwrap();
    let after = drain_quiet(&mut user).await;
    assert!(
        !has_voice_token(&after),
        "a join answered after the leave: {after:?}"
    );
}

#[tokio::test]
async fn join_voice_mints_four_screen_identities_with_the_right_grants() {
    let (url, _handle) = spawn_gateway(local_signing()).await;
    let id = BotIdentity::generate();
    let mut user = connect_user(&url, &id, "sharer").await;
    let (_guild_id, channel_id) = voice_channel(&mut user).await;

    let frames = join_voice(&mut user, channel_id).await;
    assert!(has_voice_token(&frames), "no VoiceToken in {frames:?}");
    assert!(errors(&frames).is_empty(), "unexpected errors: {frames:?}");

    let (main, audio, video) = screen_token(&frames).expect("no ScreenToken");
    let viewer = frames
        .iter()
        .find_map(|m| match m {
            ServerMessage::ScreenToken { viewer_token, .. } => Some(viewer_token),
            _ => None,
        })
        .expect("viewer token present");
    for (what, t) in [("main", &main), ("audio", &audio), ("video", &video)] {
        assert!(
            !t.is_empty(),
            "{what} token is empty — a mint failed silently"
        );
    }

    let verifier = TokenVerifier::with_api_key(API_KEY, API_SECRET);
    let pubkey = id.pubkey();
    let claims = |t: &str| verifier.verify(t).expect("token verifies").video;
    let sub = |t: &str| verifier.verify(t).expect("token verifies").sub;

    assert_eq!(sub(&main), pubkey);
    assert_eq!(sub(&audio), screen_audio_identity(pubkey));
    assert_eq!(sub(&video), screen_video_identity(pubkey));
    assert_eq!(sub(viewer), screen_viewer_identity(pubkey));
    let room = claims(&main).room;
    assert!(room.starts_with("screen-"), "unexpected room {room}");
    assert_eq!(claims(&audio).room, room);
    assert_eq!(claims(&video).room, room);
    assert_eq!(claims(viewer).room, room);
    assert!(!claims(viewer).can_publish);
    assert!(!claims(viewer).can_publish_data);

    assert!(claims(&main).can_publish, "webview must be able to capture");
    assert!(
        claims(&video).can_publish,
        "native video publisher must publish"
    );
    assert!(
        !claims(&audio).can_publish,
        "the subscribe-only identity was minted with publish rights"
    );
    assert!(!claims(&audio).can_publish_data);
    for t in [&main, &audio, &video, viewer] {
        assert!(claims(t).can_subscribe);
        assert!(claims(t).room_join);
    }
}

#[tokio::test]
async fn a_failed_viewer_mint_preserves_in_app_streams_and_voice() {
    let (url, _handle) = spawn_gateway(delegated(|req| req.identity.ends_with("#viewer"))).await;
    let id = BotIdentity::generate();
    let mut user = connect_user(&url, &id, "viewer").await;
    let (_guild_id, channel_id) = voice_channel(&mut user).await;
    let frames = join_voice(&mut user, channel_id).await;
    assert!(has_voice_token(&frames));
    assert!(errors(&frames).is_empty());
    let (main, audio, video) = screen_token(&frames).expect("screen tokens remain available");
    assert!(!main.is_empty() && !audio.is_empty() && !video.is_empty());
    assert!(frames.iter().any(
        |m| matches!(m, ServerMessage::ScreenToken { viewer_token, .. } if viewer_token.is_empty())
    ));
}

#[tokio::test]
async fn a_failed_screen_mint_reports_and_sends_no_token() {
    let (url, _handle) = spawn_gateway(delegated(|req| req.room.starts_with("screen-"))).await;
    let id = BotIdentity::generate();
    let mut user = connect_user(&url, &id, "sharer").await;
    let (_guild_id, channel_id) = voice_channel(&mut user).await;

    let frames = join_voice(&mut user, channel_id).await;

    assert!(has_voice_token(&frames), "voice should still work");
    assert!(
        screen_token(&frames).is_none(),
        "a ScreenToken was sent despite the mint failing: {frames:?}"
    );
    let errs = errors(&frames);
    assert!(
        errs.iter().any(|e| e.contains("screen-share token mint")),
        "no error explained the failure: {errs:?}"
    );
}

#[tokio::test]
async fn a_failed_video_mint_still_sends_the_frame_and_explains_itself() {
    let (url, _handle) = spawn_gateway(delegated(|req| req.identity.ends_with("#video"))).await;
    let id = BotIdentity::generate();
    let mut user = connect_user(&url, &id, "sharer").await;
    let (_guild_id, channel_id) = voice_channel(&mut user).await;

    let frames = join_voice(&mut user, channel_id).await;

    let (main, audio, video) = screen_token(&frames).expect("frame should still be sent");
    assert!(!main.is_empty());
    assert!(!audio.is_empty());
    assert!(video.is_empty(), "video token should be empty on failure");
    let errs = errors(&frames);
    assert!(
        errs.iter().any(|e| e.contains("video token mint")),
        "the user was not told the video mint failed: {errs:?}"
    );
}

#[tokio::test]
async fn a_failed_audio_mint_is_not_reported_to_the_user() {
    let (url, _handle) = spawn_gateway(delegated(|req| req.identity.ends_with("#audio"))).await;
    let id = BotIdentity::generate();
    let mut user = connect_user(&url, &id, "sharer").await;
    let (_guild_id, channel_id) = voice_channel(&mut user).await;

    let frames = join_voice(&mut user, channel_id).await;

    let (main, audio, video) = screen_token(&frames).expect("frame should still be sent");
    assert!(!main.is_empty());
    assert!(audio.is_empty(), "audio token should be empty on failure");
    assert!(!video.is_empty());
    assert!(
        errors(&frames).is_empty(),
        "a working fallback should not raise an error: {frames:?}"
    );
}

async fn join_guild(
    session: &mut Bot,
    guild_id: Id,
) -> Vec<dioxusfun_server::protocol::VoiceState> {
    session
        .send(&ClientMessage::JoinGuild {
            guild_id,
            accept: false,
            pow_nonce: None,
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::GuildJoined {
            guild,
            voice_states,
            ..
        } = next_timeout(session).await
            && guild.id == guild_id
        {
            break voice_states;
        }
    }
}

async fn next_voice_state(
    session: &mut Bot,
    pubkey: &str,
) -> dioxusfun_server::protocol::VoiceState {
    loop {
        if let ServerMessage::VoiceStateUpdate(vs) = next_timeout(session).await
            && vs.user_pubkey == pubkey
        {
            return vs;
        }
    }
}

async fn drain_quiet(session: &mut Bot) -> Vec<ServerMessage> {
    let mut out = Vec::new();
    while let Ok(Some(msg)) =
        tokio::time::timeout(Duration::from_millis(700), session.next_event()).await
    {
        out.push(msg);
    }
    out
}

fn camera_states(frames: &[ServerMessage], pubkey: &str) -> Vec<bool> {
    frames
        .iter()
        .filter_map(|m| match m {
            ServerMessage::VoiceStateUpdate(vs) if vs.user_pubkey == pubkey => Some(vs.camera_on),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn camera_flag_reaches_the_other_members() {
    let (url, _handle) = spawn_gateway(local_signing()).await;
    let owner_id = BotIdentity::generate();
    let member_id = BotIdentity::generate();
    let mut owner = connect_user(&url, &owner_id, "owner").await;
    let (guild_id, channel_id) = voice_channel(&mut owner).await;

    let mut member = connect_user(&url, &member_id, "member").await;
    join_guild(&mut member, guild_id).await;
    let _ = join_voice(&mut member, channel_id).await;
    let _ = drain_quiet(&mut owner).await;

    member
        .send(&ClientMessage::SetCamera { on: true })
        .await
        .unwrap();
    let vs = next_voice_state(&mut owner, member_id.pubkey()).await;
    assert!(vs.camera_on, "the owner should see the camera come on");
    assert_eq!(
        vs.channel_id,
        Some(channel_id),
        "and know which channel it is in"
    );

    member
        .send(&ClientMessage::SetCamera { on: false })
        .await
        .unwrap();
    let vs = next_voice_state(&mut owner, member_id.pubkey()).await;
    assert!(!vs.camera_on, "and see it go off again");
}

#[tokio::test]
async fn leaving_voice_clears_the_camera() {
    let (url, _handle) = spawn_gateway(local_signing()).await;
    let owner_id = BotIdentity::generate();
    let member_id = BotIdentity::generate();
    let mut owner = connect_user(&url, &owner_id, "owner").await;
    let (guild_id, channel_id) = voice_channel(&mut owner).await;

    let mut member = connect_user(&url, &member_id, "member").await;
    join_guild(&mut member, guild_id).await;
    let _ = join_voice(&mut member, channel_id).await;
    let _ = drain_quiet(&mut owner).await;
    member
        .send(&ClientMessage::SetCamera { on: true })
        .await
        .unwrap();
    let vs = next_voice_state(&mut owner, member_id.pubkey()).await;
    assert!(vs.camera_on, "precondition: the camera is on");

    member.send(&ClientMessage::LeaveVoice).await.unwrap();

    let vs = next_voice_state(&mut owner, member_id.pubkey()).await;
    assert_eq!(vs.channel_id, None, "precondition: this is the tombstone");
    assert!(
        !vs.camera_on,
        "the tombstone must not carry a live camera flag"
    );
}

#[tokio::test]
async fn camera_outside_voice_is_ignored() {
    let (url, _handle) = spawn_gateway(local_signing()).await;
    let owner_id = BotIdentity::generate();
    let outsider_id = BotIdentity::generate();
    let mut owner = connect_user(&url, &owner_id, "owner").await;
    let (_guild_id, _channel_id) = voice_channel(&mut owner).await;

    let mut outsider = connect_user(&url, &outsider_id, "outsider").await;
    let _ = drain_quiet(&mut owner).await;
    outsider
        .send(&ClientMessage::SetCamera { on: true })
        .await
        .unwrap();

    let frames = drain_quiet(&mut owner).await;
    assert!(
        camera_states(&frames, outsider_id.pubkey()).is_empty(),
        "a non-member's camera must not be announced: {frames:?}"
    );
}

#[tokio::test]
async fn a_repeated_camera_on_broadcasts_once() {
    let (url, _handle) = spawn_gateway(local_signing()).await;
    let owner_id = BotIdentity::generate();
    let member_id = BotIdentity::generate();
    let mut owner = connect_user(&url, &owner_id, "owner").await;
    let (guild_id, channel_id) = voice_channel(&mut owner).await;

    let mut member = connect_user(&url, &member_id, "member").await;
    join_guild(&mut member, guild_id).await;
    let _ = join_voice(&mut member, channel_id).await;
    let _ = drain_quiet(&mut owner).await;

    for _ in 0..3 {
        member
            .send(&ClientMessage::SetCamera { on: true })
            .await
            .unwrap();
    }

    let frames = drain_quiet(&mut owner).await;
    assert_eq!(
        camera_states(&frames, member_id.pubkey()),
        vec![true],
        "three identical toggles should announce once: {frames:?}"
    );
}

fn share_states(frames: &[ServerMessage], pubkey: &str) -> Vec<bool> {
    frames
        .iter()
        .filter_map(|m| match m {
            ServerMessage::VoiceStateUpdate(vs) if vs.user_pubkey == pubkey => {
                Some(vs.screen_sharing)
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn screen_watchers_are_explicit_deduplicated_and_cleared_on_stop_or_leave() {
    let (url, _handle) = spawn_gateway(local_signing()).await;
    let owner_id = BotIdentity::generate();
    let sharer_id = BotIdentity::generate();
    let mut owner = connect_user(&url, &owner_id, "viewer").await;
    let (guild_id, channel_id) = voice_channel(&mut owner).await;
    let _ = join_voice(&mut owner, channel_id).await;
    let mut sharer = connect_user(&url, &sharer_id, "sharer").await;
    join_guild(&mut sharer, guild_id).await;
    let _ = join_voice(&mut sharer, channel_id).await;
    let _ = drain_quiet(&mut owner).await;
    sharer
        .send(&ClientMessage::SetScreenShare {
            channel_id,
            sharing: true,
        })
        .await
        .unwrap();
    let _ = next_voice_state(&mut owner, sharer_id.pubkey()).await;
    assert!(
        drain_quiet(&mut sharer)
            .await
            .iter()
            .all(|frame| !matches!(frame,
        ServerMessage::VoiceStateUpdate(vs) if !vs.screen_watching.is_empty()))
    );

    owner
        .send(&ClientMessage::SetScreenWatching {
            sharers: vec![
                sharer_id.pubkey().into(),
                sharer_id.pubkey().into(),
                owner_id.pubkey().into(),
                "unknown".into(),
            ],
        })
        .await
        .unwrap();
    assert_eq!(
        next_voice_state(&mut sharer, owner_id.pubkey())
            .await
            .screen_watching,
        vec![sharer_id.pubkey().to_string()]
    );
    let _ = drain_quiet(&mut owner).await;
    owner
        .send(&ClientMessage::SetScreenWatching {
            sharers: Vec::new(),
        })
        .await
        .unwrap();
    assert!(
        next_voice_state(&mut sharer, owner_id.pubkey())
            .await
            .screen_watching
            .is_empty()
    );
    owner
        .send(&ClientMessage::SetScreenWatching {
            sharers: vec![sharer_id.pubkey().into()],
        })
        .await
        .unwrap();
    let _ = next_voice_state(&mut sharer, owner_id.pubkey()).await;
    let _ = drain_quiet(&mut owner).await;
    sharer
        .send(&ClientMessage::SetScreenShare {
            channel_id,
            sharing: false,
        })
        .await
        .unwrap();
    assert!(
        next_voice_state(&mut owner, owner_id.pubkey())
            .await
            .screen_watching
            .is_empty()
    );

    sharer
        .send(&ClientMessage::SetScreenShare {
            channel_id,
            sharing: true,
        })
        .await
        .unwrap();
    let _ = next_voice_state(&mut owner, sharer_id.pubkey()).await;
    let _ = drain_quiet(&mut sharer).await;
    owner
        .send(&ClientMessage::SetScreenWatching {
            sharers: vec![sharer_id.pubkey().into()],
        })
        .await
        .unwrap();
    assert_eq!(
        next_voice_state(&mut sharer, owner_id.pubkey())
            .await
            .screen_watching
            .len(),
        1
    );
    owner.send(&ClientMessage::LeaveVoice).await.unwrap();
    let left = next_voice_state(&mut sharer, owner_id.pubkey()).await;
    assert!(left.channel_id.is_none());
    assert!(left.screen_watching.is_empty());
}

fn legacy_sharers(frames: &[ServerMessage]) -> Vec<Vec<String>> {
    frames
        .iter()
        .filter_map(|m| match m {
            ServerMessage::ScreenShareState { sharers, .. } => Some(sharers.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn sharing_announces_on_both_the_new_flag_and_the_legacy_frame() {
    let (url, _handle) = spawn_gateway(local_signing()).await;
    let owner_id = BotIdentity::generate();
    let member_id = BotIdentity::generate();
    let mut owner = connect_user(&url, &owner_id, "owner").await;
    let (guild_id, channel_id) = voice_channel(&mut owner).await;

    let mut member = connect_user(&url, &member_id, "member").await;
    join_guild(&mut member, guild_id).await;
    let _ = join_voice(&mut member, channel_id).await;
    let _ = drain_quiet(&mut owner).await;

    member
        .send(&ClientMessage::SetScreenShare {
            channel_id,
            sharing: true,
        })
        .await
        .unwrap();
    let frames = drain_quiet(&mut owner).await;

    assert_eq!(
        share_states(&frames, member_id.pubkey()),
        vec![true],
        "the flag should reach the other member exactly once: {frames:?}"
    );
    assert_eq!(
        legacy_sharers(&frames),
        vec![vec![member_id.pubkey().to_string()]],
        "and the legacy frame should carry the same person: {frames:?}"
    );
}

#[tokio::test]
async fn a_reconnecting_member_sees_a_share_already_in_progress() {
    let (url, _handle) = spawn_gateway(local_signing()).await;
    let owner_id = BotIdentity::generate();
    let sharer_id = BotIdentity::generate();
    let mut owner = connect_user(&url, &owner_id, "owner").await;
    let (guild_id, channel_id) = voice_channel(&mut owner).await;

    let mut sharer = connect_user(&url, &sharer_id, "sharer").await;
    join_guild(&mut sharer, guild_id).await;
    let _ = join_voice(&mut sharer, channel_id).await;
    sharer
        .send(&ClientMessage::SetScreenShare {
            channel_id,
            sharing: true,
        })
        .await
        .unwrap();
    let _ = drain_quiet(&mut owner).await;

    drop(owner);
    let mut owner = Bot::connect_as_user(&url, &owner_id, "owner")
        .await
        .unwrap();
    let ready = loop {
        if let ServerMessage::Ready { voice_states, .. } = next_timeout(&mut owner).await {
            break voice_states;
        }
    };

    let sharing = ready
        .iter()
        .find(|v| v.user_pubkey == sharer_id.pubkey())
        .map(|v| v.screen_sharing);
    assert_eq!(
        sharing,
        Some(true),
        "Ready's voice roster should say the share is live: {ready:?}"
    );
}

#[tokio::test]
async fn leaving_voice_clears_the_share() {
    let (url, _handle) = spawn_gateway(local_signing()).await;
    let owner_id = BotIdentity::generate();
    let member_id = BotIdentity::generate();
    let mut owner = connect_user(&url, &owner_id, "owner").await;
    let (guild_id, channel_id) = voice_channel(&mut owner).await;

    let mut member = connect_user(&url, &member_id, "member").await;
    join_guild(&mut member, guild_id).await;
    let _ = join_voice(&mut member, channel_id).await;
    member
        .send(&ClientMessage::SetScreenShare {
            channel_id,
            sharing: true,
        })
        .await
        .unwrap();
    let _ = drain_quiet(&mut owner).await;

    member.send(&ClientMessage::LeaveVoice).await.unwrap();
    let frames = drain_quiet(&mut owner).await;

    assert!(
        share_states(&frames, member_id.pubkey())
            .last()
            .map(|s| !s)
            .unwrap_or(false),
        "the tombstone must not carry a live share flag: {frames:?}"
    );
    assert_eq!(
        legacy_sharers(&frames).last().cloned(),
        Some(Vec::new()),
        "and the legacy frame should go empty: {frames:?}"
    );
}

#[tokio::test]
async fn a_delegated_mint_is_told_which_connection_may_publish() {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let cfg = LiveKitConfig {
        minter: Some(Arc::new(RecordingMinter { seen: seen.clone() })),
        ice_servers: Default::default(),
        ..local_signing()
    };
    let (url, _handle) = spawn_gateway(cfg).await;
    let id = BotIdentity::generate();
    let mut user = connect_user(&url, &id, "sharer").await;
    let (_guild_id, channel_id) = voice_channel(&mut user).await;

    let frames = join_voice(&mut user, channel_id).await;
    assert!(errors(&frames).is_empty(), "unexpected errors: {frames:?}");

    let seen = seen.lock().expect("recording minter poisoned").clone();
    let screen = screen_room_name(channel_id);
    let pubkey = id.pubkey();
    let asked = |identity: &str| {
        seen.iter()
            .find(|(room, who, _)| room == &screen && who == identity)
            .map(|(_, _, can_publish)| *can_publish)
            .unwrap_or_else(|| panic!("no mint for {identity} in {screen}: {seen:?}"))
    };

    assert!(asked(pubkey), "the webview identity has to publish");
    assert!(
        !asked(&screen_audio_identity(pubkey)),
        "the subscribe-only identity must not be granted publish"
    );
    assert!(
        asked(&screen_video_identity(pubkey)),
        "the native video publisher has to publish"
    );
}

#[tokio::test]
async fn joining_a_guild_shows_who_is_already_in_voice() {
    let (url, _handle) = spawn_gateway(local_signing()).await;
    let owner_id = BotIdentity::generate();
    let member_id = BotIdentity::generate();
    let mut owner = connect_user(&url, &owner_id, "owner").await;
    let (guild_id, channel_id) = voice_channel(&mut owner).await;

    let _ = join_voice(&mut owner, channel_id).await;
    owner
        .send(&ClientMessage::SetCamera { on: true })
        .await
        .unwrap();
    let vs = next_voice_state(&mut owner, owner_id.pubkey()).await;
    assert!(vs.camera_on, "precondition: the owner's camera is on");

    let mut member = connect_user(&url, &member_id, "member").await;
    let voice_states = join_guild(&mut member, guild_id).await;

    let seen = voice_states
        .iter()
        .find(|v| v.user_pubkey == owner_id.pubkey())
        .unwrap_or_else(|| panic!("GuildJoined carried no state for the owner: {voice_states:?}"));
    assert_eq!(seen.guild_id, guild_id);
    assert_eq!(
        seen.channel_id,
        Some(channel_id),
        "the joiner should know which voice channel they are in"
    );
    assert!(seen.camera_on, "and that their camera is on");
}

#[tokio::test]
async fn the_join_bundle_does_not_disclose_other_guilds() {
    let (url, _handle) = spawn_gateway(local_signing()).await;
    let owner_id = BotIdentity::generate();
    let member_id = BotIdentity::generate();
    let mut owner = connect_user(&url, &owner_id, "owner").await;
    let (elsewhere, elsewhere_channel) = voice_channel(&mut owner).await;
    let (joined_guild, _) = voice_channel(&mut owner).await;

    let _ = join_voice(&mut owner, elsewhere_channel).await;

    let mut member = connect_user(&url, &member_id, "member").await;
    let voice_states = join_guild(&mut member, joined_guild).await;

    assert!(
        voice_states.is_empty(),
        "joining {joined_guild} disclosed voice presence in {elsewhere}: {voice_states:?}"
    );
}

#[tokio::test]
async fn closing_a_secondary_session_preserves_the_other_sessions_call_and_share() {
    let (url, _handle) = spawn_gateway(local_signing()).await;
    let owner_id = BotIdentity::generate();
    let member_id = BotIdentity::generate();
    let mut owner = connect_user(&url, &owner_id, "owner").await;
    let (guild_id, channel_id) = voice_channel(&mut owner).await;
    let mut member = connect_user(&url, &member_id, "member").await;
    join_guild(&mut member, guild_id).await;
    let _ = join_voice(&mut member, channel_id).await;
    member
        .send(&ClientMessage::SetScreenShare {
            channel_id,
            sharing: true,
        })
        .await
        .unwrap();
    let secondary = connect_user(&url, &member_id, "member").await;
    let _ = drain_quiet(&mut owner).await;
    drop(secondary);
    let updates = drain_quiet(&mut owner).await;
    assert!(!updates.iter().any(|message| matches!(message,
        ServerMessage::VoiceStateUpdate(state) if state.user_pubkey == member_id.pubkey() && state.channel_id.is_none()
    )), "closing an idle session must not remove the active session from voice: {updates:?}");
    member
        .send(&ClientMessage::SetVoiceMute {
            muted: true,
            deafened: false,
        })
        .await
        .unwrap();
    let state = next_voice_state(&mut owner, member_id.pubkey()).await;
    assert_eq!(state.channel_id, Some(channel_id));
    assert!(state.screen_sharing && state.muted);
    drop(member);
    let state = next_voice_state(&mut owner, member_id.pubkey()).await;
    assert!(state.channel_id.is_none() && !state.screen_sharing);
}

#[tokio::test]
async fn a_quick_gateway_reconnect_preserves_voice_and_share_after_old_cleanup() {
    let (url, _handle) = spawn_gateway(local_signing()).await;
    let owner_id = BotIdentity::generate();
    let member_id = BotIdentity::generate();
    let mut owner = connect_user(&url, &owner_id, "owner").await;
    let (guild_id, channel_id) = voice_channel(&mut owner).await;
    let mut member = connect_user(&url, &member_id, "member").await;
    join_guild(&mut member, guild_id).await;
    let _ = join_voice(&mut member, channel_id).await;
    member
        .send(&ClientMessage::SetScreenShare {
            channel_id,
            sharing: true,
        })
        .await
        .unwrap();
    let _ = drain_quiet(&mut owner).await;
    drop(member);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut replacement = connect_user(&url, &member_id, "member").await;
    tokio::time::sleep(Duration::from_millis(3200)).await;
    let events = drain_quiet(&mut owner).await;
    assert!(!events.iter().any(|event| matches!(event,
        ServerMessage::VoiceStateUpdate(vs) if vs.user_pubkey == member_id.pubkey() && vs.channel_id.is_none()
    )), "old socket teardown must not remove a recovered call: {events:?}");
    replacement
        .send(&ClientMessage::SetVoiceMute {
            muted: true,
            deafened: false,
        })
        .await
        .unwrap();
    let state = next_voice_state(&mut owner, member_id.pubkey()).await;
    assert_eq!(state.channel_id, Some(channel_id));
    assert!(state.screen_sharing && state.muted);
}

#[tokio::test]
async fn heartbeat_closes_a_session_that_stops_reading_probes() {
    let (url, _handle) = spawn_gateway(local_signing()).await;
    let identity = BotIdentity::generate();
    let mut idle = connect_user(&url, &identity, "idle").await;
    // Reading flushes automatic pongs, so wait past the server's next deadline check.
    tokio::time::sleep(
        dioxusfun_server::protocol::GATEWAY_HEARTBEAT_TIMEOUT
            + 2 * dioxusfun_server::protocol::GATEWAY_HEARTBEAT_INTERVAL,
    )
    .await;
    tokio::time::timeout(Duration::from_secs(3), async {
        while idle.next_event().await.is_some() {}
    })
    .await
    .expect("dead gateway must close its socket");
}
