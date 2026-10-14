use discordia_server::media::{DEFAULT_MAX_BYTES, MediaStore};
use discordia_server::protocol::{ChannelKind, ClientMessage, Id, Message, MessageSearch, User};
use discordia_server::state::AppState;
use discordia_server::store::Store;

const PNG: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

#[tokio::test]
async fn image_name_and_dimensions_survive_history_search_and_reopening_the_store() {
    let dir = std::env::temp_dir().join(format!("discordia-image-metadata-{}", Id::new_v4()));
    let store = Store::open_in(&dir).await.unwrap();
    let media = MediaStore::open(dir.join("media"), DEFAULT_MAX_BYTES).unwrap();
    let state = AppState::load_or_seed(store, media, Default::default())
        .await
        .unwrap();
    let owner = User {
        pubkey: "11".repeat(32),
        username: "owner".into(),
    };
    let (_, channels, _, _) = state.create_guild("images", None, &owner).await.unwrap();
    let channel = channels
        .iter()
        .find(|c| c.kind == ChannelKind::Text)
        .unwrap()
        .id;
    let sent = state
        .push_image_message(
            channel,
            owner,
            "photo".into(),
            Some(PNG.into()),
            Some("C:\\private\\phot:o.png\n".into()),
            None,
        )
        .await
        .unwrap();
    assert!(sent.image.as_ref().unwrap().starts_with("media:"));
    let metadata = sent.image_metadata.as_ref().unwrap();
    assert_eq!(metadata.name.as_deref(), Some("photo.png"));
    assert_eq!((metadata.width, metadata.height), (Some(1), Some(1)));
    assert_eq!(
        state.store.history(channel, 10, None).await.unwrap()[0].image_metadata,
        sent.image_metadata
    );
    assert_eq!(
        state
            .store
            .search_messages(channel, &MessageSearch::default(), 0)
            .await
            .unwrap()[0]
            .image_metadata,
        sent.image_metadata
    );
    drop(state);
    let reopened = Store::open_in(&dir).await.unwrap();
    assert_eq!(
        reopened.history(channel, 10, None).await.unwrap()[0].image_metadata,
        sent.image_metadata
    );
    let mut legacy = serde_json::to_value(&sent).unwrap();
    legacy.as_object_mut().unwrap().remove("image_metadata");
    assert!(
        serde_json::from_value::<Message>(legacy)
            .unwrap()
            .image_metadata
            .is_none()
    );
    let old_upload = serde_json::json!({"op":"send_message","d":{"channel_id": channel,"content":"old","image":PNG}});
    assert!(matches!(
        serde_json::from_value::<ClientMessage>(old_upload).unwrap(),
        ClientMessage::SendMessage {
            image_name: None,
            ..
        }
    ));
}

#[test]
fn clipboard_dimensions_do_not_require_a_filename_and_bad_names_are_removed() {
    let metadata = discordia_server::media::image_metadata(PNG, None);
    assert_eq!(metadata.name, None);
    assert_eq!((metadata.width, metadata.height), (Some(1), Some(1)));
    assert_eq!(
        discordia_server::media::image_metadata(PNG, Some("../..")).name,
        None
    );
    assert_eq!(
        discordia_server::media::image_metadata(PNG, Some(&"a".repeat(500)))
            .name
            .unwrap()
            .chars()
            .count(),
        240
    );
}
