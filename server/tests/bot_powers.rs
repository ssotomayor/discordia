use std::net::SocketAddr;
use std::time::Duration;

use discordia_bot::{Bot, BotIdentity};
use discordia_server::livekit::LiveKitConfig;
use discordia_server::protocol::{
    ChannelKind, ClientMessage, Id, Intent, Member, Permission, ServerMessage, XpChange,
};

async fn next_timeout(session: &mut Bot) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(5), session.next_event())
        .await
        .expect("timed out waiting for a gateway event")
        .expect("connection closed unexpectedly")
}

async fn next_error(session: &mut Bot) -> String {
    loop {
        if let ServerMessage::Error { message } = next_timeout(session).await {
            return message;
        }
    }
}

async fn member_update(session: &mut Bot, pubkey: &str) -> Member {
    loop {
        if let ServerMessage::MemberUpdate(m) = next_timeout(session).await
            && m.user.pubkey == pubkey
        {
            return m;
        }
    }
}

struct World {
    _handle: discordia_server::ServerHandle,
    url: String,
    owner: Bot,
    member: Bot,
    member_key: String,
    guild_id: Id,
    text: Id,
}

async fn world() -> World {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "discordia-bot-powers-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let config = discordia_server::ServerConfig {
        livekit: LiveKitConfig::from_env(&dir),
        operators: Default::default(),
        identities: Default::default(),
        media_max_bytes: discordia_server::media::DEFAULT_MAX_BYTES,
        data_dir: dir,
        admission: Default::default(),
    };
    let preferred: SocketAddr = "127.0.0.1:19700".parse().unwrap();
    let handle = discordia_server::spawn(preferred, 100, config)
        .await
        .expect("spawn server");
    let url = format!("ws://{}", handle.addr);
    let connect = async |name: &str| {
        let mut b = Bot::connect_as_user(&url, &BotIdentity::generate(), name)
            .await
            .unwrap();
        while !matches!(next_timeout(&mut b).await, ServerMessage::Ready { .. }) {}
        b
    };
    let mut owner = connect("Owner").await;
    owner
        .send(&ClientMessage::CreateGuild {
            name: "Powers".into(),
            template: None,
        })
        .await
        .unwrap();
    let (guild_id, text) = loop {
        if let ServerMessage::GuildJoined {
            guild, channels, ..
        } = next_timeout(&mut owner).await
        {
            let text = channels
                .iter()
                .find(|c| c.kind == ChannelKind::Text)
                .unwrap()
                .id;
            break (guild.id, text);
        }
    };
    let mut member = connect("Member").await;
    member
        .send(&ClientMessage::JoinGuild {
            guild_id,
            accept: true,
            pow_nonce: None,
        })
        .await
        .unwrap();
    while !matches!(
        next_timeout(&mut member).await,
        ServerMessage::GuildJoined { .. }
    ) {}
    let member_key = member.user.pubkey.clone();
    World {
        _handle: handle,
        url,
        owner,
        member,
        member_key,
        guild_id,
        text,
    }
}

async fn create_role(by: &mut Bot, guild_id: Id, name: &str, permissions: Vec<Permission>) -> Id {
    by.send(&ClientMessage::CreateRole {
        guild_id,
        name: name.into(),
        color: None,
        permissions,
    })
    .await
    .unwrap();
    loop {
        if let ServerMessage::GuildRoles { roles, .. } = next_timeout(by).await
            && let Some(r) = roles.iter().find(|r| r.name == name)
        {
            return r.id;
        }
    }
}

fn install_message(
    guild_id: Id,
    id: &BotIdentity,
    permissions: Vec<Permission>,
    intents: Vec<Intent>,
) -> ClientMessage {
    ClientMessage::InstallBot {
        guild_id,
        bot_pubkey: id.pubkey().to_string(),
        name: "Helper".into(),
        permissions,
        intents,
        manifest: None,
    }
}

async fn install(
    w: &mut World,
    permissions: Vec<Permission>,
    intents: Vec<Intent>,
) -> (Bot, String) {
    let id = BotIdentity::generate();
    w.owner
        .send(&install_message(w.guild_id, &id, permissions, intents))
        .await
        .unwrap();
    loop {
        if let ServerMessage::GuildIntegrations { bots, .. } = next_timeout(&mut w.owner).await
            && bots.iter().any(|b| b.bot_pubkey == id.pubkey())
        {
            break;
        }
    }
    let mut bot = Bot::connect(&w.url, &id, "Helper").await.unwrap();
    while !matches!(next_timeout(&mut bot).await, ServerMessage::Ready { .. }) {}
    (bot, id.pubkey().to_string())
}

#[tokio::test]
async fn a_bot_gives_only_roles_its_install_could_hold_itself() {
    let mut w = world().await;
    let rank = create_role(&mut w.owner, w.guild_id, "Rank", vec![]).await;
    let moderator = create_role(
        &mut w.owner,
        w.guild_id,
        "Mod",
        vec![Permission::ManageMessages],
    )
    .await;
    let admin = create_role(
        &mut w.owner,
        w.guild_id,
        "Admin",
        vec![Permission::ManageRoles],
    )
    .await;
    let (mut bot, _) = install(
        &mut w,
        vec![Permission::SendMessages, Permission::ManageRoles],
        vec![],
    )
    .await;

    bot.assign_role(w.guild_id, rank, &w.member_key)
        .await
        .unwrap();
    let m = member_update(&mut w.owner, &w.member_key).await;
    assert!(m.roles.contains(&rank), "a cosmetic role is given");

    bot.assign_role(w.guild_id, moderator, &w.member_key)
        .await
        .unwrap();
    let refused = next_error(&mut bot).await;
    assert!(refused.contains("manage_messages"), "{refused}");

    bot.assign_role(w.guild_id, admin, &w.member_key)
        .await
        .unwrap();
    let refused = next_error(&mut bot).await;
    assert!(refused.contains("owner-only"), "{refused}");

    bot.unassign_role(w.guild_id, rank, &w.member_key)
        .await
        .unwrap();
    let m = member_update(&mut w.owner, &w.member_key).await;
    assert!(!m.roles.contains(&rank), "and taken back");
}

#[tokio::test]
async fn a_bot_without_the_grants_is_refused() {
    let mut w = world().await;
    let rank = create_role(&mut w.owner, w.guild_id, "Rank", vec![]).await;
    let (mut bot, _) = install(&mut w, vec![Permission::SendMessages], vec![]).await;

    bot.assign_role(w.guild_id, rank, &w.member_key)
        .await
        .unwrap();
    let refused = next_error(&mut bot).await;
    assert!(refused.contains("manage_roles"), "{refused}");

    bot.adjust_xp(w.guild_id, &w.member_key, XpChange::Set(10))
        .await
        .unwrap();
    let refused = next_error(&mut bot).await;
    assert!(refused.contains("manage_xp"), "{refused}");
}

#[tokio::test]
async fn a_bot_sets_and_adds_experience_and_everyone_sees_it() {
    let mut w = world().await;
    let (mut bot, bot_key) = install(&mut w, vec![Permission::ManageXp], vec![]).await;

    bot.adjust_xp(w.guild_id, &w.member_key, XpChange::Set(500))
        .await
        .unwrap();
    assert_eq!(member_update(&mut w.member, &w.member_key).await.xp, 500);
    assert_eq!(member_update(&mut w.owner, &w.member_key).await.xp, 500);

    bot.adjust_xp(w.guild_id, &w.member_key, XpChange::Add(-600))
        .await
        .unwrap();
    assert_eq!(
        member_update(&mut w.member, &w.member_key).await.xp,
        0,
        "never below zero"
    );

    bot.adjust_xp(w.guild_id, &w.member_key, XpChange::Add(25))
        .await
        .unwrap();
    assert_eq!(member_update(&mut w.member, &w.member_key).await.xp, 25);

    bot.adjust_xp(w.guild_id, &bot_key, XpChange::Set(5))
        .await
        .unwrap();
    let refused = next_error(&mut bot).await;
    assert!(refused.contains("bots don't earn"), "{refused}");
}

#[tokio::test]
async fn manage_xp_on_a_role_lets_a_person_adjust() {
    let mut w = world().await;
    let owner_key = w.owner.user.pubkey.clone();
    w.member
        .send(&ClientMessage::AdjustXp {
            guild_id: w.guild_id,
            user_pubkey: owner_key.clone(),
            change: XpChange::Set(10),
        })
        .await
        .unwrap();
    let refused = next_error(&mut w.member).await;
    assert!(refused.contains("manage_xp"), "{refused}");

    let keeper = create_role(
        &mut w.owner,
        w.guild_id,
        "Keeper",
        vec![Permission::ManageXp],
    )
    .await;
    w.owner
        .send(&ClientMessage::AssignRole {
            guild_id: w.guild_id,
            role_id: keeper,
            user_pubkey: w.member_key.clone(),
        })
        .await
        .unwrap();
    member_update(&mut w.member, &w.member_key).await;
    w.member
        .send(&ClientMessage::AdjustXp {
            guild_id: w.guild_id,
            user_pubkey: owner_key.clone(),
            change: XpChange::Set(10),
        })
        .await
        .unwrap();
    assert_eq!(member_update(&mut w.owner, &owner_key).await.xp, 10);
}

/// Manage guild installs apps, and an app holding a power is a way to use it,
/// so a manager without manage roles could otherwise hand out roles through one.
#[tokio::test]
async fn only_someone_holding_a_power_can_grant_it_to_an_app() {
    let mut w = world().await;
    let manager = create_role(
        &mut w.owner,
        w.guild_id,
        "Manager",
        vec![Permission::ManageGuild],
    )
    .await;
    w.owner
        .send(&ClientMessage::AssignRole {
            guild_id: w.guild_id,
            role_id: manager,
            user_pubkey: w.member_key.clone(),
        })
        .await
        .unwrap();
    member_update(&mut w.member, &w.member_key).await;

    let id = BotIdentity::generate();
    w.member
        .send(&install_message(
            w.guild_id,
            &id,
            vec![Permission::ManageRoles],
            vec![],
        ))
        .await
        .unwrap();
    let refused = next_error(&mut w.member).await;
    assert!(
        refused.contains("you can't grant manage_roles"),
        "{refused}"
    );

    w.member
        .send(&install_message(
            w.guild_id,
            &id,
            vec![Permission::SendMessages],
            vec![],
        ))
        .await
        .unwrap();
    loop {
        if let ServerMessage::GuildIntegrations { bots, .. } = next_timeout(&mut w.member).await
            && bots.iter().any(|b| b.bot_pubkey == id.pubkey())
        {
            break;
        }
    }
}

#[tokio::test]
async fn history_without_the_content_intent_comes_back_blank() {
    let mut w = world().await;
    let owner_key = w.owner.user.pubkey.clone();
    w.owner.send_message(w.text, "secret plans").await.unwrap();
    loop {
        if let ServerMessage::MessageCreate(m) = next_timeout(&mut w.owner).await
            && m.content == "secret plans"
        {
            break;
        }
    }

    let history_of = async |bot: &mut Bot, text: Id| {
        bot.fetch_messages(text, 50, None).await.unwrap();
        loop {
            if let ServerMessage::MessageHistory { messages, .. } = next_timeout(bot).await {
                return messages
                    .into_iter()
                    .find(|m| m.author.pubkey == owner_key)
                    .expect("the owner's message is in the history")
                    .content;
            }
        }
    };
    let (mut blind, _) = install(
        &mut w,
        vec![Permission::ReadMessageHistory],
        vec![Intent::GuildMessages],
    )
    .await;
    assert_eq!(history_of(&mut blind, w.text).await, "");

    let (mut reader, _) = install(
        &mut w,
        vec![Permission::ReadMessageHistory],
        vec![Intent::GuildMessages, Intent::MessageContent],
    )
    .await;
    assert_eq!(history_of(&mut reader, w.text).await, "secret plans");
}
