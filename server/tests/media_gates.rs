//! What a blob address may name and cost (trap 3): only the exact names the
//! store writes are served, each once and within the answer's budget, and a
//! bundle reaches the disk only as the one a manifest names.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine;
use discordia_bot::{Bot, BotIdentity};
use discordia_server::livekit::LiveKitConfig;
use discordia_server::media::{DEFAULT_MAX_BYTES, MediaStore};
use discordia_server::protocol::{
    ActivityCapability, ActivityManifest, AppManifest, ClientMessage, EmojiBlob, ServerMessage,
    User,
};
use discordia_server::state::AppState;
use discordia_server::store::Store;
use sha2::{Digest, Sha256};

fn scratch() -> PathBuf {
    std::env::temp_dir().join(format!("discordia-media-gates-{}", uuid::Uuid::new_v4()))
}

async fn gateway(dir: &Path) -> String {
    let preferred: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let cfg = discordia_server::ServerConfig {
        livekit: LiveKitConfig::from_env(dir),
        operators: Default::default(),
        identities: Default::default(),
        media_max_bytes: DEFAULT_MAX_BYTES,
        data_dir: dir.to_path_buf(),
        admission: Default::default(),
    };
    let handle = discordia_server::spawn(preferred, 100, cfg).await.unwrap();
    let url = format!("ws://{}/gateway", handle.addr);
    std::mem::forget(handle);
    url
}

async fn fetch(session: &mut Bot, images: &[&str]) -> Vec<EmojiBlob> {
    session
        .send(&ClientMessage::FetchEmoji {
            images: images.iter().map(|s| s.to_string()).collect(),
        })
        .await
        .unwrap();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(5), session.next_event())
            .await
            .unwrap()
            .unwrap();
        if let ServerMessage::EmojiBlobs { blobs } = event {
            return blobs;
        }
    }
}

/// On a case-insensitive volume `ABC….BIN` opens `abc….bin`, and the gate
/// matched only the lowercase spelling. Planting the capitals here shows the
/// same thing on any file system: a spelling the store never writes is refused.
#[tokio::test]
async fn a_blob_is_served_only_under_the_name_the_store_writes() {
    let dir = scratch();
    let url = gateway(&dir).await;
    let hash = hex::encode(Sha256::digest(b"a private attachment"));
    let media = dir.join("media");
    std::fs::write(media.join(format!("{hash}.bin")), b"a private attachment").unwrap();
    std::fs::write(
        media.join(format!("{}.BIN", hash.to_uppercase())),
        b"a private attachment",
    )
    .unwrap();
    let mut stranger = Bot::connect_as_user(&url, &BotIdentity::generate(), "Stranger")
        .await
        .unwrap();
    let shouted = format!("{}.BIN", hash.to_uppercase());
    let blobs = fetch(&mut stranger, &[&shouted, &format!("{hash}.bin")]).await;
    assert!(
        blobs.iter().all(|b| b.data_url.is_empty()),
        "a gated file reached someone with no message naming it: {:?}",
        blobs.iter().map(|b| &b.image).collect::<Vec<_>>()
    );
    assert!(
        blobs.iter().all(|b| b.image != shouted),
        "a name the store never writes was answered"
    );
}

#[tokio::test]
async fn one_answer_carries_each_blob_once() {
    let dir = scratch();
    let url = gateway(&dir).await;
    let png = b"\x89PNG\r\n\x1a\nthe picture".to_vec();
    let name = format!("{}.png", hex::encode(Sha256::digest(&png)));
    std::fs::write(dir.join("media").join(&name), &png).unwrap();
    let mut member = Bot::connect_as_user(&url, &BotIdentity::generate(), "Member")
        .await
        .unwrap();
    let blobs = fetch(&mut member, &[&name, &name, &name]).await;
    assert_eq!(blobs.len(), 1, "the same blob was read and encoded again");
    assert!(!blobs[0].data_url.is_empty());
}

/// A bundle stored before its hash was checked filled the shared quota with
/// anything; now nothing that is not the named bundle reaches the disk.
#[tokio::test]
async fn only_the_named_bundle_is_ever_written() {
    let dir = scratch();
    let store = Store::open_in(&dir).await.unwrap();
    let media = MediaStore::open(dir.join("media"), DEFAULT_MAX_BYTES).unwrap();
    let state = AppState::load_or_seed(store, media, Default::default())
        .await
        .unwrap();
    let owner = User {
        pubkey: "f".repeat(64),
        username: "Owner".into(),
    };
    let (guild, ..) = state.create_guild("Games", None, &owner).await.unwrap();
    let page = b"<!doctype html><p>rock</p>".to_vec();
    let app = BotIdentity::generate();
    let manifest = AppManifest {
        name: "Rock".into(),
        description: "Best of three".into(),
        icon: "R".into(),
        version: 1,
        permissions: vec![],
        intents: vec![],
        commands: vec![],
        activity: Some(ActivityManifest {
            sha256: hex::encode(Sha256::digest(&page)),
            capabilities: vec![ActivityCapability::UserRead],
            max_players: 2,
            url: None,
        }),
    };
    state
        .install_bot(
            guild.id,
            app.pubkey(),
            "Rock",
            vec![],
            vec![],
            Some(app.sign_manifest(&manifest)),
            &owner.pubkey,
        )
        .await
        .unwrap();
    let bundle = |bytes: &[u8]| {
        format!(
            "data:{};base64,{}",
            discordia_server::protocol::BUNDLE_MIME,
            base64::engine::general_purpose::STANDARD.encode(bytes)
        )
    };
    let dxa = || {
        std::fs::read_dir(dir.join("media"))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".dxa"))
            .count()
    };

    for wrong in [
        b"<p>another page</p>".to_vec(),
        format!("<p>{}</p>", "x".repeat(100_000)).into_bytes(),
    ] {
        assert!(
            state
                .upload_bundle(guild.id, app.pubkey(), &bundle(&wrong), &owner.pubkey)
                .is_err()
        );
        assert!(state.offer_bundle(app.pubkey(), &bundle(&wrong)).is_err());
    }
    assert_eq!(dxa(), 0, "a refused bundle was written");
    assert_eq!(
        state.media.used_bytes(),
        0,
        "a refused bundle was charged to the quota"
    );

    state
        .upload_bundle(guild.id, app.pubkey(), &bundle(&page), &owner.pubkey)
        .unwrap();
    assert_eq!(dxa(), 1);
}
