use discordia_bot::BotIdentity;
use discordia_server::livekit::LiveKitConfig;
use discordia_server::protocol::{ClientMessage, Id, Member, ServerMessage};
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message as WsMessage;
type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(url: &str, identity: &BotIdentity, mobile: bool) -> (Socket, Vec<Member>) {
    let (mut socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    let nonce = loop {
        if let Some(Ok(WsMessage::Text(text))) = socket.next().await
            && let ServerMessage::Hello { nonce } = serde_json::from_str(&text).unwrap()
        {
            break nonce;
        }
    };
    let origin = discordia_server::protocol::dial_origin(url).unwrap();
    let identify = ClientMessage::Identify {
        username: "image-test".into(),
        pubkey: identity.pubkey().into(),
        signature: identity.sign_identify(&nonce, &origin, "image-test"),
        origin,
        bot: false,
        client_version: "test".into(),
        progressive_image_uploads: false,
        mobile,
    };
    socket
        .send(WsMessage::Text(serde_json::to_string(&identify).unwrap()))
        .await
        .unwrap();
    loop {
        if let ServerMessage::Ready { members, .. } = next(&mut socket).await {
            return (socket, members);
        }
    }
}

async fn next(socket: &mut Socket) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(Ok(WsMessage::Text(text))) = socket.next().await
                && let Ok(message) = serde_json::from_str(&text)
            {
                return message;
            }
        }
    })
    .await
    .expect("timed out waiting for a server message")
}

async fn send(socket: &mut Socket, message: ClientMessage) {
    socket
        .send(WsMessage::Text(serde_json::to_string(&message).unwrap()))
        .await
        .unwrap();
}

async fn presence(socket: &mut Socket, key: &str, guild_id: Id, mobile: bool) {
    loop {
        match next(socket).await {
            ServerMessage::MemberJoin(m) | ServerMessage::MemberUpdate(m)
                if m.user.pubkey == key && m.guild_id == guild_id =>
            {
                assert!(m.online);
                if m.mobile == mobile {
                    return;
                }
            }
            ServerMessage::MemberLeave {
                user_pubkey,
                guild_id: left,
            } if user_pubkey == key && left == guild_id => {
                panic!("another session is still online")
            }
            ServerMessage::Error { message } => panic!("{message}"),
            _ => {}
        }
    }
}

#[tokio::test]
async fn mobile_presence_follows_all_live_sessions_and_survives_member_updates() {
    let dir = std::env::temp_dir().join(format!("discordia-mobile-{}", Id::new_v4()));
    let cfg = discordia_server::ServerConfig {
        livekit: LiveKitConfig::from_env(&dir),
        operators: Default::default(),
        identities: Default::default(),
        launch: Default::default(),
        data_dir: dir,
        admission: Default::default(),
    };
    let handle = discordia_server::spawn("127.0.0.1:0".parse().unwrap(), 100, cfg)
        .await
        .unwrap();
    let url = format!("ws://{}/gateway", handle.addr);
    let person = BotIdentity::generate();
    let observer = BotIdentity::generate();
    let (mut desktop, _) = connect(&url, &person, false).await;
    send(
        &mut desktop,
        ClientMessage::CreateGuild {
            name: "Devices".into(),
            template: None,
        },
    )
    .await;
    let guild_id = loop {
        if let ServerMessage::GuildJoined { guild, members, .. } = next(&mut desktop).await {
            assert!(
                !members
                    .iter()
                    .find(|m| m.user.pubkey == person.pubkey())
                    .unwrap()
                    .mobile
            );
            break guild.id;
        }
    };
    let (mut watcher, _) = connect(&url, &observer, false).await;
    send(
        &mut watcher,
        ClientMessage::JoinGuild {
            guild_id,
            accept: false,
            pow_nonce: None,
        },
    )
    .await;
    loop {
        if matches!(next(&mut watcher).await, ServerMessage::GuildJoined { .. }) {
            break;
        }
    }
    let (mut mobile, ready) = connect(&url, &person, true).await;
    assert!(
        ready
            .iter()
            .find(|m| m.guild_id == guild_id && m.user.pubkey == person.pubkey())
            .unwrap()
            .mobile
    );
    presence(&mut watcher, person.pubkey(), guild_id, true).await;
    send(
        &mut desktop,
        ClientMessage::UpdateUsername {
            username: "renamed".into(),
        },
    )
    .await;
    loop {
        if let ServerMessage::MemberUpdate(m) = next(&mut watcher).await
            && m.guild_id == guild_id
            && m.user.pubkey == person.pubkey()
            && m.user.username == "renamed"
        {
            assert!(m.mobile, "member updates must preserve the mobile session");
            break;
        }
    }
    let (mut second_mobile, _) = connect(&url, &person, true).await;
    presence(&mut watcher, person.pubkey(), guild_id, true).await;
    mobile.close(None).await.unwrap();
    presence(&mut watcher, person.pubkey(), guild_id, true).await;
    second_mobile.close(None).await.unwrap();
    presence(&mut watcher, person.pubkey(), guild_id, false).await;

    let (mut mobile, _) = connect(&url, &person, true).await;
    presence(&mut watcher, person.pubkey(), guild_id, true).await;
    desktop.close(None).await.unwrap();
    presence(&mut watcher, person.pubkey(), guild_id, true).await;
    let (mut reconnected, ready) = connect(&url, &observer, false).await;
    assert!(
        ready
            .iter()
            .find(|m| m.guild_id == guild_id && m.user.pubkey == person.pubkey())
            .unwrap()
            .mobile
    );
    mobile.close(None).await.unwrap();
    loop {
        if let ServerMessage::MemberLeave {
            user_pubkey,
            guild_id: left,
        } = next(&mut watcher).await
            && user_pubkey == person.pubkey()
            && left == guild_id
        {
            break;
        }
    }
    let (mut desktop, ready) = connect(&url, &person, false).await;
    assert!(
        !ready
            .iter()
            .find(|m| m.guild_id == guild_id && m.user.pubkey == person.pubkey())
            .unwrap()
            .mobile
    );
    presence(&mut watcher, person.pubkey(), guild_id, false).await;
    desktop.close(None).await.unwrap();
    reconnected.close(None).await.unwrap();
    watcher.close(None).await.unwrap();
    handle.abort();
}

#[tokio::test]
async fn joining_and_creating_a_guild_from_mobile_advertises_the_phone() {
    let dir = std::env::temp_dir().join(format!("discordia-mobile-{}", Id::new_v4()));
    let cfg = discordia_server::ServerConfig {
        livekit: LiveKitConfig::from_env(&dir),
        operators: Default::default(),
        identities: Default::default(),
        launch: Default::default(),
        data_dir: dir,
        admission: Default::default(),
    };
    let handle = discordia_server::spawn("127.0.0.1:0".parse().unwrap(), 100, cfg)
        .await
        .unwrap();
    let url = format!("ws://{}/gateway", handle.addr);
    let owner = BotIdentity::generate();
    let guest = BotIdentity::generate();
    let (mut mobile, _) = connect(&url, &owner, true).await;
    send(
        &mut mobile,
        ClientMessage::CreateGuild {
            name: "Mobile guild".into(),
            template: None,
        },
    )
    .await;
    let guild_id = loop {
        if let ServerMessage::GuildJoined { guild, members, .. } = next(&mut mobile).await {
            assert!(
                members
                    .iter()
                    .find(|m| m.user.pubkey == owner.pubkey())
                    .unwrap()
                    .mobile
            );
            break guild.id;
        }
    };
    let (mut joining, _) = connect(&url, &guest, true).await;
    send(
        &mut joining,
        ClientMessage::JoinGuild {
            guild_id,
            accept: false,
            pow_nonce: None,
        },
    )
    .await;
    loop {
        if let ServerMessage::GuildJoined { members, .. } = next(&mut joining).await {
            assert!(
                members
                    .iter()
                    .filter(|m| m.guild_id == guild_id)
                    .all(|m| m.mobile)
            );
            break;
        }
    }
    presence(&mut mobile, guest.pubkey(), guild_id, true).await;
    joining.close(None).await.unwrap();
    mobile.close(None).await.unwrap();
    handle.abort();
}
