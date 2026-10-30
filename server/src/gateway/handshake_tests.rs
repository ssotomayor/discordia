use super::*;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

struct Socket {
    incoming: UnboundedReceiver<WsMessage>,
    outgoing: UnboundedSender<WsMessage>,
}

impl futures_util::Stream for Socket {
    type Item = Result<WsMessage, axum::Error>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.incoming.poll_recv(cx).map(|frame| frame.map(Ok))
    }
}

impl futures_util::Sink<WsMessage> for Socket {
    type Error = axum::Error;
    fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
    fn start_send(self: Pin<&mut Self>, frame: WsMessage) -> Result<(), Self::Error> {
        self.outgoing.send(frame).map_err(axum::Error::new)
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
}

async fn gateway() -> (
    UnboundedSender<WsMessage>,
    UnboundedReceiver<WsMessage>,
    tokio::task::JoinHandle<()>,
    String,
) {
    let dir = std::env::temp_dir().join(format!("discordia-handshake-{}", Id::new_v4()));
    let store = crate::store::Store::open_in(&dir).await.unwrap();
    let media =
        crate::media::MediaStore::open(dir.join("media"), crate::media::DEFAULT_MAX_BYTES).unwrap();
    let state = crate::state::AppState::load_or_seed(store, media, Default::default())
        .await
        .unwrap();
    let ctx = Arc::new(AppContext {
        state: Arc::new(state),
        livekit: crate::livekit::LiveKitConfig::from_env(&dir),
        identities: ["localhost:9000".into()].into_iter().collect(),
        shutdown: Arc::new(tokio::sync::watch::Sender::new(false)),
        admission: Default::default(),
        tunnels_by_key: crate::admission::Budget::new(8, 128),
        tunnels_by_origin: crate::admission::Budget::new(16, 128),
    });
    let (incoming, rx) = unbounded_channel();
    let (tx, mut outgoing) = unbounded_channel();
    tokio::time::pause();
    let task = tokio::spawn(handle_connection(
        Socket {
            incoming: rx,
            outgoing: tx,
        },
        ctx,
        None,
        None,
    ));
    let WsMessage::Text(hello) = outgoing.recv().await.unwrap() else {
        panic!("expected hello")
    };
    let ServerMessage::Hello { nonce } = serde_json::from_str(&hello).unwrap() else {
        panic!("expected nonce")
    };
    (incoming, outgoing, task, nonce)
}

#[tokio::test]
async fn handshake_deadline_closes_an_unidentified_socket_without_waiting_ten_seconds() {
    let (incoming, mut outgoing, task, _) = gateway().await;
    tokio::time::advance(IDENTIFY_TIMEOUT - Duration::from_millis(1)).await;
    incoming.send(WsMessage::Ping(vec![7])).unwrap();
    assert!(matches!(outgoing.recv().await, Some(WsMessage::Pong(_))));
    assert!(!task.is_finished());
    tokio::time::advance(Duration::from_millis(1)).await;
    let WsMessage::Text(error) = outgoing.recv().await.unwrap() else {
        panic!("expected error")
    };
    assert!(
        matches!(serde_json::from_str::<ServerMessage>(&error).unwrap(), ServerMessage::Error { message } if message == "identify timed out")
    );
    task.await.unwrap();
    assert!(outgoing.recv().await.is_none());
}

#[tokio::test]
async fn identified_socket_survives_the_handshake_deadline() {
    let (incoming, mut outgoing, task, nonce) = gateway().await;
    tokio::time::resume();
    let identity = discordia_bot::BotIdentity::generate();
    let frame = ClientMessage::Identify {
        username: "member".into(),
        pubkey: identity.pubkey().into(),
        signature: identity.sign_identify(&nonce, "localhost:9000", "member"),
        origin: "localhost:9000".into(),
        bot: false,
        client_version: String::new(),
    };
    incoming
        .send(WsMessage::Text(serde_json::to_string(&frame).unwrap()))
        .unwrap();
    loop {
        let Some(WsMessage::Text(text)) = outgoing.recv().await else {
            continue;
        };
        if matches!(
            serde_json::from_str::<ServerMessage>(&text).unwrap(),
            ServerMessage::Ready { .. }
        ) {
            break;
        }
    }
    tokio::time::pause();
    incoming.send(WsMessage::Pong(vec![])).unwrap();
    tokio::task::yield_now().await;
    tokio::time::advance(IDENTIFY_TIMEOUT + Duration::from_millis(1)).await;
    incoming.send(WsMessage::Ping(vec![8])).unwrap();
    loop {
        match outgoing.recv().await.unwrap() {
            WsMessage::Pong(_) => break,
            WsMessage::Text(text) => assert!(
                !matches!(serde_json::from_str::<ServerMessage>(&text).unwrap(), ServerMessage::Error { message } if message == "identify timed out")
            ),
            _ => {}
        }
    }
    assert!(!task.is_finished());
    incoming.send(WsMessage::Close(None)).unwrap();
    task.await.unwrap();
}
