//! Mention pushes end to end: who a guild message wakes through the relay, and
//! that the relay gets only ciphertext the registered key opens.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use discordia_bot::{Bot, BotIdentity};
use discordia_server::livekit::{BoxFuture, LiveKitConfig};
use discordia_server::protocol::push::{MentionPayload, PushReason};
use discordia_server::protocol::{ChannelKind, ClientMessage, Id, Message, ServerMessage};
use discordia_server::push::{Outcome, PushConfig, PushRelay};

const RELAY: &str = "https://push.test";

#[derive(Default)]
struct RecordingRelay {
    sent: Mutex<Vec<(String, String)>>,
    script: Mutex<VecDeque<Outcome>>,
}

impl PushRelay for RecordingRelay {
    fn push<'a>(&'a self, handle: &'a str, body: String) -> BoxFuture<'a, Outcome> {
        self.sent.lock().unwrap().push((handle.to_string(), body));
        let outcome = self
            .script
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Outcome::Accepted);
        Box::pin(async move { outcome })
    }
}

impl RecordingRelay {
    fn to(&self, handle: &str) -> Vec<String> {
        self.sent
            .lock()
            .unwrap()
            .iter()
            .filter(|(h, _)| h == handle)
            .map(|(_, body)| body.clone())
            .collect()
    }

    fn total(&self) -> usize {
        self.sent.lock().unwrap().len()
    }

    async fn wait_for(&self, handle: &str, n: usize) -> Vec<String> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let got = self.to(handle);
            if got.len() >= n {
                return got;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "waited for {n} pushes, got {}",
                got.len()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn answer(&self, outcomes: &[Outcome]) {
        self.script.lock().unwrap().extend(outcomes.iter().copied());
    }
}

async fn next_timeout(bot: &mut Bot) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(5), bot.next_event())
        .await
        .expect("timed out waiting for a gateway event")
        .expect("connection closed unexpectedly")
}

fn data_dir() -> PathBuf {
    std::env::temp_dir().join(format!("discordia-push-{}", Id::new_v4()))
}

async fn spawn_gateway(push: PushConfig, dir: PathBuf) -> (String, discordia_server::ServerHandle) {
    let cfg = discordia_server::ServerConfig {
        livekit: LiveKitConfig::from_env(&dir),
        operators: Default::default(),
        identities: Default::default(),
        launch: discordia_server::server_settings::LaunchSettings {
            push,
            ..Default::default()
        },
        data_dir: dir,
        admission: Default::default(),
    };
    let preferred: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let handle = discordia_server::spawn(preferred, 100, cfg)
        .await
        .expect("spawn server");
    (format!("ws://{}", handle.addr), handle)
}

async fn connect(url: &str, identity: &BotIdentity, name: &str) -> (Bot, ServerMessage) {
    let mut bot = Bot::connect_as_user(url, identity, name).await.unwrap();
    loop {
        let event = next_timeout(&mut bot).await;
        if matches!(event, ServerMessage::Ready { .. }) {
            return (bot, event);
        }
    }
}

struct Server {
    url: String,
    handle: discordia_server::ServerHandle,
    dir: PathBuf,
    relay: Arc<RecordingRelay>,
    owner: Bot,
    owner_key: BotIdentity,
    guild: Id,
    text: Id,
    text_name: String,
}

async fn server() -> Server {
    let relay = Arc::new(RecordingRelay::default());
    let dir = data_dir();
    let (url, handle) = spawn_gateway(
        PushConfig::with_relay(RELAY, relay.clone()).unwrap(),
        dir.clone(),
    )
    .await;
    let owner_key = BotIdentity::generate();
    let (mut owner, ready) = connect(&url, &owner_key, "owner").await;
    let ServerMessage::Ready { server, .. } = ready else {
        unreachable!()
    };
    assert_eq!(server.push.as_deref(), Some(RELAY));
    owner
        .send(&ClientMessage::CreateGuild {
            name: "Pushes".into(),
            template: None,
        })
        .await
        .unwrap();
    let (guild, text) = loop {
        if let ServerMessage::GuildJoined {
            guild, channels, ..
        } = next_timeout(&mut owner).await
        {
            break (
                guild.id,
                channels
                    .into_iter()
                    .find(|c| c.kind == ChannelKind::Text)
                    .unwrap(),
            );
        }
    };
    Server {
        url,
        handle,
        dir,
        relay,
        owner,
        owner_key,
        guild,
        text: text.id,
        text_name: text.name,
    }
}

impl Server {
    async fn member(&self, name: &str) -> (Bot, BotIdentity) {
        let identity = BotIdentity::generate();
        let (mut bot, _) = connect(&self.url, &identity, name).await;
        bot.send(&ClientMessage::JoinGuild {
            guild_id: self.guild,
            accept: true,
            pow_nonce: None,
        })
        .await
        .unwrap();
        while !matches!(
            next_timeout(&mut bot).await,
            ServerMessage::GuildJoined { .. }
        ) {}
        (bot, identity)
    }

    async fn post(&mut self, content: &str) -> Message {
        self.owner.send_message(self.text, content).await.unwrap();
        own_message(&mut self.owner, content).await
    }

    /// A member mentioned alongside every case: their push is the sign the
    /// worker has caught up, so a missing one is a real absence.
    async fn quiet_for(&mut self, handle: &str, content: &str, control: &str) {
        let before = self.relay.to(control).len();
        self.post(content).await;
        self.relay.wait_for(control, before + 1).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(self.relay.to(handle).is_empty(), "pushed for {content:?}");
    }
}

async fn own_message(bot: &mut Bot, content: &str) -> Message {
    loop {
        if let ServerMessage::MessageCreate(m) = next_timeout(bot).await
            && m.content == content
        {
            return m;
        }
    }
}

/// Frames on one socket are handled in order, so an answer to this says the
/// ones sent before it are done; an error before it is theirs.
async fn sync(bot: &mut Bot, channel_id: Id) -> Result<(), String> {
    bot.send(&ClientMessage::FetchMessages {
        channel_id,
        limit: 1,
        before_ms: None,
    })
    .await
    .unwrap();
    loop {
        match next_timeout(bot).await {
            ServerMessage::MessageHistory { .. } => return Ok(()),
            ServerMessage::Error { message } => {
                while !matches!(
                    next_timeout(bot).await,
                    ServerMessage::MessageHistory { .. }
                ) {}
                return Err(message);
            }
            _ => {}
        }
    }
}

struct Device {
    handle: String,
    key: [u8; 32],
}

fn device() -> Device {
    Device {
        handle: URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>()),
        key: rand::random(),
    }
}

async fn register(bot: &mut Bot, channel: Id, device: &Device) -> Result<(), String> {
    register_raw(bot, channel, &device.handle, &STANDARD.encode(device.key)).await
}

async fn register_raw(bot: &mut Bot, channel: Id, handle: &str, key: &str) -> Result<(), String> {
    bot.send(&ClientMessage::RegisterPush {
        handle: handle.into(),
        key: key.into(),
    })
    .await
    .unwrap();
    sync(bot, channel).await
}

async fn attention(bot: &mut Bot, channel: Id, attended: bool) {
    bot.send(&ClientMessage::SetAttention { attended })
        .await
        .unwrap();
    sync(bot, channel).await.unwrap();
}

/// Registered and looking elsewhere: pushed for every mention.
async fn away(server: &Server, name: &str) -> (Bot, Device) {
    let (mut bot, _) = server.member(name).await;
    let device = device();
    register(&mut bot, server.text, &device).await.unwrap();
    attention(&mut bot, server.text, false).await;
    (bot, device)
}

fn open(body: &str, key: &[u8; 32]) -> MentionPayload {
    let json = discordia_nostr::nip44::decrypt(key, body).expect("opens with the registered key");
    serde_json::from_str(&json).unwrap()
}

#[tokio::test]
async fn an_offline_member_is_pushed_once_and_only_the_key_opens_it() {
    let mut s = server().await;
    let (mut bob, bob_key) = s.member("bob").await;
    let phone = device();
    register(&mut bob, s.text, &phone).await.unwrap();
    drop(bob);
    // The abrupt-disconnect grace: once the others are told, no session is left.
    loop {
        if let ServerMessage::MemberLeave { user_pubkey, .. } = next_timeout(&mut s.owner).await
            && user_pubkey == bob_key.pubkey()
        {
            break;
        }
    }

    let sent = s.post("hey @bob, look").await;
    let bodies = s.relay.wait_for(&phone.handle, 1).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(s.relay.total(), 1, "exactly one push");
    assert!(!bodies[0].contains("bob") && !bodies[0].contains("look"));
    let payload = open(&bodies[0], &phone.key);
    assert_eq!(
        payload,
        MentionPayload {
            v: 1,
            k: PushReason::Mention,
            guild: s.guild,
            channel: s.text,
            message: sent.id,
            author: s.owner_key.pubkey().to_string(),
            author_name: "owner".into(),
            guild_name: "Pushes".into(),
            channel_name: s.text_name.clone(),
            at: sent.created_at.timestamp(),
        }
    );
    assert!(discordia_nostr::nip44::decrypt(&[0u8; 32], &bodies[0]).is_err());

    s.post("nothing for anyone").await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        s.relay.total(),
        1,
        "a message without a mention pushes nothing"
    );

    // Registrations are the server's, not the guild's: an export never carries one.
    let store = discordia_server::store::Store::open_in(&s.dir)
        .await
        .unwrap();
    let archive = store.export_guild(s.guild).await.unwrap().unwrap();
    let exported = serde_json::to_string(&archive).unwrap();
    assert!(!exported.contains(&phone.handle));
    assert!(!exported.contains(&STANDARD.encode(phone.key)));
    s.handle.abort();
}

#[tokio::test]
async fn someone_looking_is_not_pushed_and_a_client_in_the_tray_is() {
    let mut s = server().await;
    let (_carol, carol) = away(&s, "carol").await;
    let (mut bob, bob_key) = s.member("bob").await;
    let phone = device();
    register(&mut bob, s.text, &phone).await.unwrap();

    // Never said otherwise, like a client from before `SetAttention`.
    s.quiet_for(&phone.handle, "@bob @carol one", &carol.handle)
        .await;

    attention(&mut bob, s.text, false).await;
    s.post("@bob two").await;
    s.relay.wait_for(&phone.handle, 1).await;

    attention(&mut bob, s.text, true).await;
    let pushed = s.relay.to(&phone.handle).len();
    let carol_had = s.relay.to(&carol.handle).len();
    s.post("@bob @carol three").await;
    s.relay.wait_for(&carol.handle, carol_had + 1).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(s.relay.to(&phone.handle).len(), pushed);

    // One session someone is looking at is enough, whatever the others say.
    attention(&mut bob, s.text, false).await;
    let (_desk, _) = connect(&s.url, &bob_key, "bob").await;
    let carol_had = s.relay.to(&carol.handle).len();
    s.post("@bob @carol four").await;
    s.relay.wait_for(&carol.handle, carol_had + 1).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(s.relay.to(&phone.handle).len(), pushed);
    s.handle.abort();
}

#[tokio::test]
async fn dnd_the_author_a_bot_and_a_leaver_are_not_pushed() {
    let mut s = server().await;
    let (_carol, carol) = away(&s, "carol").await;

    let (mut dana, dana_phone) = away(&s, "dana").await;
    dana.send(&ClientMessage::SetProfile {
        avatar: None,
        banner: None,
        bio: None,
        status: Some("dnd".into()),
        custom_status: None,
        hide_guild_owner: false,
        hide_operator: false,
        updated_at: 1,
    })
    .await
    .unwrap();
    sync(&mut dana, s.text).await.unwrap();
    s.quiet_for(&dana_phone.handle, "@dana @carol", &carol.handle)
        .await;

    let own = device();
    register(&mut s.owner, s.text, &own).await.unwrap();
    attention(&mut s.owner, s.text, false).await;
    s.quiet_for(&own.handle, "@owner @carol", &carol.handle)
        .await;

    // Registered while a person with a guild of its own, then seated here as an app.
    let helper_key = BotIdentity::generate();
    let (mut helper, _) = connect(&s.url, &helper_key, "helper").await;
    helper
        .send(&ClientMessage::CreateGuild {
            name: "Elsewhere".into(),
            template: None,
        })
        .await
        .unwrap();
    let elsewhere = loop {
        if let ServerMessage::GuildJoined { channels, .. } = next_timeout(&mut helper).await {
            break channels
                .into_iter()
                .find(|c| c.kind == ChannelKind::Text)
                .unwrap()
                .id;
        }
    };
    let helper_phone = device();
    register(&mut helper, elsewhere, &helper_phone)
        .await
        .unwrap();
    attention(&mut helper, elsewhere, false).await;
    s.owner
        .send(&ClientMessage::InstallBot {
            guild_id: s.guild,
            bot_pubkey: helper_key.pubkey().to_string(),
            name: "helper".into(),
            permissions: vec![],
            intents: vec![],
            manifest: None,
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::GuildIntegrations { bots, .. } = next_timeout(&mut s.owner).await
            && bots.iter().any(|b| b.bot_pubkey == helper_key.pubkey())
        {
            break;
        }
    }
    s.quiet_for(&helper_phone.handle, "@helper @carol", &carol.handle)
        .await;

    let (mut erin, erin_phone) = away(&s, "erin").await;
    erin.send(&ClientMessage::LeaveGuild { guild_id: s.guild })
        .await
        .unwrap();
    while !matches!(
        next_timeout(&mut erin).await,
        ServerMessage::GuildDelete { .. }
    ) {}
    s.quiet_for(&erin_phone.handle, "@erin @carol", &carol.handle)
        .await;

    let (_frank, frank_phone) = away(&s, "frank").await;
    s.quiet_for(&frank_phone.handle, "frank and @carol", &carol.handle)
        .await;
    s.handle.abort();
}

#[tokio::test]
async fn a_file_and_a_finished_image_upload_push_too() {
    let mut s = server().await;
    let (_bob, phone) = away(&s, "bob").await;

    let request_id = Id::new_v4();
    s.owner
        .send(&ClientMessage::SendFile {
            request_id,
            channel_id: s.text,
            name: "notes.txt".into(),
            data_url: format!(
                "data:application/octet-stream;base64,{}",
                STANDARD.encode(b"notes")
            ),
            content: "@bob the notes".into(),
            reply_to: None,
        })
        .await
        .unwrap();
    own_message(&mut s.owner, "@bob the notes").await;
    let body = &s.relay.wait_for(&phone.handle, 1).await[0];
    assert_eq!(open(body, &phone.key).channel, s.text);

    let mut upload = progressive_session(&s.url, &s.owner_key).await;
    let upload_id = Id::new_v4();
    upload::send(
        &mut upload,
        ClientMessage::BeginImageUpload {
            upload_id,
            channel_id: s.text,
            content: "@bob a picture".into(),
            preview: upload::PREVIEW.into(),
            image_name: Some("p.png".into()),
            reply_to: None,
        },
    )
    .await;
    upload::message(&mut upload, upload_id).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        s.relay.to(&phone.handle).len(),
        1,
        "nothing is committed until the upload finishes"
    );
    upload::send(
        &mut upload,
        ClientMessage::CompleteImageUpload {
            upload_id,
            image: upload::PREVIEW.into(),
        },
    )
    .await;
    upload::message(&mut upload, upload_id).await;
    let bodies = s.relay.wait_for(&phone.handle, 2).await;
    assert_eq!(open(&bodies[1], &phone.key).message, upload_id);
    s.handle.abort();
}

/// A session that negotiated progressive uploads, which the bot SDK never does.
async fn progressive_session(url: &str, identity: &BotIdentity) -> upload::Socket {
    upload::connect(&format!("{url}/gateway"), identity).await
}

mod upload {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    pub type Socket = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    pub const PREVIEW: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

    pub async fn connect(url: &str, identity: &BotIdentity) -> Socket {
        let (mut socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        let nonce = loop {
            if let ServerMessage::Hello { nonce } = next(&mut socket).await {
                break nonce;
            }
        };
        let origin = discordia_server::protocol::dial_origin(url).unwrap();
        send(
            &mut socket,
            ClientMessage::Identify {
                username: "owner".into(),
                pubkey: identity.pubkey().into(),
                signature: identity.sign_identify(&nonce, &origin, "owner"),
                origin,
                bot: false,
                client_version: "test".into(),
                progressive_image_uploads: true,
            },
        )
        .await;
        while !matches!(next(&mut socket).await, ServerMessage::Ready { .. }) {}
        socket
    }

    pub async fn next(socket: &mut Socket) -> ServerMessage {
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

    pub async fn send(socket: &mut Socket, message: ClientMessage) {
        socket
            .send(WsMessage::Text(serde_json::to_string(&message).unwrap()))
            .await
            .unwrap();
    }

    pub async fn message(socket: &mut Socket, id: Id) -> Message {
        loop {
            match next(socket).await {
                ServerMessage::MessageCreate(m) if m.id == id => return m,
                ServerMessage::Error { message } => panic!("{message}"),
                _ => {}
            }
        }
    }
}

#[tokio::test]
async fn a_gone_handle_is_dropped_and_a_busy_relay_retried_once() {
    let mut s = server().await;
    let (_carol, carol) = away(&s, "carol").await;
    let (_bob, phone) = away(&s, "bob").await;

    s.relay.answer(&[Outcome::Retry, Outcome::Retry]);
    s.post("@bob busy").await;
    let tries = s.relay.wait_for(&phone.handle, 2).await;
    assert_eq!(tries[0], tries[1], "the retry sends the same body");
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        s.relay.to(&phone.handle).len(),
        2,
        "one retry, then dropped"
    );

    s.post("@bob still there").await;
    s.relay.wait_for(&phone.handle, 3).await;

    s.relay.answer(&[Outcome::Gone]);
    s.post("@bob gone").await;
    s.relay.wait_for(&phone.handle, 4).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    let carol_had = s.relay.to(&carol.handle).len();
    s.post("@bob @carol after").await;
    s.relay.wait_for(&carol.handle, carol_had + 1).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        s.relay.to(&phone.handle).len(),
        4,
        "the relay said the handle is gone"
    );
    s.handle.abort();
}

#[tokio::test]
async fn a_fifth_registration_evicts_the_oldest() {
    let mut s = server().await;
    let (mut bob, _) = s.member("bob").await;
    let phones: Vec<Device> = (0..5).map(|_| device()).collect();
    for phone in &phones {
        register(&mut bob, s.text, phone).await.unwrap();
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    attention(&mut bob, s.text, false).await;
    s.post("@bob everywhere").await;
    for phone in &phones[1..] {
        s.relay.wait_for(&phone.handle, 1).await;
    }
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(s.relay.to(&phones[0].handle).is_empty());
    assert_eq!(s.relay.total(), 4);
    s.handle.abort();
}

#[tokio::test]
async fn only_a_relay_handle_and_a_32_byte_key_register() {
    let mut s = server().await;
    let (mut bob, _) = s.member("bob").await;
    let good = device();
    let key = STANDARD.encode(good.key);
    let mut handles: Vec<String> = ["/", "?", "#", "..", "%"]
        .iter()
        .map(|bad| format!("{bad}{}", &good.handle[bad.len()..]))
        .collect();
    handles.push(format!("{}/x", &good.handle[..40]));
    handles.push("../../admin".into());
    handles.push(format!("{}%2F", &good.handle[..40]));
    handles.push(String::new());
    for handle in handles {
        assert!(
            register_raw(&mut bob, s.text, &handle, &key).await.is_err(),
            "{handle:?}"
        );
    }
    for key in [
        STANDARD.encode([1u8; 31]),
        "not base64".into(),
        String::new(),
    ] {
        assert!(
            register_raw(&mut bob, s.text, &good.handle, &key)
                .await
                .is_err(),
            "{key:?}"
        );
    }
    register(&mut bob, s.text, &good).await.unwrap();
    bob.send(&ClientMessage::UnregisterPush {
        handle: good.handle.clone(),
    })
    .await
    .unwrap();
    attention(&mut bob, s.text, false).await;
    s.post("@bob unregistered").await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(s.relay.total(), 0);
    s.handle.abort();
}

#[tokio::test]
async fn with_push_off_nothing_registers_and_no_relay_is_named() {
    let (url, handle) = spawn_gateway(PushConfig::off(), data_dir()).await;
    let (mut owner, ready) = connect(&url, &BotIdentity::generate(), "owner").await;
    let ServerMessage::Ready { server, .. } = ready else {
        unreachable!()
    };
    assert_eq!(server.push, None);
    owner
        .send(&ClientMessage::CreateGuild {
            name: "Quiet".into(),
            template: None,
        })
        .await
        .unwrap();
    let text = loop {
        if let ServerMessage::GuildJoined { channels, .. } = next_timeout(&mut owner).await {
            break channels
                .into_iter()
                .find(|c| c.kind == ChannelKind::Text)
                .unwrap()
                .id;
        }
    };
    let refused = register(&mut owner, text, &device()).await;
    assert!(refused.unwrap_err().contains("push"));
    handle.abort();
}
