//! Two live peers against a real SFU — the half `server/tests/voice.rs` cannot
//! reach.
//!
//! That suite proves what the gateway *hands out*: identities, grants, and the
//! frames sent when a mint fails. It stops there on purpose, because whether an
//! SFU then honours `can_publish: false` is a property of LiveKit, not of our
//! code. `7b5b6bb` removed publish rights from the subscribe-only `#audio`
//! identity and the review of #42 called that the branch's real regression
//! risk: if the SFU treats the grant as a reason to reject the *connection*, or
//! to stop delivering subscribed audio, stream sound dies and nothing in CI
//! would say so.
//!
//! `#[ignore]`d, because it needs a LiveKit server listening. Run it against
//! the bundled one:
//!
//! ```text
//! cargo run --release -p dioxusfun-server          # spawns the SFU on :7880
//! cargo test -p dioxusfun --test live_sfu -- --ignored --nocapture
//! ```
//!
//! `LIVEKIT_URL`, `LIVEKIT_API_KEY` and `LIVEKIT_API_SECRET` override the
//! defaults, which match what the bundled server uses.

use std::time::Duration;

use livekit::options::TrackPublishOptions;
use livekit::prelude::*;
use livekit::track::{LocalAudioTrack, LocalTrack, TrackSource};
use livekit::webrtc::audio_source::native::NativeAudioSource;
use livekit::webrtc::prelude::{AudioSourceOptions, RtcAudioSource};
use livekit_api::access_token::{AccessToken, VideoGrants};

const SAMPLE_RATE: u32 = 48_000;
const CHANNELS: u32 = 1;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.into())
}

/// Mint with the same grant shape `server::livekit::mint_screen_token` uses, so
/// what is under test is the SFU's reading of those grants.
fn token(identity: &str, room: &str, can_publish: bool) -> String {
    AccessToken::with_api_key(
        &env_or("LIVEKIT_API_KEY", "devkey"),
        &env_or(
            "LIVEKIT_API_SECRET",
            "secret-must-be-at-least-32-chars-long",
        ),
    )
    .with_identity(identity)
    .with_name(identity)
    .with_grants(VideoGrants {
        room_join: true,
        room: room.to_string(),
        can_publish,
        can_subscribe: true,
        can_publish_data: can_publish,
        ..Default::default()
    })
    .to_jwt()
    .expect("mint")
}

async fn publish_silence(room: &Room, source: TrackSource) -> Result<(), String> {
    let src = NativeAudioSource::new(AudioSourceOptions::default(), SAMPLE_RATE, CHANNELS, 1000);
    let track = LocalAudioTrack::create_audio_track("probe", RtcAudioSource::Native(src));
    room.local_participant()
        .publish_track(
            LocalTrack::Audio(track),
            TrackPublishOptions {
                source,
                ..Default::default()
            },
        )
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Wait for a room event matching `pred`, or give up.
async fn wait_for(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<RoomEvent>,
    label: &str,
    pred: impl Fn(&RoomEvent) -> bool,
) -> RoomEvent {
    let deadline = Duration::from_secs(15);
    tokio::time::timeout(deadline, async {
        loop {
            let ev = events.recv().await.expect("event stream closed");
            println!("[{label}] {ev:?}");
            if pred(&ev) {
                return ev;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("[{label}] timed out after {deadline:?}"))
}

/// The question the unit and integration tests cannot answer: with publish
/// revoked, does the subscribe-only identity still get in, and still *hear*?
///
/// Both halves matter and they fail differently. A rejected connection would be
/// obvious. Audio that stops arriving would look exactly like the bug this
/// grant change was meant to have nothing to do with.
#[tokio::test]
#[ignore = "needs a running LiveKit server; see the module docs"]
async fn subscribe_only_identity_still_receives_audio() {
    let url = env_or("LIVEKIT_URL", "ws://127.0.0.1:7880");
    let room_name = format!("screen-{}", uuid::Uuid::new_v4());
    let pubkey = "a".repeat(64);

    // The sharer: bare pubkey, publishes, exactly as the webview does.
    let (sharer, mut sharer_events) = Room::connect(
        &url,
        &token(&pubkey, &room_name, true),
        RoomOptions::default(),
    )
    .await
    .expect("sharer connects");

    // The listener: `{pubkey}#audio`, minted with can_publish false.
    let (listener, mut listener_events) = Room::connect(
        &url,
        &token(&format!("{pubkey}#audio"), &room_name, false),
        RoomOptions::default(),
    )
    .await
    .expect("the subscribe-only identity must still be allowed to connect");

    println!("both peers connected to {room_name}");

    // The sharer publishes what a screen share's audio would be.
    publish_silence(&sharer, TrackSource::Screenshare)
        .await
        .expect("sharer publishes");

    // The listener must be told about it and must end up subscribed. Losing
    // publish rights has to leave subscription untouched — that is the whole
    // premise of the change.
    wait_for(&mut listener_events, "listener", |ev| {
        matches!(ev, RoomEvent::TrackSubscribed { .. })
    })
    .await;

    // And the converse: with the grant revoked, the publish must not succeed.
    // If it did, `can_publish: false` would be decorative and the token would
    // not be the control it is documented to be.
    //
    // Observed against livekit-server 1.12.0, and worth knowing: the refusal is
    // a *timeout*, not an answer — "track publication timed out, no response
    // received from the server". The SFU drops the AddTrack request on the
    // floor rather than rejecting it. Enforcement is real, but any code that
    // ever tried to publish on this identity would hang for ten seconds instead
    // of failing fast, so the assertion is on the outcome and the message is
    // printed rather than matched. A future LiveKit that starts answering
    // properly should not fail this test.
    let refused = publish_silence(&listener, TrackSource::Microphone).await;
    assert!(
        refused.is_err(),
        "the SFU accepted a publish from an identity minted without publish rights"
    );
    println!(
        "publish did not succeed, as required: {}",
        refused.unwrap_err()
    );

    let _ = sharer_events.recv().await;
    listener.close().await.ok();
    sharer.close().await.ok();
}
