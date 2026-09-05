use std::collections::HashSet;

use chrono::Utc;
use dioxusfun_server::media::MediaStore;
use dioxusfun_server::protocol::{Message, User};
use dioxusfun_server::state::AppState;
use dioxusfun_server::store::Store;
use uuid::Uuid;

fn temp_dir(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "dioxusfun-archive-{tag}-{}-{}",
        std::process::id(),
        Uuid::new_v4()
    ))
}

async fn fresh_state() -> AppState {
    let dir = temp_dir("state");
    let store = Store::open_in(&dir).await.expect("open store");
    let media = MediaStore::open(
        dir.join("media"),
        dioxusfun_server::media::DEFAULT_MAX_BYTES,
    )
    .expect("open media");
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
        reply_to: None,
        created_at: Utc::now(),
    }
}

#[tokio::test]
async fn export_import_round_trip_preserves_structure_and_pubkeys() {
    let state = fresh_state().await;
    let owner = User {
        pubkey: "owner_pubkey_hex".into(),
        username: "Owner".into(),
    };

    let (guild, channels, _member, roles) = state
        .create_guild("Origin", Some("community"), &owner)
        .await
        .expect("create guild");
    assert!(channels.len() >= 2, "template seeds several channels");
    assert!(!roles.is_empty(), "template seeds roles");

    let first_channel = channels[0].id;
    for body in ["hello world", "second message"] {
        state
            .store
            .insert_message(&msg(first_channel, &owner, body))
            .await
            .unwrap();
    }

    let archive = state
        .store
        .export_guild(guild.id)
        .await
        .unwrap()
        .expect("guild exists");
    assert_eq!(archive.version, dioxusfun_server::archive::ARCHIVE_VERSION);

    let new_id = state.store.import_guild(&archive).await.unwrap();
    assert_ne!(new_id, guild.id, "imported guild gets a fresh id");

    let loaded = state.store.load_all().await.unwrap();

    let orig = loaded.guilds.iter().find(|g| g.id == guild.id).unwrap();
    let copy = loaded.guilds.iter().find(|g| g.id == new_id).unwrap();
    assert_eq!(copy.name, orig.name);
    assert_eq!(
        copy.owner_pubkey, orig.owner_pubkey,
        "owner pubkey preserved"
    );

    let orig_channels: Vec<_> = loaded
        .channels
        .iter()
        .filter(|c| c.guild_id == guild.id)
        .collect();
    let copy_channels: Vec<_> = loaded
        .channels
        .iter()
        .filter(|c| c.guild_id == new_id)
        .collect();
    assert_eq!(copy_channels.len(), orig_channels.len());
    let mut on: Vec<_> = orig_channels.iter().map(|c| c.name.clone()).collect();
    let mut cn: Vec<_> = copy_channels.iter().map(|c| c.name.clone()).collect();
    on.sort();
    cn.sort();
    assert_eq!(cn, on, "channel names preserved");
    let orig_ids: HashSet<_> = orig_channels.iter().map(|c| c.id).collect();
    assert!(
        copy_channels.iter().all(|c| !orig_ids.contains(&c.id)),
        "channel ids are fresh"
    );

    let copy_roles: Vec<_> = loaded
        .roles
        .iter()
        .filter(|r| r.guild_id == new_id)
        .collect();
    assert_eq!(copy_roles.len(), roles.len());

    let new_first = copy_channels
        .iter()
        .find(|c| c.name == channels[0].name)
        .expect("matching channel by name");
    let history = state.store.history(new_first.id, 100, None).await.unwrap();
    assert_eq!(history.len(), 2, "both messages imported");
    assert!(
        history.iter().all(|m| m.author.pubkey == owner.pubkey),
        "author pubkeys preserved"
    );
    let contents: HashSet<_> = history.iter().map(|m| m.content.clone()).collect();
    assert!(contents.contains("hello world") && contents.contains("second message"));
}

// Both halves of the migration in docs/OPS.md: a guild made by a running
// server, then reached the way the `export` CLI reaches it. Every other test
// here builds its own Store, so none of them saw the two open different files.
#[tokio::test]
async fn the_export_cli_opens_the_database_the_server_wrote() {
    use dioxusfun_bot::{Bot, BotIdentity};
    use dioxusfun_server::livekit::LiveKitConfig;
    use dioxusfun_server::protocol::{ClientMessage, ServerMessage};
    use std::time::Duration;

    let dir = temp_dir("cli");
    let guild_id = {
        let cfg = dioxusfun_server::ServerConfig {
            livekit: LiveKitConfig::from_env(&dir),
            operators: Default::default(),
            identities: Default::default(),
            media_max_bytes: dioxusfun_server::media::DEFAULT_MAX_BYTES,
            data_dir: dir.clone(),
        };
        let handle = dioxusfun_server::spawn("127.0.0.1:0".parse().unwrap(), 100, cfg)
            .await
            .expect("spawn");
        let identity = BotIdentity::generate();
        let mut owner = Bot::connect_as_user(&format!("ws://{}", handle.addr), &identity, "Owner")
            .await
            .unwrap();

        let next = async |owner: &mut Bot| {
            tokio::time::timeout(Duration::from_secs(5), owner.next_event())
                .await
                .expect("timed out")
                .expect("closed")
        };
        loop {
            if matches!(next(&mut owner).await, ServerMessage::Ready { .. }) {
                break;
            }
        }
        owner
            .send(&ClientMessage::CreateGuild {
                name: "Migrated".into(),
                template: None,
            })
            .await
            .unwrap();
        let id = loop {
            if let ServerMessage::GuildJoined { guild, .. } = next(&mut owner).await {
                break guild.id;
            }
        };
        handle.abort();
        id
    };

    let dbs_after_serving = db_files(&dir);
    assert_eq!(dbs_after_serving.len(), 1, "the server wrote one database");

    let cli_store = Store::open_in(&dir)
        .await
        .expect("open the way the CLI does");
    let archive = cli_store
        .export_guild(guild_id)
        .await
        .expect("export")
        .expect("the CLI finds the guild the server persisted");
    assert_eq!(archive.guild.name, "Migrated");

    // `create_if_missing` means a CLI pointed at the wrong name gets a silent
    // empty database rather than an error, so the count is the real assertion.
    assert_eq!(
        db_files(&dir),
        dbs_after_serving,
        "the CLI opened the server's database instead of creating its own"
    );
}

fn db_files(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("data dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".db") || n.ends_with(".sqlite"))
        .collect();
    names.sort();
    names
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
    let owner = User {
        pubkey: "pk".into(),
        username: "O".into(),
    };
    let (guild, _c, _m, _r) = state
        .create_guild("V", Some("friend"), &owner)
        .await
        .expect("create guild");
    let mut archive = state.store.export_guild(guild.id).await.unwrap().unwrap();
    archive.version = 999;
    assert!(state.store.import_guild(&archive).await.is_err());
}

fn hex_key(fill: char) -> String {
    std::iter::repeat_n(fill, 64).collect()
}

/// Cross-instance migration is exactly the path a hostile archive arrives on,
/// so the import gets every filter the gateway applies to a live client.
#[tokio::test]
async fn a_hostile_archive_is_filtered_on_import() {
    use dioxusfun_server::archive::{ARCHIVE_VERSION, GuildArchive};
    use dioxusfun_server::protocol::{
        AuditEntry, BotInstall, Channel, ChannelKind, Guild, GuildEmoji, Intent, Member,
        Permission, Role,
    };

    let state = fresh_state().await;
    let gid = Uuid::new_v4();
    let chan = Uuid::new_v4();
    let owner = hex_key('a');
    let author = User {
        pubkey: hex_key('b'),
        username: format!("\u{202E}{}", "x".repeat(80)),
    };
    let archive = GuildArchive {
        version: ARCHIVE_VERSION,
        guild: Guild {
            id: gid,
            name: format!("\u{202E}{}", "g".repeat(300)),
            icon: None,
            owner_pubkey: owner.clone(),
            accent: Some("javascript:alert(1)".into()),
            visibility: Default::default(),
            description: Some(format!("\u{0}{}", "d".repeat(5000))),
            icon_image: Some("https://evil.example/track.png".into()),
            banner: None,
            retention_days: None,
            join_gate: Default::default(),
            rules: None,
            panic_mode: false,
        },
        channels: vec![Channel {
            id: chan,
            guild_id: gid,
            name: "general\u{2028}INFO forged".into(),
            kind: ChannelKind::Text,
            topic: Some("t".repeat(1000)),
            read_only: false,
            slowmode_secs: 0,
            position: 0,
        }],
        roles: vec![Role {
            id: Uuid::new_v4(),
            guild_id: gid,
            name: "r".repeat(100),
            color: Some("red; background:url(x)".into()),
            permissions: vec![],
            position: 0,
        }],
        emojis: vec![GuildEmoji {
            id: Uuid::new_v4(),
            guild_id: gid,
            shortcode: "Bad Code!".into(),
            image: "../../etc/passwd".into(),
            added_by: owner.clone(),
            created_ms: 0,
        }],
        members: vec![
            Member {
                user: author.clone(),
                guild_id: gid,
                online: false,
                bot: false,
                roles: vec![],
                xp: 0,
            },
            Member {
                user: User {
                    pubkey: "\u{0}\u{0}".into(),
                    username: "ghost".into(),
                },
                guild_id: gid,
                online: false,
                bot: false,
                roles: vec![],
                xp: 0,
            },
        ],
        bans: vec![hex_key('c'), "\u{202E}".into()],
        invite: None,
        bot_installs: vec![BotInstall {
            guild_id: gid,
            bot_pubkey: hex_key('d'),
            name: "\u{0}".into(),
            permissions: vec![Permission::SendMessages, Permission::ManageGuild],
            intents: vec![Intent::GuildMessages],
        }],
        messages: vec![(
            chan,
            vec![
                msg(chan, &author, &"m".repeat(10_000)),
                msg(chan, &author, "   "),
            ],
        )],
        audit: vec![AuditEntry {
            at_ms: 0,
            actor_pubkey: owner.clone(),
            action: "kick\nINFO forged".into(),
            target: String::new(),
            detail: "x".repeat(2000),
        }],
    };

    let new_id = state.store.import_guild(&archive).await.expect("import");
    let loaded = state.store.load_all().await.expect("load");

    let guild = loaded
        .guilds
        .iter()
        .find(|g| g.id == new_id)
        .expect("guild");
    assert!(
        !guild.name.contains('\u{202E}'),
        "bidi override survived: {:?}",
        guild.name
    );
    assert!(guild.name.chars().count() <= 64);
    assert!(guild.accent.is_none(), "a non-colour accent was kept");
    assert!(guild.icon_image.is_none(), "a link was kept as an icon");
    assert!(
        guild
            .description
            .as_ref()
            .is_some_and(|d| d.chars().count() <= 280 && !d.contains('\u{0}'))
    );

    let channel = loaded
        .channels
        .iter()
        .find(|c| c.guild_id == new_id)
        .expect("channel");
    assert!(!channel.name.contains('\u{2028}'));
    assert!(
        channel
            .topic
            .as_ref()
            .is_some_and(|t| t.chars().count() <= 120)
    );

    let role = loaded
        .roles
        .iter()
        .find(|r| r.guild_id == new_id)
        .expect("role");
    assert!(role.name.chars().count() <= 32);
    assert!(role.color.is_none());

    assert!(
        loaded.emojis.iter().all(|e| e.guild_id != new_id),
        "a bad emoji was kept"
    );

    let members: Vec<_> = loaded
        .members
        .iter()
        .filter(|(g, ..)| *g == new_id)
        .collect();
    assert_eq!(members.len(), 1, "the member with no key is gone");
    assert!(members[0].2.chars().count() <= 32 && !members[0].2.contains('\u{202E}'));

    let bans: Vec<_> = loaded.bans.iter().filter(|(g, _)| *g == new_id).collect();
    assert_eq!(bans.len(), 1);

    let bot = loaded
        .bot_installs
        .iter()
        .find(|b| b.guild_id == new_id)
        .expect("bot");
    assert_eq!(bot.name, "Bot");
    assert_eq!(bot.permissions, vec![Permission::SendMessages]);

    let history = state
        .store
        .history(channel.id, 50, None)
        .await
        .expect("history");
    assert_eq!(history.len(), 1, "the blank message is gone");
    assert!(history[0].content.len() <= 2000);
    assert!(history[0].author.username.chars().count() <= 32);

    let audit = state.store.audit_log(new_id, 10).await.expect("audit");
    assert_eq!(audit.len(), 1);
    assert!(!audit[0].action.contains('\n'));
    assert!(audit[0].detail.chars().count() <= 280);
}

/// Rows written before a filter existed stay as they were written; loading is
/// the one moment every row passes through, so that is where they are cleaned.
#[tokio::test]
async fn rows_written_before_the_filters_are_cleaned_at_load() {
    use dioxusfun_server::protocol::Guild;

    let dir = temp_dir("legacy");
    let store = Store::open_in(&dir).await.expect("open store");
    let id = Uuid::new_v4();
    store
        .upsert_guild(&Guild {
            id,
            name: format!("\u{202E}{}", "legacy".repeat(40)),
            icon: None,
            owner_pubkey: hex_key('a'),
            accent: Some("not-a-colour".into()),
            visibility: Default::default(),
            description: Some("https://x\u{2029}INFO".into()),
            icon_image: Some("https://evil.example/i.png".into()),
            banner: None,
            retention_days: None,
            join_gate: Default::default(),
            rules: None,
            panic_mode: false,
        })
        .await
        .expect("write the dirty row directly");

    let media = MediaStore::open(
        dir.join("media"),
        dioxusfun_server::media::DEFAULT_MAX_BYTES,
    )
    .expect("open media");
    let state = AppState::load_or_seed(store, media, HashSet::new())
        .await
        .expect("load");
    let guild = state.guilds.get(&id).expect("loaded").clone();
    assert!(!guild.name.contains('\u{202E}'));
    assert!(guild.name.chars().count() <= 64);
    assert!(guild.accent.is_none());
    assert!(guild.icon_image.is_none());
    assert!(
        guild
            .description
            .as_ref()
            .is_some_and(|d| !d.contains('\u{2029}'))
    );
}
