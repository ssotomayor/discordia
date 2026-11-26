#![cfg(feature = "activity")]

use discordia_bot::{
    Bot, BotIdentity,
    protocol::{ActivityRole, ChannelKind, ClientMessage, Id, ServerMessage},
};
use discordia_music::activity::{manifest, runtime};
use discordia_server::{ServerConfig, livekit::LiveKitConfig};
use serde_json::{Value, json};
use std::time::Duration;

async fn wait<T>(bot: &mut Bot, mut select: impl FnMut(ServerMessage) -> Option<T>) -> T {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let event = bot.next_event().await.expect("gateway connection closed");
            if let ServerMessage::Error { message } = &event {
                panic!("gateway: {message}");
            }
            if let Some(value) = select(event) {
                return value;
            }
        }
    })
    .await
    .expect("gateway event timed out")
}

async fn state(bot: &mut Bot, key: &str, select: impl Fn(&Value) -> bool) -> Value {
    wait(bot, |event| match event {
        ServerMessage::ActivityEvent { from, data, .. } if from == key => {
            let value: Value = serde_json::from_str(&data).unwrap();
            if value["t"] == "state" && select(&value) {
                Some(value)
            } else {
                None
            }
        }
        _ => None,
    })
    .await
}

async fn send(bot: &mut Bot, instance: Id, app: &str, data: Value) {
    bot.send(&ClientMessage::ActivitySend {
        instance_id: instance,
        to: Some(app.into()),
        data: data.to_string(),
    })
    .await
    .unwrap();
}

#[test]
fn explicit_install_delivers_bundle_and_runs_a_shared_queue() {
    let directory = tempfile::tempdir().unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let config = ServerConfig {
            livekit: LiveKitConfig::from_env(directory.path()),
            operators: Default::default(),
            data_dir: directory.path().into(),
            identities: discordia_server::local_identities(address.port()),
            launch: Default::default(),
            admission: Default::default(),
        };
        let (router, shutdown) = discordia_server::build_gateway(config).await.unwrap();
        let handle = discordia_server::serve_router(listener, router);
        let url = format!("ws://{address}");
        let owner_id = BotIdentity::generate();
        let mut owner = Bot::connect_as_user(&url, &owner_id, "Music test owner")
            .await
            .unwrap();
        wait(&mut owner, |event| {
            matches!(event, ServerMessage::Ready { .. }).then_some(())
        })
        .await;
        owner
            .send(&ClientMessage::CreateGuild {
                name: "Music install test".into(),
                template: None,
            })
            .await
            .unwrap();
        let (guild, channel) = wait(&mut owner, |event| match event {
            ServerMessage::GuildJoined {
                guild, channels, ..
            } => Some((
                guild.id,
                channels
                    .into_iter()
                    .find(|c| c.kind == ChannelKind::Text)
                    .unwrap()
                    .id,
            )),
            _ => None,
        })
        .await;

        let app = BotIdentity::generate();
        let app_key = app.pubkey().to_string();
        let mut bot = Bot::connect(&url, &app, "Music Lab").await.unwrap();
        wait(&mut bot, |event| {
            matches!(event, ServerMessage::Ready { .. }).then_some(())
        })
        .await;
        bot.register_manifest(&app, &manifest()).await.unwrap();
        let run_key = app_key.clone();
        let service = tokio::spawn(async move { runtime::run(bot, &run_key).await });

        owner
            .send(&ClientMessage::FetchGuildActivities { guild_id: guild })
            .await
            .unwrap();
        wait(&mut owner, |event| match event {
            ServerMessage::GuildActivities { activities, .. } => {
                assert!(activities.iter().all(|a| a.bot_pubkey != app_key));
                Some(())
            }
            _ => None,
        })
        .await;
        owner
            .send(&ClientMessage::InstallBot {
                guild_id: guild,
                bot_pubkey: app_key.clone(),
                name: String::new(),
                permissions: vec![],
                intents: vec![],
                manifest: Some(app.sign_manifest(&manifest())),
            })
            .await
            .unwrap();
        wait(&mut owner, |event| match event {
            ServerMessage::GuildActivities { activities, .. } => activities
                .iter()
                .any(|a| a.bot_pubkey == app_key)
                .then_some(()),
            _ => None,
        })
        .await;
        owner
            .send(&ClientMessage::ActivityStart {
                channel_id: channel,
                app: app_key.clone(),
            })
            .await
            .unwrap();
        let instance = wait(&mut owner, |event| match event {
            ServerMessage::ActivityStarted { instance } => Some(instance.id),
            _ => None,
        })
        .await;
        state(&mut owner, &app_key, |value| value["queue"] == json!([])).await;

        let guest_id = BotIdentity::generate();
        let mut guest = Bot::connect_as_user(&url, &guest_id, "Music test guest")
            .await
            .unwrap();
        wait(&mut guest, |event| {
            matches!(event, ServerMessage::Ready { .. }).then_some(())
        })
        .await;
        guest
            .send(&ClientMessage::JoinGuild {
                guild_id: guild,
                accept: true,
                pow_nonce: None,
            })
            .await
            .unwrap();
        wait(&mut guest, |event| {
            matches!(event, ServerMessage::GuildJoined { .. }).then_some(())
        })
        .await;
        guest
            .send(&ClientMessage::ActivityJoin {
                instance_id: instance,
                role: ActivityRole::Player,
            })
            .await
            .unwrap();
        state(&mut guest, &app_key, |value| value["queue"] == json!([])).await;
        let request = Id::new_v4();
        let add = json!({"t":"add", "request_id":request, "url":"https://tidal.com/track/123"});
        send(&mut guest, instance, &app_key, add.clone()).await;
        let received = state(&mut owner, &app_key, |value| {
            value["queue"][0]["metadata"] == "unsupported_probe"
        })
        .await;
        assert_eq!(received["queue"][0]["added_by"], guest_id.pubkey());
        assert_eq!(received["playback"], "not_connected");
        send(&mut guest, instance, &app_key, add).await;
        let repeated = state(&mut guest, &app_key, |value| {
            value["ack"] == request.to_string()
        })
        .await;
        assert_eq!(repeated["queue"].as_array().unwrap().len(), 1);

        owner
            .send(&ClientMessage::ActivityLeave {
                instance_id: instance,
            })
            .await
            .unwrap();
        let transferred = state(&mut guest, &app_key, |value| {
            value["host"] == guest_id.pubkey()
        })
        .await;
        assert_eq!(transferred["queue"].as_array().unwrap().len(), 1);
        service.abort();
        assert!(service.await.unwrap_err().is_cancelled());
        let restart_url = url.clone();
        let service = tokio::spawn(async move { runtime::connect(&restart_url, &app).await });
        let restarted = state(&mut guest, &app_key, |value| {
            value["session_epoch"] != transferred["session_epoch"]
        })
        .await;
        assert_eq!(restarted["queue"], json!([]));
        assert_eq!(restarted["host"], guest_id.pubkey());
        send(
            &mut guest,
            instance,
            &app_key,
            json!({"t":"clear", "request_id":Id::new_v4()}),
        )
        .await;
        state(&mut guest, &app_key, |value| value["queue"] == json!([])).await;

        shutdown.close_all();
        tokio::time::timeout(Duration::from_secs(5), service)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        drop(owner);
        drop(guest);
        handle.abort();
        drop(shutdown);
    });
    drop(rt);
    // SQLite's connection workers finish closing just after the runtime drops on Windows.
    let path = directory.keep();
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        match std::fs::remove_dir_all(&path) {
            Ok(()) => break,
            Err(error) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25));
                assert!(path.exists(), "unexpected cleanup error: {error}");
            }
            Err(error) => panic!("test directory cleanup failed: {error}"),
        }
    }
}
