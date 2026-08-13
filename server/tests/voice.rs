//! End-to-end tests for the voice/screen-share token handshake, and for voice
//! *presence* — who is in a channel and what their camera is doing.
//!
//! `JoinVoice` is the only client message that mints LiveKit credentials, and
//! until now nothing exercised it: the three screen-room identities, their
//! grants, and what happens when a mint fails were all covered by reasoning.
//! Same harness as `owner_controls.rs` — a real gateway, driven over a real
//! WebSocket through the bot SDK's `connect_as_user`.
//!
//! What these can and cannot settle is worth being clear about. They prove what
//! the *server* hands out: which identities, which grants, and which frames on
//! failure. Whether an SFU then lets a `can_publish: false` peer subscribe is a
//! property of LiveKit, not of this code, and needs a live room.
//!
//! The camera tests at the bottom are the first coverage of the
//! `VoiceStateUpdate` fan-out anywhere in the repo — `ScreenShareState`,
//! `SetVoiceMute` and `SetSpeaking` still have none — so they pin the shape of
//! that broadcast as much as they test the camera.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use dioxusfun_bot::{Bot, BotIdentity};
use dioxusfun_server::livekit::{
    BoxFuture, LiveKitConfig, MintRequest, VoiceTokenMinter, screen_audio_identity,
    screen_video_identity,
};
use dioxusfun_server::protocol::{ChannelKind, ClientMessage, Id, ServerMessage};
use livekit_api::access_token::TokenVerifier;

const API_KEY: &str = "devkey";
const API_SECRET: &str = "secret-must-be-at-least-32-chars-long";

async fn next_timeout(session: &mut Bot) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(5), session.next_event())
        .await
        .expect("timed out waiting for a gateway event")
        .expect("connection closed unexpectedly")
}

/// Per-test data dir, counter-based rather than clock-based: `8f95f22` found
/// that two tests in one binary starting together shared a directory when the
/// key was pid + nanos, because macOS resolves that to about a microsecond.
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
        data_dir: dir,
    }
}

/// A config that signs locally, which is the only path where `can_publish`
/// reaches the token — a delegated minter takes no grants (see TODO.md).
fn local_signing() -> LiveKitConfig {
    LiveKitConfig {
        explicit_url: Some("ws://127.0.0.1:7880".into()),
        port: 7880,
        lan_host: None,
        api_key: API_KEY.into(),
        api_secret: API_SECRET.into(),
        minter: None,
    }
}

/// A delegated minter under test control. `fail_when` decides, per request,
/// whether that mint blows up — which is the only way to make one of the three
/// screen mints fail while the others succeed, since a single config's key and
/// secret either work for all of them or none.
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

fn delegated(fail_when: impl Fn(&MintRequest) -> bool + Send + Sync + 'static) -> LiveKitConfig {
    LiveKitConfig {
        minter: Some(Arc::new(ScriptedMinter {
            fail_when: Box::new(fail_when),
        })),
        ..local_signing()
    }
}

async fn spawn_gateway(livekit: LiveKitConfig) -> (String, dioxusfun_server::ServerHandle) {
    // Port 0: let the OS pick, so these run alongside the other suites.
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

/// Create a guild and a voice channel in it, returning both ids.
///
/// The guild id is returned because the camera tests need a *second* member,
/// and joining one means naming the guild.
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

/// Drive a `JoinVoice` and collect what comes back, stopping once the gateway
/// goes quiet. Every arm under test sends between one and three frames, so a
/// short idle window is the honest terminator — a fixed count would pass by
/// accident if the wrong number arrived, and the negative assertions ("no
/// ScreenToken") need a quiet period rather than a count anyway.
///
/// Two windows, not one. The wait for the *first* frame is generous because it
/// includes whatever the runner is doing; the gap between frames is short
/// because there is nothing between them but local HS256 signing. A single
/// 700ms budget would make a loaded CI box look like a server that sent
/// nothing.
async fn join_voice(session: &mut Bot, channel_id: Id) -> Vec<ServerMessage> {
    session
        .send(&ClientMessage::JoinVoice { channel_id })
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

/// The happy path, and the one assertion that matters most: the subscribe-only
/// `#audio` identity is minted **without** publish rights, while the other two
/// keep them. That grant is the change with the highest regression risk in this
/// area, and it was previously only checked at the unit level against
/// `mint_screen_token` directly — not through a real `JoinVoice`.
#[tokio::test]
async fn join_voice_mints_three_identities_with_the_right_grants() {
    let (url, _handle) = spawn_gateway(local_signing()).await;
    let id = BotIdentity::generate();
    let mut user = connect_user(&url, &id, "sharer").await;
    let (_guild_id, channel_id) = voice_channel(&mut user).await;

    let frames = join_voice(&mut user, channel_id).await;
    assert!(has_voice_token(&frames), "no VoiceToken in {frames:?}");
    assert!(errors(&frames).is_empty(), "unexpected errors: {frames:?}");

    let (main, audio, video) = screen_token(&frames).expect("no ScreenToken");
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

    // Identities: bare, #audio, #video, all in the same screen room.
    assert_eq!(sub(&main), pubkey);
    assert_eq!(sub(&audio), screen_audio_identity(pubkey));
    assert_eq!(sub(&video), screen_video_identity(pubkey));
    let room = claims(&main).room;
    assert!(room.starts_with("screen-"), "unexpected room {room}");
    assert_eq!(claims(&audio).room, room);
    assert_eq!(claims(&video).room, room);

    // The grants. Everything subscribes; only the audio-only one is barred from
    // sending, on both the media and the data channel.
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
    for t in [&main, &audio, &video] {
        assert!(claims(t).can_subscribe);
        assert!(claims(t).room_join);
    }
}

/// The main screen token has no fallback, so its failure must suppress the
/// whole frame rather than send one with an empty field: the client stores that
/// field unconditionally and would try to join with an empty token. Before
/// `3df2fd3` this was an `if let Ok`, which sent nothing and said nothing.
#[tokio::test]
async fn a_failed_screen_mint_reports_and_sends_no_token() {
    let (url, _handle) = spawn_gateway(delegated(|req| req.room.starts_with("screen-"))).await;
    let id = BotIdentity::generate();
    let mut user = connect_user(&url, &id, "sharer").await;
    let (_guild_id, channel_id) = voice_channel(&mut user).await;

    let frames = join_voice(&mut user, channel_id).await;

    // Voice is unaffected — the failure is scoped to the screen room.
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

/// The video token *does* have a frame to travel in, so its failure empties one
/// field and tells the user why. Without the message the client blames the
/// server's age: "This server is too old to accept a natively captured screen
/// share" is what an empty video token means on macOS, and it is a diagnosis
/// rather than a fact when the mint simply failed.
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

/// The counterpart: a failed `#audio` mint is deliberately quiet, because the
/// client degrades to playing stream audio in the webview. That still works —
/// it just lands on the system's output device instead of the chosen one — and
/// a message about a working fallback is noise.
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

// ---------------------------------------------------------------------------
// Camera presence
//
// `SetCamera` rides `VoiceStateUpdate` rather than getting a whole-set message
// of its own like `ScreenShareState`. These are the first tests of that fan-out
// in the repo — nothing covers `ScreenShareState`, `SetVoiceMute` or
// `SetSpeaking` at all — so they pin the shape as much as the feature.
// ---------------------------------------------------------------------------

/// Join a guild as a second member. Guilds are public by default, so no invite.
async fn join_guild(session: &mut Bot, guild_id: Id) {
    session
        .send(&ClientMessage::JoinGuild {
            guild_id,
            accept: false,
            pow_nonce: None,
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::GuildJoined { guild, .. } = next_timeout(session).await
            && guild.id == guild_id
        {
            break;
        }
    }
}

/// Wait for a `VoiceStateUpdate` about `pubkey`, ignoring everything else —
/// joining voice emits member and voice traffic these tests do not care about.
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

/// Collect frames until the gateway goes quiet, for the negative assertions.
/// Same reasoning as `join_voice`: silence is the only honest terminator when
/// what you are asserting is that nothing was sent.
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

/// The happy path: a member's camera flag reaches the *other* people in the
/// guild, which is the whole point of routing it through the server rather than
/// letting the SFU's own track events speak for themselves.
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

/// The `..prev` trap in `clear_voice`. The tombstone must carry `camera_on:
/// false`; a spread would carry a live `true` into a frame that says the user
/// is in no channel at all.
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
    // Joining voice emits its own `VoiceStateUpdate` for this member; drain it,
    // or the assertion below reads the join frame and sees a camera that has
    // not been turned on yet.
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

/// `SetCamera` from someone who never joined voice must do nothing at all —
/// this is what makes the missing `is_guild_member` check safe, since only
/// `JoinVoice` (which checks) creates the state `update_camera` can touch.
#[tokio::test]
async fn camera_outside_voice_is_ignored() {
    let (url, _handle) = spawn_gateway(local_signing()).await;
    let owner_id = BotIdentity::generate();
    let outsider_id = BotIdentity::generate();
    let mut owner = connect_user(&url, &owner_id, "owner").await;
    let (_guild_id, _channel_id) = voice_channel(&mut owner).await;

    // Deliberately never joins the guild or voice.
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

/// The dedupe in `update_camera`. Without it every redundant click fans out to
/// every member of the guild, which is why no rate limiter is needed here.
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
