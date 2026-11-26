#![cfg(not(feature = "activity-audio"))]

use discordia_bot::{Bot, BotIdentity};
use discordia_server::{
    ServerConfig,
    livekit::LiveKitConfig,
    protocol::{ActivityAudioRole, ClientMessage, Id, ServerMessage},
};
use std::time::Duration;

#[tokio::test]
async fn activity_audio_is_disabled_in_an_ordinary_host() {
    let dir = std::env::temp_dir().join(format!("discordia-audio-disabled-{}", Id::new_v4()));
    let config = ServerConfig {
        livekit: LiveKitConfig::from_env(&dir),
        operators: Default::default(),
        data_dir: dir,
        identities: Default::default(),
        launch: Default::default(),
        admission: Default::default(),
    };
    let handle = discordia_server::spawn("127.0.0.1:0".parse().unwrap(), 1, config)
        .await
        .unwrap();
    let identity = BotIdentity::generate();
    let mut client = Bot::connect_as_user(
        &format!("ws://{}", handle.addr),
        &identity,
        "Audio disabled test",
    )
    .await
    .unwrap();
    let request = Id::new_v4();
    client
        .send(&ClientMessage::RequestActivityAudio {
            instance_id: Id::new_v4(),
            request_id: request,
            epoch: "0".repeat(32),
            role: ActivityAudioRole::Listener,
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match client.next_event().await.unwrap() {
                ServerMessage::ActivityAudioError {
                    request_id,
                    message,
                    ..
                } if request_id == request => {
                    assert!(message.contains("not enabled"));
                    break;
                }
                ServerMessage::ActivityAudioToken { .. } => {
                    panic!("an ordinary host granted activity audio")
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    handle.abort();
}
