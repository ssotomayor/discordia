use base64::Engine as _;
use dioxusfun_server::media::{DEFAULT_MAX_BYTES, MediaStore};
use dioxusfun_server::protocol::{
    ClientMessage, FilePolicy, Id, MessageSearch, ServerMessage, User,
};
use dioxusfun_server::state::AppState;
use dioxusfun_server::store::Store;

async fn fixture() -> (AppState, User, Id, Id) {
    let dir = std::env::temp_dir().join(format!("discordia-chat-tools-{}", Id::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("store.sqlite")).await.unwrap();
    let media = MediaStore::open(dir.join("media"), DEFAULT_MAX_BYTES).unwrap();
    let state = AppState::load_or_seed(store, media, Default::default())
        .await
        .unwrap();
    let owner = User {
        pubkey: "11".repeat(32),
        username: "owner".into(),
    };
    let (guild, channels, _, _) = state.create_guild("test", None, &owner).await.unwrap();
    let channel = channels
        .iter()
        .find(|c| c.kind == dioxusfun_server::protocol::ChannelKind::Text)
        .unwrap()
        .id;
    (state, owner, guild.id, channel)
}

fn upload(channel_id: Id, name: &str, bytes: &[u8]) -> ClientMessage {
    ClientMessage::SendFile {
        request_id: Id::new_v4(),
        channel_id,
        name: name.into(),
        data_url: format!(
            "data:application/octet-stream;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        ),
        content: String::new(),
        reply_to: None,
    }
}

#[tokio::test]
async fn file_expiry_keeps_the_message_but_revokes_download_and_reclaims_disk() {
    let (state, owner, guild_id, channel_id) = fixture().await;
    state
        .handle_chat_tool(
            &owner,
            upload(channel_id, "program.exe", b"MZ never execute this"),
        )
        .await
        .unwrap();
    let mut message = state
        .store
        .history(channel_id, 10, None)
        .await
        .unwrap()
        .pop()
        .unwrap();
    let attachment = message.attachment.as_ref().unwrap();
    assert!(attachment.media.ends_with(".bin"));
    assert!(
        attachment.expires_ms.unwrap() > chrono::Utc::now().timestamp_millis() + 6 * 86_400_000
    );
    assert_eq!(
        state
            .store
            .file_policy(guild_id)
            .await
            .unwrap()
            .retention_days,
        7
    );
    assert!(state.file_available(&owner.pubkey, &attachment.media).await);
    let stranger = "22".repeat(32);
    assert!(!state.file_available(&stranger, &attachment.media).await);
    let media = attachment.media.clone();
    message.attachment.as_mut().unwrap().expires_ms = Some(0);
    state
        .store
        .delete_message(channel_id, message.id)
        .await
        .unwrap();
    state.store.insert_message(&message).await.unwrap();
    assert!(!state.file_available(&owner.pubkey, &media).await);
    state.sweep_files().await;
    assert!(state.media.inline(&media).is_none());
    assert_eq!(state.media.used_bytes(), 0);
    assert_eq!(
        state
            .store
            .history(channel_id, 10, None)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn shared_file_is_kept_until_its_last_live_attachment_expires() {
    let (state, owner, _, channel_id) = fixture().await;
    for name in ["a.txt", "b.txt"] {
        state
            .handle_chat_tool(&owner, upload(channel_id, name, b"same bytes"))
            .await
            .unwrap();
    }
    let mut messages = state.store.history(channel_id, 10, None).await.unwrap();
    let media = messages[0].attachment.as_ref().unwrap().media.clone();
    messages[0].attachment.as_mut().unwrap().expires_ms = Some(0);
    state
        .store
        .delete_message(channel_id, messages[0].id)
        .await
        .unwrap();
    state.store.insert_message(&messages[0]).await.unwrap();
    state.sweep_files().await;
    assert!(state.media.inline(&media).is_some());
    messages[1].attachment.as_mut().unwrap().expires_ms = Some(0);
    state
        .store
        .delete_message(channel_id, messages[1].id)
        .await
        .unwrap();
    state.store.insert_message(&messages[1]).await.unwrap();
    state.sweep_files().await;
    assert!(state.media.inline(&media).is_none());
}

#[tokio::test]
async fn invalid_and_oversized_files_cannot_create_messages_or_blobs() {
    let (state, owner, guild_id, channel_id) = fixture().await;
    state
        .handle_chat_tool(
            &owner,
            ClientMessage::SetGuildFilePolicy {
                guild_id,
                policy: FilePolicy {
                    max_bytes: 10,
                    retention_days: 7,
                },
            },
        )
        .await
        .unwrap();
    for command in [
        upload(channel_id, "big.txt", b"elevenbytes"),
        upload(channel_id, "../bad.exe", b"a"),
        upload(channel_id, "file.txt:payload", b"a"),
        upload(channel_id, "empty.txt", b""),
    ] {
        assert!(state.handle_chat_tool(&owner, command).await.is_err());
    }
    assert!(
        state
            .store
            .history(channel_id, 10, None)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(state.media.used_bytes(), 0);
    let member = User {
        pubkey: "22".repeat(32),
        username: "member".into(),
    };
    state.add_member(guild_id, &member).await.unwrap();
    assert!(
        state
            .handle_chat_tool(
                &member,
                ClientMessage::SetGuildFilePolicy {
                    guild_id,
                    policy: Default::default()
                }
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn search_filters_pins_and_attachments_persist_and_do_not_cross_channels() {
    let (state, owner, _, channel_id) = fixture().await;
    let file = upload(channel_id, "literal.txt", b"data");
    let ClientMessage::SendFile {
        request_id,
        channel_id,
        name,
        data_url,
        reply_to,
        ..
    } = file
    else {
        unreachable!()
    };
    state
        .handle_chat_tool(
            &owner,
            ClientMessage::SendFile {
                request_id,
                channel_id,
                name,
                data_url,
                reply_to,
                content: "100%_literal".into(),
            },
        )
        .await
        .unwrap();
    let message = state
        .store
        .history(channel_id, 10, None)
        .await
        .unwrap()
        .pop()
        .unwrap();
    state
        .handle_chat_tool(
            &owner,
            ClientMessage::PinMessage {
                channel_id,
                message_id: message.id,
                pinned: true,
            },
        )
        .await
        .unwrap();
    let query = MessageSearch {
        text: "%_".into(),
        author: Some(owner.pubkey.clone()),
        has_attachment: true,
        pinned_only: true,
        after_ms: Some(message.created_at.timestamp_millis()),
        before_ms: Some(message.created_at.timestamp_millis() + 1),
    };
    let found = state
        .store
        .search_messages(channel_id, &query, 0)
        .await
        .unwrap();
    assert_eq!(found.len(), 1);
    assert!(found[0].pinned);
    assert!(
        state
            .store
            .search_messages(channel_id, &query, 100)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        state
            .store
            .search_messages(Id::new_v4(), &query, 0)
            .await
            .unwrap()
            .is_empty()
    );
    let stranger = User {
        pubkey: "33".repeat(32),
        username: "stranger".into(),
    };
    assert!(
        state
            .handle_chat_tool(
                &stranger,
                ClientMessage::SearchMessages {
                    channel_id,
                    request_id: Id::new_v4(),
                    query,
                    offset: 0
                }
            )
            .await
            .is_err()
    );
    let archive = state
        .store
        .export_guild(state.channels.get(&channel_id).unwrap().guild_id)
        .await
        .unwrap()
        .unwrap();
    let restored = state.store.import_guild(&archive).await.unwrap();
    let imported = state.store.export_guild(restored).await.unwrap().unwrap();
    assert!(
        imported
            .messages
            .iter()
            .flat_map(|(_, messages)| messages)
            .any(|m| m.attachment.is_some() && m.pinned)
    );
}

#[tokio::test]
async fn stickers_require_management_and_can_only_be_sent_in_their_community() {
    let (state, owner, guild_id, channel_id) = fixture().await;
    let member = User {
        pubkey: "22".repeat(32),
        username: "member".into(),
    };
    state.add_member(guild_id, &member).await.unwrap();
    let image = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";
    let role = state
        .create_role(
            guild_id,
            "Sender",
            None,
            vec![dioxusfun_server::protocol::Permission::SendMessages],
            &owner.pubkey,
        )
        .await
        .unwrap();
    state
        .set_member_role(guild_id, role.id, &member.pubkey, true, &owner.pubkey)
        .await
        .unwrap();
    let create = || ClientMessage::CreateGuildSticker {
        guild_id,
        name: "test_sticker".into(),
        image: image.into(),
    };
    assert!(state.handle_chat_tool(&member, create()).await.is_err());
    state.handle_chat_tool(&owner, create()).await.unwrap();
    assert!(state.handle_chat_tool(&owner, create()).await.is_err());
    let sticker = state.store.stickers(guild_id).await.unwrap().pop().unwrap();
    assert!(
        state
            .store
            .referenced_media()
            .await
            .unwrap()
            .contains(sticker.image.strip_prefix("media:").unwrap())
    );
    let (other, channels, _, _) = state.create_guild("other", None, &owner).await.unwrap();
    assert!(
        state
            .handle_chat_tool(
                &owner,
                ClientMessage::SendSticker {
                    channel_id: channels[0].id,
                    sticker_id: sticker.id
                }
            )
            .await
            .is_err()
    );
    state
        .handle_chat_tool(
            &member,
            ClientMessage::SendSticker {
                channel_id,
                sticker_id: sticker.id,
            },
        )
        .await
        .unwrap();
    state
        .handle_chat_tool(
            &owner,
            ClientMessage::DeleteGuildSticker {
                guild_id,
                sticker_id: sticker.id,
            },
        )
        .await
        .unwrap();
    assert!(state.store.stickers(guild_id).await.unwrap().is_empty());
    assert!(
        state.store.history(channel_id, 10, None).await.unwrap()[0]
            .image
            .is_some()
    );
    let response = state
        .handle_chat_tool(
            &owner,
            ClientMessage::FetchGuildFilePolicy { guild_id: other.id },
        )
        .await
        .unwrap();
    assert!(matches!(
        response,
        Some(ServerMessage::GuildFilePolicy { .. })
    ));
}
