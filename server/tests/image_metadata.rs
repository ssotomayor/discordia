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
    let (guild, channels, _, _) = state.create_guild("images", None, &owner).await.unwrap();
    let channel = channels
        .iter()
        .find(|c| c.kind == ChannelKind::Text)
        .unwrap()
        .id;
    state.channels.get_mut(&channel).unwrap().slowmode_secs = 60;
    state
        .last_post
        .insert((channel, owner.pubkey.clone()), std::time::Instant::now());
    assert_eq!(state.slowmode_remaining(channel, &owner.pubkey), Some(60));
    assert!(state.slowmode_exempt(guild.id, &owner.pubkey, false));
    let sent = state
        .push_image_message(
            channel,
            owner,
            "photo".into(),
            Some(PNG.into()),
            None,
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

#[tokio::test]
async fn sender_preview_is_stored_beside_the_unchanged_original_and_persisted() {
    use base64::Engine as _;
    use std::io::Cursor;
    let dir = std::env::temp_dir().join(format!("discordia-sender-preview-{}", Id::new_v4()));
    let store = Store::open_in(&dir).await.unwrap();
    let media = MediaStore::open(dir.join("media"), DEFAULT_MAX_BYTES).unwrap();
    let state = AppState::load_or_seed(store, media.clone(), Default::default())
        .await
        .unwrap();
    let owner = User {
        pubkey: "22".repeat(32),
        username: "owner".into(),
    };
    let (_, channels, _, _) = state.create_guild("images", None, &owner).await.unwrap();
    let channel = channels
        .iter()
        .find(|c| c.kind == ChannelKind::Text)
        .unwrap()
        .id;
    let encode = |width, height, color| {
        let image = image::RgbaImage::from_pixel(width, height, image::Rgba(color));
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes.into_inner())
        )
    };
    let original = encode(800, 600, [10, 20, 30, 255]);
    let preview = encode(640, 480, [30, 20, 10, 255]);
    let upload_id = Id::new_v4();
    let provisional = state
        .begin_image_upload(
            77,
            owner.clone(),
            upload_id,
            channel,
            "photo".into(),
            preview.clone(),
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(provisional.id, upload_id);
    assert!(provisional.image_metadata.as_ref().unwrap().upload_pending);
    assert!(
        state
            .store
            .history(channel, 10, None)
            .await
            .unwrap()
            .is_empty()
    );
    let provisional_preview = provisional.image.clone().unwrap();
    let sent = state
        .complete_image_upload(77, &owner.pubkey, upload_id, original.clone())
        .await
        .unwrap();
    assert_eq!(sent.id, upload_id);
    assert_eq!(
        media.inline(sent.image.as_deref().unwrap()).unwrap(),
        original
    );
    let metadata = sent.image_metadata.as_ref().unwrap();
    let preview_address = metadata.preview.as_deref().unwrap();
    assert!(!metadata.upload_pending);
    assert_eq!(preview_address, provisional_preview);
    assert_ne!(sent.image.as_deref(), Some(preview_address));
    assert_eq!((metadata.width, metadata.height), (Some(800), Some(600)));
    assert!(
        media
            .inline(preview_address)
            .unwrap()
            .starts_with("data:image/png;base64,")
    );
    let history = state.store.history(channel, 10, None).await.unwrap();
    assert_eq!(history[0].image_metadata, sent.image_metadata);
    let references = state.store.referenced_media().await.unwrap();
    assert!(references.contains(sent.image.as_deref().unwrap().trim_start_matches("media:")));
    assert!(references.contains(preview_address.trim_start_matches("media:")));
    drop(state);
}

#[tokio::test]
async fn preview_quota_failure_does_not_leave_an_unreferenced_original() {
    use base64::Engine as _;
    use std::io::Cursor;
    let dir = std::env::temp_dir().join(format!("discordia-preview-quota-{}", Id::new_v4()));
    let encode = |width, height, color| {
        let image = image::RgbaImage::from_pixel(width, height, image::Rgba(color));
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes.into_inner())
        )
    };
    let original = encode(800, 600, [10, 20, 30, 255]);
    let preview = encode(640, 480, [30, 20, 10, 255]);
    let decoded_len = |url: &str| {
        base64::engine::general_purpose::STANDARD
            .decode(url.split_once(',').unwrap().1)
            .unwrap()
            .len()
    };
    let quota = (decoded_len(&original) + decoded_len(&preview) - 1) as u64;
    let store = Store::open_in(&dir).await.unwrap();
    let media_dir = dir.join("media");
    let media = MediaStore::open(media_dir.clone(), quota).unwrap();
    let state = AppState::load_or_seed(store, media.clone(), Default::default())
        .await
        .unwrap();
    let owner = User {
        pubkey: "33".repeat(32),
        username: "owner".into(),
    };
    let (_, channels, _, _) = state.create_guild("images", None, &owner).await.unwrap();
    let channel = channels
        .iter()
        .find(|channel| channel.kind == ChannelKind::Text)
        .unwrap()
        .id;

    let result = state
        .push_image_message(
            channel,
            owner,
            "photo".into(),
            Some(original),
            Some(preview),
            None,
            None,
        )
        .await;
    assert!(result.is_err());
    assert_eq!(media.used_bytes(), 0);
    assert_eq!(std::fs::read_dir(media_dir).unwrap().count(), 0);
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
