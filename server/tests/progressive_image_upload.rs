use std::io::Cursor;
use std::net::SocketAddr;
use std::time::Duration;

use base64::Engine as _;
use discordia_bot::BotIdentity;
use discordia_server::livekit::LiveKitConfig;
use discordia_server::protocol::{ChannelKind, ClientMessage, Id, ServerMessage};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message as WsMessage;

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

const PREVIEW: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

async fn connect(url: &str, identity: &BotIdentity, progressive: bool) -> Socket {
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
        progressive_image_uploads: progressive,
    };
    socket
        .send(WsMessage::Text(serde_json::to_string(&identify).unwrap()))
        .await
        .unwrap();
    loop {
        if matches!(next(&mut socket).await, ServerMessage::Ready { .. }) {
            return socket;
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

fn original_image() -> String {
    let image = image::RgbImage::from_pixel(800, 600, image::Rgb([20, 40, 60]));
    let mut bytes = Cursor::new(Vec::new());
    image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes.into_inner())
    )
}

#[tokio::test]
async fn progressive_upload_updates_one_message_and_only_opted_in_sessions_see_preview() {
    let dir = std::env::temp_dir().join(format!("discordia-progressive-{}", Id::new_v4()));
    let cfg = discordia_server::ServerConfig {
        livekit: LiveKitConfig::from_env(&dir),
        operators: Default::default(),
        identities: Default::default(),
        launch: Default::default(),
        data_dir: dir,
        admission: Default::default(),
    };
    let handle = discordia_server::spawn("127.0.0.1:0".parse::<SocketAddr>().unwrap(), 100, cfg)
        .await
        .expect("server starts");
    let url = format!("ws://{}/gateway", handle.addr);
    let identity = BotIdentity::generate();
    let mut progressive = connect(&url, &identity, true).await;
    send(
        &mut progressive,
        ClientMessage::CreateGuild {
            name: "Images".into(),
            template: None,
        },
    )
    .await;
    let channel_id = loop {
        if let ServerMessage::GuildJoined { channels, .. } = next(&mut progressive).await {
            break channels
                .into_iter()
                .find(|channel| channel.kind == ChannelKind::Text)
                .expect("text channel exists")
                .id;
        }
    };
    let mut conventional = connect(&url, &identity, false).await;

    let upload_id = Id::new_v4();
    send(
        &mut progressive,
        ClientMessage::BeginImageUpload {
            upload_id,
            channel_id,
            content: "progressive image".into(),
            preview: PREVIEW.into(),
            image_name: Some("original.png".into()),
            reply_to: None,
        },
    )
    .await;
    let provisional = loop {
        if let ServerMessage::MessageCreate(message) = next(&mut progressive).await {
            break message;
        }
    };
    assert_eq!(provisional.id, upload_id);
    assert!(
        provisional
            .image_metadata
            .as_ref()
            .is_some_and(|metadata| metadata.upload_pending)
    );
    let provisional_for_conventional = tokio::time::timeout(Duration::from_millis(250), async {
        loop {
            if let ServerMessage::MessageCreate(message) = next(&mut conventional).await
                && message.id == upload_id
            {
                return message;
            }
        }
    })
    .await;
    assert!(
        provisional_for_conventional.is_err(),
        "a session without the capability must not receive the provisional message"
    );

    send(
        &mut progressive,
        ClientMessage::CompleteImageUpload {
            upload_id,
            image: original_image(),
        },
    )
    .await;
    let final_for_sender = loop {
        if let ServerMessage::MessageCreate(message) = next(&mut progressive).await {
            break message;
        }
    };
    let final_for_conventional = loop {
        if let ServerMessage::MessageCreate(message) = next(&mut conventional).await {
            break message;
        }
    };
    for final_message in [final_for_sender, final_for_conventional] {
        assert_eq!(final_message.id, upload_id);
        assert!(
            !final_message
                .image_metadata
                .as_ref()
                .is_some_and(|metadata| metadata.upload_pending)
        );
        assert_ne!(final_message.image.as_deref(), provisional.image.as_deref());
        assert_eq!(
            final_message
                .image_metadata
                .as_ref()
                .and_then(|metadata| metadata.preview.as_deref()),
            provisional.image.as_deref()
        );
    }
    drop(conventional);
    drop(progressive);
    handle.abort();
}
