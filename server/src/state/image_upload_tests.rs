use super::*;
use tokio::sync::mpsc::Receiver;

const PREVIEW: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

struct Fixture {
    state: AppState,
    guild_id: Id,
    channel_id: Id,
    sender: User,
    upload_id: Id,
    sender_conn: u64,
    sender_rx: Receiver<ServerMessage>,
    /// Another member, with one session that took the provisional message and
    /// one that never negotiated progressive uploads.
    peer_rx: Receiver<ServerMessage>,
    legacy_rx: Receiver<ServerMessage>,
}

fn session(state: &AppState, pubkey: &str, progressive: bool) -> (u64, Receiver<ServerMessage>) {
    let (id, rx) = state.register_conn(None).unwrap();
    state.identify_conn(id, pubkey).unwrap();
    state.set_progressive_image_uploads(id, progressive);
    (id, rx)
}

fn drain(rx: &mut Receiver<ServerMessage>) -> Vec<ServerMessage> {
    std::iter::from_fn(|| rx.try_recv().ok()).collect()
}

/// A pending upload whose provisional message reached both progressive sessions.
async fn pending() -> Fixture {
    let dir = std::env::temp_dir().join(format!("discordia-upload-{}", Id::new_v4()));
    let store = crate::store::Store::open_in(&dir).await.unwrap();
    let media =
        crate::media::MediaStore::open(dir.join("media"), crate::media::DEFAULT_MAX_BYTES).unwrap();
    let state = AppState::load_or_seed(store, media, Default::default())
        .await
        .unwrap();
    let sender = User {
        pubkey: "a".repeat(64),
        username: "sender".into(),
    };
    let peer = User {
        pubkey: "b".repeat(64),
        username: "peer".into(),
    };
    let (guild, channels, _, _) = state.create_guild("Pictures", None, &sender).await.unwrap();
    state.join_guild(guild.id, &peer).await.unwrap();
    let channel_id = channels
        .iter()
        .find(|c| c.kind == crate::protocol::ChannelKind::Text)
        .unwrap()
        .id;
    let (sender_conn, mut sender_rx) = session(&state, &sender.pubkey, true);
    let (_, mut peer_rx) = session(&state, &peer.pubkey, true);
    let (_, mut legacy_rx) = session(&state, &peer.pubkey, false);
    let upload_id = Id::new_v4();
    let provisional = state
        .begin_image_upload(BeginImageUploadInput {
            conn_id: sender_conn,
            author: sender.clone(),
            upload_id,
            channel_id,
            content: "photo".into(),
            preview: PREVIEW.into(),
            image_name: None,
            reply_to: None,
        })
        .await
        .unwrap();
    state.deliver_progressive_images(
        state.guild_member_pubkeys(guild.id),
        ServerMessage::MessageCreate(provisional),
    );
    for rx in [&mut sender_rx, &mut peer_rx, &mut legacy_rx] {
        drain(rx);
    }
    Fixture {
        state,
        guild_id: guild.id,
        channel_id,
        sender,
        upload_id,
        sender_conn,
        sender_rx,
        peer_rx,
        legacy_rx,
    }
}

#[derive(Debug, Clone, Copy)]
enum Exit {
    Cancel(UploadCancel),
    /// The sender left the guild after the provisional message went out.
    CancelAfterLeaving,
    CancelAfterChannelDeleted,
    CompleteWithBrokenImage,
    Complete,
}

/// (deletes, final creates) seen by the sender, the peer's progressive
/// session and the peer's legacy session.
type Seen = [(usize, usize); 3];

async fn leave_pending(exit: Exit) -> (Fixture, Seen) {
    let mut f = pending().await;
    let state = &f.state;
    match exit {
        Exit::Cancel(why) => {
            assert!(state.cancel_image_upload(f.sender_conn, f.upload_id, why));
        }
        Exit::CancelAfterLeaving => {
            if let Some(members) = state.members.get(&f.guild_id) {
                members.remove(&f.sender.pubkey);
            }
            assert!(state.cancel_image_upload(f.sender_conn, f.upload_id, UploadCancel::NotMember));
        }
        Exit::CancelAfterChannelDeleted => {
            state.channels.remove(&f.channel_id);
            assert!(state.cancel_image_upload(
                f.sender_conn,
                f.upload_id,
                UploadCancel::ChannelGone
            ));
        }
        Exit::CompleteWithBrokenImage => {
            let broken = "data:image/png;base64,AAAA".to_string();
            let completed = state
                .complete_image_upload(f.sender_conn, &f.sender.pubkey, f.upload_id, broken)
                .await;
            assert!(completed.is_err());
        }
        Exit::Complete => {
            state
                .complete_image_upload(f.sender_conn, &f.sender.pubkey, f.upload_id, PREVIEW.into())
                .await
                .unwrap();
        }
    }
    let upload_id = f.upload_id;
    let count = |seen: Vec<ServerMessage>| {
        let deletes = seen
            .iter()
            .filter(|m| matches!(m, ServerMessage::MessageDelete { message_id, .. } if *message_id == upload_id))
            .count();
        let creates = seen
            .iter()
            .filter(
                |m| matches!(m, ServerMessage::MessageCreate(message) if message.id == upload_id),
            )
            .count();
        (deletes, creates)
    };
    let seen = [
        count(drain(&mut f.sender_rx)),
        count(drain(&mut f.peer_rx)),
        count(drain(&mut f.legacy_rx)),
    ];
    (f, seen)
}

#[tokio::test]
async fn every_way_out_of_pending_tells_each_session_once() {
    use UploadCancel::*;
    let taken_back: Seen = [(1, 0), (1, 0), (0, 0)];
    let table: [(Exit, Seen); 12] = [
        (Exit::Cancel(RateLimited), taken_back),
        (Exit::Cancel(InvalidImage), taken_back),
        (Exit::Cancel(NotMember), taken_back),
        (Exit::Cancel(ReadOnly), taken_back),
        // Once to the sender: it is a member, and used to get a second copy.
        (Exit::Cancel(Slowmode), taken_back),
        (Exit::Cancel(Failed), taken_back),
        (Exit::Cancel(SocketClosed), taken_back),
        (Exit::Cancel(ChannelGone), taken_back),
        (Exit::CancelAfterLeaving, taken_back),
        // The peers were told the channel went; only the sender has no guild to hear from.
        (Exit::CancelAfterChannelDeleted, [(1, 0), (0, 0), (0, 0)]),
        (Exit::CompleteWithBrokenImage, taken_back),
        (Exit::Complete, [(0, 1), (0, 1), (0, 1)]),
    ];
    for (exit, expected) in table {
        let (f, seen) = leave_pending(exit).await;
        assert_eq!(seen, expected, "{exit:?}");
        assert_eq!(
            f.state
                .pending_image_upload_channel(f.sender_conn, f.upload_id),
            None,
            "{exit:?} left the upload pending"
        );
    }
}

#[tokio::test]
async fn an_upload_that_already_left_pending_sends_nothing_more() {
    let (mut f, _) = leave_pending(Exit::Cancel(UploadCancel::RateLimited)).await;
    assert!(
        !f.state
            .cancel_image_upload(f.sender_conn, f.upload_id, UploadCancel::Failed)
    );
    let late = f
        .state
        .complete_image_upload(f.sender_conn, &f.sender.pubkey, f.upload_id, PREVIEW.into())
        .await;
    assert_eq!(late.unwrap_err(), "image upload is no longer pending");
    f.state.cancel_image_uploads_for_connection(f.sender_conn);
    for rx in [&mut f.sender_rx, &mut f.peer_rx, &mut f.legacy_rx] {
        assert!(drain(rx).is_empty());
    }
}

#[tokio::test]
async fn a_closing_socket_takes_back_only_its_own_uploads() {
    let mut f = pending().await;
    let (other_conn, _other_rx) = session(&f.state, &f.sender.pubkey, true);
    let other_upload = Id::new_v4();
    f.state
        .begin_image_upload(BeginImageUploadInput {
            conn_id: other_conn,
            author: f.sender.clone(),
            upload_id: other_upload,
            channel_id: f.channel_id,
            content: String::new(),
            preview: PREVIEW.into(),
            image_name: None,
            reply_to: None,
        })
        .await
        .unwrap();
    f.state.cancel_image_uploads_for_connection(f.sender_conn);
    assert_eq!(
        f.state
            .pending_image_upload_channel(other_conn, other_upload),
        Some(f.channel_id)
    );
    let deleted: Vec<Id> = drain(&mut f.peer_rx)
        .into_iter()
        .filter_map(|m| match m {
            ServerMessage::MessageDelete { message_id, .. } => Some(message_id),
            _ => None,
        })
        .collect();
    assert_eq!(deleted, vec![f.upload_id]);
}
