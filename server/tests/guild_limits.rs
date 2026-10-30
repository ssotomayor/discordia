use std::path::PathBuf;

use discordia_server::gateway::MAX_FRAME_BYTES;
use discordia_server::media::{DEFAULT_MAX_BYTES, MediaStore};
use discordia_server::protocol::{
    Activity, BotCommand, ChannelAccess, ChannelKind, Choice, CommandOption, Guild, Id, Intent,
    JoinGate, LevelTier, Leveling, MAX_ACCESS_ROLES, MAX_ACCESS_USERS, MAX_ACTIVITY_NAME,
    MAX_ACTIVITY_TEXT, MAX_COMMAND_CHOICES, MAX_COMMAND_DESCRIPTION, MAX_COMMAND_LABEL,
    MAX_COMMAND_SET_BYTES, MAX_EMOJIS_PER_GUILD, MAX_SOUNDS_PER_GUILD, MAX_TIER_EMOJI,
    MAX_TIER_NAME, MAX_TIERS, OptionKind, Permission, Profile, ServerMessage, ShownWhen, User,
    validate_commands,
};
use discordia_server::state::{
    AppState, MAX_CHANNELS_PER_GUILD, MAX_GUILD_INSTALLS, MAX_ROLES_PER_GUILD,
};
use discordia_server::store::Store;

/// Four UTF-8 bytes a character: the widest a capped name can be on the wire.
fn wide(chars: usize) -> String {
    "𝔸".repeat(chars)
}

fn key(n: usize) -> String {
    format!("{n:064x}")
}

fn channels_of(state: &AppState, gid: Id) -> Vec<discordia_server::protocol::Channel> {
    state
        .channels
        .iter()
        .filter(|c| c.guild_id == gid)
        .map(|c| c.clone())
        .collect()
}

struct Fixture {
    state: AppState,
    owner: User,
    guild: Guild,
    _dir: PathBuf,
}

async fn fixture() -> Fixture {
    let dir = std::env::temp_dir().join(format!("discordia-limits-{}", uuid::Uuid::new_v4()));
    let store = Store::open_in(&dir).await.unwrap();
    let media = MediaStore::open(dir.join("media"), DEFAULT_MAX_BYTES).unwrap();
    let state = AppState::load_or_seed(store, media, Default::default())
        .await
        .unwrap();
    let owner = User {
        pubkey: key(0xa11ce),
        username: "Owner".into(),
    };
    let (guild, ..) = state.create_guild("Limits", None, &owner).await.unwrap();
    Fixture {
        state,
        owner,
        guild,
        _dir: dir,
    }
}

/// As wide as a declaration may be: whole commands of the widest kind until
/// one more would cross `MAX_COMMAND_SET_BYTES`.
fn widest_commands() -> Vec<BotCommand> {
    let command = |c: usize| BotCommand {
        id: format!("{c:0>32}"),
        label: wide(MAX_COMMAND_LABEL),
        description: wide(MAX_COMMAND_DESCRIPTION),
        permission: Some(Permission::ManageGuild),
        options: (0..2)
            .map(|o| CommandOption {
                id: format!("{o:0>32}"),
                label: wide(MAX_COMMAND_LABEL),
                description: wide(MAX_COMMAND_DESCRIPTION),
                required: false,
                kind: OptionKind::Choice {
                    choices: (0..MAX_COMMAND_CHOICES)
                        .map(|k| Choice {
                            value: format!("{k:0>32}"),
                            label: wide(MAX_COMMAND_LABEL),
                        })
                        .collect(),
                },
                when: (o > 0).then(|| ShownWhen {
                    option: format!("{:0>32}", 0),
                    any_of: (0..MAX_COMMAND_CHOICES)
                        .map(|k| format!("{k:0>32}"))
                        .collect(),
                }),
            })
            .collect(),
        callable_by_apps: true,
    };
    let bare = |c: usize| BotCommand {
        options: Vec::new(),
        ..command(c)
    };
    let mut commands = Vec::new();
    for make in [&command as &dyn Fn(usize) -> BotCommand, &bare] {
        while commands.len() < discordia_server::protocol::MAX_BOT_COMMANDS {
            commands.push(make(commands.len()));
            if validate_commands(commands.clone()).is_err() {
                commands.pop();
                break;
            }
        }
    }
    validate_commands(commands).unwrap()
}

#[tokio::test]
async fn a_guild_at_every_cap_still_fits_in_its_members_ready() {
    let f = fixture().await;
    let (gid, by) = (f.guild.id, f.owner.pubkey.as_str());
    let observer = User {
        pubkey: key(0x0b5),
        username: "Observer".into(),
    };
    f.state.add_member(gid, &observer).await.unwrap();

    let mut roles = Vec::new();
    while let Ok(role) = f
        .state
        .create_role(
            gid,
            &wide(32),
            Some("#ffffff".into()),
            Permission::ALL.to_vec(),
            by,
        )
        .await
    {
        roles.push(role.id);
    }
    assert_eq!(f.state.guild_roles(gid).len(), MAX_ROLES_PER_GUILD);

    for _ in 0..MAX_CHANNELS_PER_GUILD {
        let _ = f
            .state
            .create_channel(
                gid,
                &wide(64),
                ChannelKind::Voice,
                Some(wide(120)),
                Default::default(),
                by,
            )
            .await;
    }
    let err = f
        .state
        .create_channel(
            gid,
            "divider",
            ChannelKind::Divider,
            None,
            Default::default(),
            by,
        )
        .await
        .unwrap_err();
    assert!(err.contains("at most"), "got: {err}");
    let mut access_roles: Vec<Id> = roles.clone();
    access_roles.resize_with(MAX_ACCESS_ROLES, uuid::Uuid::new_v4);
    let mut access_users: Vec<String> = (1..MAX_ACCESS_USERS).map(key).collect();
    access_users.push(observer.pubkey.clone());
    let channels: Vec<Id> = channels_of(&f.state, gid)
        .into_iter()
        .map(|c| c.id)
        .collect();
    assert_eq!(channels.len(), MAX_CHANNELS_PER_GUILD);
    for c in &channels {
        let access = ChannelAccess {
            roles: access_roles.clone(),
            users: access_users.clone(),
        };
        let _ = f.state.set_channel_access(*c, Some(access), by).await;
    }

    f.state
        .set_guild_profile(gid, Some(wide(64)), Some(wide(280)), None, None, by)
        .await
        .unwrap();
    f.state
        .set_join_gate(gid, JoinGate::Rules, Some(wide(4000)), by)
        .await
        .unwrap();
    f.state
        .set_guild_accent(gid, Some("#ffffff".into()), by)
        .await
        .unwrap();
    let leveling = Leveling {
        channels: channels.clone(),
        tiers: (0..MAX_TIERS as u64)
            .map(|xp| LevelTier {
                xp,
                name: wide(MAX_TIER_NAME),
                color: Some("#ffffff".into()),
                emoji: wide(MAX_TIER_EMOJI),
            })
            .collect(),
        ..Default::default()
    };
    f.state.set_guild_leveling(gid, leveling, by).await.unwrap();

    for n in 0..MAX_EMOJIS_PER_GUILD {
        f.state
            .create_emoji(gid, &format!("{n:z>32}"), format!("{}.png", key(n)), by)
            .await
            .unwrap();
    }
    for n in 0..MAX_SOUNDS_PER_GUILD {
        f.state
            .create_sound(
                gid,
                &format!("{}{n:02}", wide(30)),
                format!("media:{}.ogg", key(n)),
                by,
            )
            .await
            .unwrap();
    }

    let commands = widest_commands();
    let declared = serde_json::to_vec(&commands).unwrap().len();
    assert!(
        declared > MAX_COMMAND_SET_BYTES * 3 / 4,
        "only {declared} bytes"
    );
    for n in 0..MAX_GUILD_INSTALLS {
        let bot = key(0xb07_000 + n);
        f.state
            .install_bot(
                gid,
                &bot,
                &wide(32),
                Permission::BOT_INSTALLABLE.to_vec(),
                Intent::ALL.to_vec(),
                None,
                by,
            )
            .await
            .unwrap();
        f.state.register_commands(&bot, commands.clone()).unwrap();
        // A bot is its installer's to dress, so its card is as wide as allowed.
        f.state
            .set_profile(Profile {
                pubkey: bot.clone(),
                avatar: Some(format!("media:{}.png", key(n))),
                banner: Some(format!("media:{}.png", key(n + 1))),
                bio: Some(wide(280)),
                status: Some("online".into()),
                custom_status: Some(wide(80)),
                ..Default::default()
            })
            .await
            .unwrap();
        f.state.set_activity(
            &bot,
            Some(Activity {
                image: None,
                kind: Default::default(),
                name: wide(MAX_ACTIVITY_NAME),
                details: Some(wide(MAX_ACTIVITY_TEXT)),
                state: Some(wide(MAX_ACTIVITY_TEXT)),
                started_ms: None,
            }),
        );
    }

    let ready = f.state.snapshot_for(&observer).await.unwrap();
    let ServerMessage::Ready {
        channels, profiles, ..
    } = &ready
    else {
        unreachable!()
    };
    let here = channels.iter().filter(|c| c.guild_id == gid).count();
    assert_eq!(
        here, MAX_CHANNELS_PER_GUILD,
        "every channel, each visible to them"
    );
    assert_eq!(profiles.len(), MAX_GUILD_INSTALLS);
    let bytes = serde_json::to_vec(&ready).unwrap().len();
    eprintln!("Ready for a guild at every cap: {bytes} bytes of {MAX_FRAME_BYTES}");
    assert!(
        bytes < MAX_FRAME_BYTES,
        "{bytes} bytes cannot reach a member"
    );
}

#[tokio::test]
async fn a_guild_row_keeps_canonical_colours_and_only_its_own_channels() {
    let f = fixture().await;
    let (gid, by) = (f.guild.id, f.owner.pubkey.as_str());
    let padded = format!("#fff{}", " ".repeat(100_000));
    let guild = f
        .state
        .set_guild_accent(gid, Some(padded.clone()), by)
        .await
        .unwrap();
    assert_eq!(guild.accent.as_deref(), Some("#fff"));

    let own: Vec<Id> = channels_of(&f.state, gid)
        .into_iter()
        .map(|c| c.id)
        .collect();
    let mut asked: Vec<Id> = (0..10_000).map(|_| uuid::Uuid::new_v4()).collect();
    asked.extend(&own);
    let leveling = Leveling {
        channels: asked,
        tiers: vec![LevelTier {
            xp: 1,
            name: "Rank".into(),
            color: Some(padded.clone()),
            emoji: "*".into(),
        }],
        ..Default::default()
    };
    let guild = f.state.set_guild_leveling(gid, leveling, by).await.unwrap();
    let mut kept = guild.leveling.channels.clone();
    kept.sort_unstable();
    let mut own_sorted = own.clone();
    own_sorted.sort_unstable();
    assert_eq!(kept, own_sorted, "ids of no channel here are dropped");
    assert_eq!(guild.leveling.tiers[0].color.as_deref(), Some("#fff"));

    let role = f
        .state
        .create_role(gid, "Painted", Some(padded), vec![], by)
        .await
        .unwrap();
    assert_eq!(role.color.as_deref(), Some("#fff"));

    let foreign = Leveling {
        channels: (0..10_000).map(|_| uuid::Uuid::new_v4()).collect(),
        ..Default::default()
    };
    let loaded = discordia_server::protocol::sanitize_leveling(foreign);
    assert_eq!(
        loaded.channels.len(),
        MAX_CHANNELS_PER_GUILD,
        "load and import cap it too"
    );
}

#[tokio::test]
async fn a_reorder_names_each_channel_once_and_may_name_only_those_that_moved() {
    let f = fixture().await;
    let (gid, by) = (f.guild.id, f.owner.pubkey.as_str());
    let channel = channels_of(&f.state, gid)[0].clone();
    let repeated = vec![(channel.id, channel.position + 1); 5_000];
    let err = f
        .state
        .reorder_channels(gid, &repeated, by)
        .await
        .unwrap_err();
    assert!(err.contains("once"), "got: {err}");

    let moved = f
        .state
        .reorder_channels(gid, &[(channel.id, channel.position + 1)], by)
        .await
        .unwrap();
    assert_eq!(moved.len(), 1, "a partial list is what the client sends");
}

#[tokio::test]
async fn ready_carries_profiles_of_people_the_reader_shares_a_guild_with() {
    let f = fixture().await;
    let reader = User {
        pubkey: key(0x2ead),
        username: "Reader".into(),
    };
    f.state.add_member(f.guild.id, &reader).await.unwrap();
    for (n, who) in [f.owner.pubkey.clone(), reader.pubkey.clone(), key(0x57a)]
        .into_iter()
        .enumerate()
    {
        f.state
            .set_profile(Profile {
                pubkey: who,
                bio: Some(format!("bio {n}")),
                ..Default::default()
            })
            .await
            .unwrap();
    }
    let ServerMessage::Ready { profiles, .. } = f.state.snapshot_for(&reader).await.unwrap() else {
        unreachable!()
    };
    let mut shown: Vec<String> = profiles.into_iter().map(|p| p.pubkey).collect();
    shown.sort_unstable();
    let mut expected = vec![f.owner.pubkey.clone(), reader.pubkey.clone()];
    expected.sort_unstable();
    assert_eq!(
        shown, expected,
        "a stranger's profile rides in nobody's Ready"
    );
}
