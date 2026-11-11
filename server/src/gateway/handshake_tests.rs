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

type Client = (
    UnboundedSender<WsMessage>,
    UnboundedReceiver<WsMessage>,
    tokio::task::JoinHandle<()>,
    String,
);

async fn context() -> Arc<AppContext> {
    let dir = std::env::temp_dir().join(format!("discordia-handshake-{}", Id::new_v4()));
    let store = crate::store::Store::open_in(&dir).await.unwrap();
    let media =
        crate::media::MediaStore::open(dir.join("media"), crate::media::DEFAULT_MAX_BYTES).unwrap();
    let state = crate::state::AppState::load_or_seed(store, media, Default::default())
        .await
        .unwrap();
    Arc::new(AppContext {
        state: Arc::new(state),
        livekit: crate::livekit::LiveKitConfig::from_env(&dir),
        identities: ["localhost:9000".into()].into_iter().collect(),
        shutdown: Arc::new(tokio::sync::watch::Sender::new(false)),
        admission: Default::default(),
        tunnels_by_key: crate::admission::Budget::new(8, 128),
        tunnels_by_origin: crate::admission::Budget::new(16, 128),
    })
}

async fn open(ctx: &Arc<AppContext>) -> Client {
    let (incoming, rx) = unbounded_channel();
    let (tx, mut outgoing) = unbounded_channel();
    let task = tokio::spawn(handle_connection(
        Socket {
            incoming: rx,
            outgoing: tx,
        },
        ctx.clone(),
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

async fn gateway() -> (
    UnboundedSender<WsMessage>,
    UnboundedReceiver<WsMessage>,
    tokio::task::JoinHandle<()>,
    String,
    Arc<crate::state::AppState>,
) {
    let ctx = context().await;
    tokio::time::pause();
    let (incoming, outgoing, task, nonce) = open(&ctx).await;
    (incoming, outgoing, task, nonce, ctx.state.clone())
}

#[tokio::test]
async fn handshake_deadline_closes_an_unidentified_socket_without_waiting_ten_seconds() {
    let (incoming, mut outgoing, task, _, _) = gateway().await;
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
    let (incoming, mut outgoing, task, nonce, _) = gateway().await;
    tokio::time::resume();
    let identity = discordia_bot::BotIdentity::generate();
    let frame = ClientMessage::Identify {
        username: "member".into(),
        pubkey: identity.pubkey().into(),
        signature: identity.sign_identify(&nonce, "localhost:9000", "member"),
        origin: "localhost:9000".into(),
        bot: false,
        client_version: String::new(),
        progressive_image_uploads: false,
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

fn identify_frame(identity: &discordia_bot::BotIdentity, nonce: &str) -> WsMessage {
    let frame = ClientMessage::Identify {
        username: "member".into(),
        pubkey: identity.pubkey().into(),
        signature: identity.sign_identify(nonce, "localhost:9000", "member"),
        origin: "localhost:9000".into(),
        bot: false,
        client_version: String::new(),
        progressive_image_uploads: false,
    };
    WsMessage::Text(serde_json::to_string(&frame).unwrap())
}

/// The Ready send fails, so the login stops after `identify_conn` and after the
/// snapshot marked the key online: both must be undone like any disconnect.
#[tokio::test]
async fn a_login_that_fails_before_ready_gives_its_session_back() {
    let (incoming, outgoing, task, nonce, state) = gateway().await;
    tokio::time::resume();
    let identity = discordia_bot::BotIdentity::generate();
    let pubkey = identity.pubkey().to_string();
    drop(outgoing);
    incoming.send(identify_frame(&identity, &nonce)).unwrap();
    task.await.unwrap();

    assert!(
        state
            .members
            .iter()
            .any(|guild| guild.contains_key(&pubkey)),
        "the snapshot joined the lobby, so there is an online flag to clear"
    );
    assert!(
        !state.has_sessions(&pubkey),
        "the key still holds the dead socket"
    );
    assert!(!state.connected_pubkeys().contains(&pubkey));
    assert!(
        state
            .members
            .iter()
            .all(|guild| guild.get(&pubkey).is_none_or(|m| !m.online)),
        "nobody is left online by a login that never finished"
    );
}

async fn next_text(outgoing: &mut UnboundedReceiver<WsMessage>) -> ServerMessage {
    let text = async {
        loop {
            if let WsMessage::Text(text) = outgoing.recv().await.expect("socket closed") {
                return serde_json::from_str(&text).unwrap();
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(5), text)
        .await
        .expect("no answer")
}

async fn until_ready(outgoing: &mut UnboundedReceiver<WsMessage>) {
    while !matches!(next_text(outgoing).await, ServerMessage::Ready { .. }) {}
}

/// A second login for a key that is already online breaks off: the live session
/// keeps its slot and its online flag, and only the dead socket is let go.
#[tokio::test]
async fn a_failed_second_login_leaves_the_live_session_alone() {
    let ctx = context().await;
    let identity = discordia_bot::BotIdentity::generate();
    let pubkey = identity.pubkey().to_string();
    let (live_in, mut live_out, live_task, nonce) = open(&ctx).await;
    live_in.send(identify_frame(&identity, &nonce)).unwrap();
    until_ready(&mut live_out).await;

    let (incoming, outgoing, task, nonce) = open(&ctx).await;
    drop(outgoing);
    incoming.send(identify_frame(&identity, &nonce)).unwrap();
    task.await.unwrap();

    assert_eq!(ctx.state.connected_pubkeys(), vec![pubkey.clone()]);
    assert!(ctx.state.has_sessions(&pubkey));
    assert!(
        ctx.state
            .members
            .iter()
            .any(|guild| guild.get(&pubkey).is_some_and(|m| m.online))
    );
    live_in.send(WsMessage::Close(None)).unwrap();
    live_task.await.unwrap();
    assert!(!ctx.state.has_sessions(&pubkey));
}

/// One gate answers everything sent before Identify alike, including frames
/// whose handlers once let them through or dropped them without a word.
#[tokio::test]
async fn every_frame_before_identify_gets_the_same_refusal() {
    let (incoming, mut outgoing, task, nonce, _) = gateway().await;
    tokio::time::resume();
    let early = [
        ClientMessage::FetchCatalog {
            offset: 0,
            limit: 0,
        },
        ClientMessage::SetAttention { attended: false },
        ClientMessage::LeaveVoice,
        ClientMessage::Typing {
            channel_id: Id::new_v4(),
        },
        ClientMessage::FetchMessages {
            channel_id: Id::new_v4(),
            limit: 10,
            before_ms: None,
        },
    ];
    for frame in early {
        incoming
            .send(WsMessage::Text(serde_json::to_string(&frame).unwrap()))
            .unwrap();
        assert!(
            matches!(next_text(&mut outgoing).await, ServerMessage::Error { message } if message == "identify first"),
            "{frame:?} was not refused"
        );
    }
    let identity = discordia_bot::BotIdentity::generate();
    incoming.send(identify_frame(&identity, &nonce)).unwrap();
    until_ready(&mut outgoing).await;
    incoming.send(WsMessage::Close(None)).unwrap();
    task.await.unwrap();
}
