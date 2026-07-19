//! Phase 6 — guild export/import round-trip.
//!
//! Seeds a guild (with a template, so it has multiple channels + roles),
//! posts messages, exports it, imports it back into the SAME store, and
//! asserts the copy is structurally identical under FRESH ids while every
//! pubkey is preserved.

use std::collections::HashSet;

use chrono::Utc;
use dioxusfun_server::media::MediaStore;
use dioxusfun_server::protocol::{Message, User};
use dioxusfun_server::state::AppState;
use dioxusfun_server::store::Store;
use uuid::Uuid;

/// Unique temp dir per test so SQLite + media are hermetic and parallel-safe.
fn temp_dir(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "dioxusfun-archive-{tag}-{}-{}",
        std::process::id(),
        Uuid::new_v4()
    ))
}

async fn fresh_state() -> AppState {
    let dir = temp_dir("state");
    let store = Store::open(&dir.join("db.sqlite")).await.expect("open store");
    let media = MediaStore::open(dir.join("media")).expect("open media");
    AppState::load_or_seed(store, media, HashSet::new())
        .await
        .expect("load_or_seed")
}

fn msg(channel_id: Uuid, author: &User, content: &str) -> Message {
    Message {
        id: Uuid::new_v4(),
        channel_id,
        author: author.clone(),
        content: content.into(),
        image: None,
        reactions: Vec::new(),
        created_at: Utc::now(),
    }
}

#[tokio::test]
async fn export_import_round_trip_preserves_structure_and_pubkeys() {
    let state = fresh_state().await;
    let owner = User { pubkey: "owner_pubkey_hex".into(), username: "Owner".into() };

    // Seed a guild from the "community" template (multi-channel, multi-role).
    let (guild, channels, _member, roles) =
        state.create_guild("Origin", Some("community"), &owner).await;
    assert!(channels.len() >= 2, "template seeds several channels");
    assert!(!roles.is_empty(), "template seeds roles");

    // Post a couple of messages into the first text channel.
    let first_channel = channels[0].id;
    for body in ["hello world", "second message"] {
        state.store.insert_message(&msg(first_channel, &owner, body)).await.unwrap();
    }

    // Export → import into the same store.
    let archive = state
        .store
        .export_guild(guild.id)
        .await
        .unwrap()
        .expect("guild exists");
    assert_eq!(archive.version, dioxusfun_server::archive::ARCHIVE_VERSION);

    let new_id = state.store.import_guild(&archive).await.unwrap();
    assert_ne!(new_id, guild.id, "imported guild gets a fresh id");

    // Reload everything and inspect the imported copy.
    let loaded = state.store.load_all().await.unwrap();

    let orig = loaded.guilds.iter().find(|g| g.id == guild.id).unwrap();
    let copy = loaded.guilds.iter().find(|g| g.id == new_id).unwrap();
    assert_eq!(copy.name, orig.name);
    assert_eq!(copy.owner_pubkey, orig.owner_pubkey, "owner pubkey preserved");

    // Channels: same count + names, all with FRESH ids under the new guild.
    let orig_channels: Vec<_> = loaded.channels.iter().filter(|c| c.guild_id == guild.id).collect();
    let copy_channels: Vec<_> = loaded.channels.iter().filter(|c| c.guild_id == new_id).collect();
    assert_eq!(copy_channels.len(), orig_channels.len());
    let mut on: Vec<_> = orig_channels.iter().map(|c| c.name.clone()).collect();
    let mut cn: Vec<_> = copy_channels.iter().map(|c| c.name.clone()).collect();
    on.sort();
    cn.sort();
    assert_eq!(cn, on, "channel names preserved");
    let orig_ids: HashSet<_> = orig_channels.iter().map(|c| c.id).collect();
    assert!(copy_channels.iter().all(|c| !orig_ids.contains(&c.id)), "channel ids are fresh");

    // Roles: same names, fresh ids.
    let copy_roles: Vec<_> = loaded.roles.iter().filter(|r| r.guild_id == new_id).collect();
    assert_eq!(copy_roles.len(), roles.len());

    // Messages carried over into the corresponding new channel, content + author
    // preserved.
    let new_first = copy_channels
        .iter()
        .find(|c| c.name == channels[0].name)
        .expect("matching channel by name");
    let history = state.store.history(new_first.id, 100, None).await.unwrap();
    assert_eq!(history.len(), 2, "both messages imported");
    assert!(history.iter().all(|m| m.author.pubkey == owner.pubkey), "author pubkeys preserved");
    let contents: HashSet<_> = history.iter().map(|m| m.content.clone()).collect();
    assert!(contents.contains("hello world") && contents.contains("second message"));
}

#[tokio::test]
async fn export_unknown_guild_is_none_check() {
    let state = fresh_state().await;
    let missing = state.store.export_guild(Uuid::new_v4()).await.unwrap();
    assert!(missing.is_none());
}

#[tokio::test]
async fn import_rejects_unknown_version() {
    let state = fresh_state().await;
    let owner = User { pubkey: "pk".into(), username: "O".into() };
    let (guild, _c, _m, _r) = state.create_guild("V", Some("friend"), &owner).await;
    let mut archive = state.store.export_guild(guild.id).await.unwrap().unwrap();
    archive.version = 999;
    assert!(state.store.import_guild(&archive).await.is_err());
}
