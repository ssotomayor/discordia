use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use discordia_agent::{
    AgentConfig, ApiKey, Harness, HarnessError, Limits, ModelConfig, Prompt, Reply,
};
use discordia_bot::{Bot, BotIdentity};
use discordia_server::livekit::LiveKitConfig;
use discordia_server::protocol::{
    AppAccess, ArgValue, ChannelKind, ClientMessage, CommandArg, Id, Intent, Member, Message,
    Permission, ServerMessage,
};

async fn next_timeout(session: &mut Bot) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(5), session.next_event())
        .await
        .expect("timed out waiting for a gateway event")
        .expect("connection closed unexpectedly")
}

/// Echoes the last turn and remembers every prompt, so a test sees what the
/// provider would have been sent.
#[derive(Clone, Default)]
struct Echo {
    prompts: Arc<Mutex<Vec<Prompt>>>,
}

impl Harness for Echo {
    async fn reply(&self, prompt: Prompt) -> Result<Reply, HarnessError> {
        let last = prompt
            .turns
            .last()
            .map(|t| t.text.clone())
            .unwrap_or_default();
        self.prompts.lock().unwrap().push(prompt);
        Ok(Reply {
            text: format!("echo: {last}"),
            tokens: 10,
        })
    }
}

struct World {
    _handle: discordia_server::ServerHandle,
    owner: Bot,
    member: Bot,
    guild_id: Id,
    text: Id,
    agent_key: String,
    echo: Echo,
    seat: Member,
    _agent: tokio::task::JoinHandle<()>,
}

fn config(limits: Limits) -> AgentConfig {
    AgentConfig {
        name: "Helper".into(),
        prompt: "Be brief.".into(),
        model: ModelConfig::anthropic("claude-opus-5-5", ApiKey::new("unused")),
        limits,
        manifest_version: 1,
    }
}

async fn world(limits: Limits) -> World {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "discordia-agents-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let server = discordia_server::ServerConfig {
        livekit: LiveKitConfig::from_env(&dir),
        operators: Default::default(),
        identities: Default::default(),
        media_max_bytes: discordia_server::media::DEFAULT_MAX_BYTES,
        data_dir: dir,
        admission: Default::default(),
    };
    let preferred: SocketAddr = "127.0.0.1:19700".parse().unwrap();
    let handle = discordia_server::spawn(preferred, 100, server)
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
            name: "Agents".into(),
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

    let config = config(limits);
    let identity = BotIdentity::generate();
    owner
        .send(&ClientMessage::InstallBot {
            guild_id,
            bot_pubkey: identity.pubkey().to_string(),
            name: "Helper".into(),
            permissions: vec![Permission::SendMessages, Permission::ReadMessageHistory],
            intents: vec![Intent::GuildMessages, Intent::MessageContent],
            manifest: Some(identity.sign_manifest(&config.manifest())),
        })
        .await
        .unwrap();
    while !matches!(
        next_timeout(&mut owner).await,
        ServerMessage::GuildIntegrations { .. }
    ) {}

    let echo = Echo::default();
    let agent_key = identity.pubkey().to_string();
    let bot = Bot::connect(&url, &identity, "Helper").await.unwrap();
    let harness = echo.clone();
    let agent = tokio::spawn(async move {
        discordia_agent::run(bot, &identity, config, harness)
            .await
            .unwrap();
    });
    // Commands are registered after Ready: once they arrive, the agent listens.
    let mut seat = None;
    loop {
        match next_timeout(&mut member).await {
            ServerMessage::BotCommands(_) => break,
            ServerMessage::MemberJoin(m) if m.user.pubkey == agent_key => seat = Some(m),
            _ => {}
        }
    }

    World {
        _handle: handle,
        owner,
        member,
        guild_id,
        text,
        agent_key,
        echo,
        seat: seat.expect("members are told the agent took a seat"),
        _agent: agent,
    }
}

async fn sent(w: &mut World, content: &str) -> Message {
    w.member.send_message(w.text, content).await.unwrap();
    loop {
        if let ServerMessage::MessageCreate(m) = next_timeout(&mut w.member).await
            && m.content == content
        {
            return m;
        }
    }
}

async fn agent_message(w: &mut World) -> Message {
    loop {
        if let ServerMessage::MessageCreate(m) = next_timeout(&mut w.member).await
            && m.author_bot
        {
            return m;
        }
    }
}

/// Every message the agent posts within `window`.
async fn agent_messages_within(w: &mut World, window: Duration) -> Vec<Message> {
    let mut out = Vec::new();
    let deadline = tokio::time::Instant::now() + window;
    while let Ok(Some(event)) = tokio::time::timeout_at(deadline, w.member.next_event()).await {
        if let ServerMessage::MessageCreate(m) = event
            && m.author_bot
        {
            out.push(m);
        }
    }
    out
}

#[tokio::test]
async fn a_mention_is_answered_as_a_reply_with_the_channel_as_context() {
    let mut w = world(Limits::default()).await;
    sent(&mut w, "the build is red again").await;
    let trigger = sent(&mut w, "@Helper what did I just say?").await;

    let reply = agent_message(&mut w).await;
    assert_eq!(
        reply.reply_to.as_ref().map(|r| r.message_id),
        Some(trigger.id)
    );
    assert_eq!(
        reply.content,
        "echo: Member: the build is red again\nMember: @Helper what did I just say?"
    );

    let prompts = w.echo.prompts.lock().unwrap();
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].system.starts_with("Be brief."));
    assert!(prompts[0].system.contains("You are Helper"));
}

#[tokio::test]
async fn only_a_mention_or_a_reply_to_the_agent_wakes_it() {
    let mut w = world(Limits::default()).await;
    sent(&mut w, "nobody asked Helper anything").await;
    sent(&mut w, "mail@Helper is not a mention").await;
    let first = sent(&mut w, "@helper, hi").await;
    let answer = agent_message(&mut w).await;
    assert_eq!(
        answer.reply_to.as_ref().map(|r| r.message_id),
        Some(first.id)
    );

    w.member
        .send(&ClientMessage::SendMessage {
            channel_id: w.text,
            content: "and a follow-up".into(),
            image: None,
            image_name: None,
            reply_to: Some(answer.id),
        })
        .await
        .unwrap();
    let followed = agent_message(&mut w).await;
    assert!(
        followed.content.ends_with("Member: and a follow-up"),
        "{}",
        followed.content
    );
    assert_eq!(w.echo.prompts.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn past_the_hourly_cap_a_member_is_told_once_and_no_call_is_made() {
    let mut w = world(Limits {
        per_member_per_hour: 1,
        ..Limits::default()
    })
    .await;
    sent(&mut w, "@Helper one").await;
    assert!(agent_message(&mut w).await.content.starts_with("echo:"));
    sent(&mut w, "@Helper two").await;
    sent(&mut w, "@Helper three").await;

    let more = agent_messages_within(&mut w, Duration::from_millis(1500)).await;
    assert_eq!(more.len(), 1, "one notice, then silence: {more:?}");
    assert!(more[0].content.contains("this hour"));
    assert_eq!(w.echo.prompts.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn ask_answers_only_the_person_who_asked() {
    let mut w = world(Limits::default()).await;
    w.member
        .send(&ClientMessage::InvokeCommand {
            guild_id: w.guild_id,
            channel_id: w.text,
            bot_pubkey: w.agent_key.clone(),
            command: "ask".into(),
            args: vec![CommandArg {
                id: "question".into(),
                value: ArgValue::Text("is it lunch yet?".into()),
            }],
            parent: None,
        })
        .await
        .unwrap();
    let content = loop {
        match next_timeout(&mut w.member).await {
            ServerMessage::CommandResponse { content, .. } => break content,
            ServerMessage::MessageCreate(m) if m.author_bot => {
                panic!("a private answer went public")
            }
            _ => {}
        }
    };
    assert_eq!(content, "echo: Member: is it lunch yet?");
}

#[tokio::test]
async fn every_member_sees_that_the_app_is_an_agent_and_where_text_goes() {
    let w = world(Limits::default()).await;
    assert!(w.seat.bot);
    let agent = w
        .seat
        .agent
        .as_ref()
        .expect("the seat carries the disclosure");
    assert_eq!(agent.provider, "Anthropic");
    assert_eq!(agent.model, "claude-opus-5-5");
}

async fn set_access(w: &mut World, access: AppAccess) {
    w.owner
        .send(&ClientMessage::SetAppAccess {
            guild_id: w.guild_id,
            bot_pubkey: w.agent_key.clone(),
            access: access.clone(),
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::GuildIntegrations { bots, .. } = next_timeout(&mut w.owner).await
            && bots
                .iter()
                .any(|b| b.bot_pubkey == w.agent_key && b.access == access)
        {
            return;
        }
    }
}

async fn said_in(w: &mut World, channel_id: Id, content: &str) {
    w.member.send_message(channel_id, content).await.unwrap();
}

#[tokio::test]
async fn a_scoped_agent_hears_only_its_channels_and_members_see_the_scope() {
    let mut w = world(Limits::default()).await;
    w.owner
        .send(&ClientMessage::CreateChannel {
            guild_id: w.guild_id,
            name: "ask-the-bot".into(),
            kind: ChannelKind::Text,
            topic: None,
            divider: Default::default(),
        })
        .await
        .unwrap();
    let quiet = loop {
        if let ServerMessage::ChannelCreate(c) = next_timeout(&mut w.owner).await {
            break c.id;
        }
    };
    set_access(
        &mut w,
        AppAccess {
            channels: Some(vec![quiet]),
            roles: None,
        },
    )
    .await;
    loop {
        if let ServerMessage::MemberJoin(m) | ServerMessage::MemberUpdate(m) =
            next_timeout(&mut w.member).await
            && m.user.pubkey == w.agent_key
        {
            assert_eq!(m.reads, Some(vec![quiet]));
            break;
        }
    }

    let text = w.text;
    said_in(&mut w, text, "@Helper are you here?").await;
    assert!(
        agent_messages_within(&mut w, Duration::from_millis(1200))
            .await
            .is_empty(),
        "outside its channels it hears nothing"
    );
    said_in(&mut w, quiet, "@Helper and here?").await;
    let reply = agent_message(&mut w).await;
    assert_eq!(reply.channel_id, quiet);
}

#[tokio::test]
async fn when_roles_are_set_only_they_and_managers_reach_the_agent() {
    let mut w = world(Limits::default()).await;
    w.owner
        .send(&ClientMessage::CreateRole {
            guild_id: w.guild_id,
            name: "Askers".into(),
            color: None,
            permissions: vec![],
        })
        .await
        .unwrap();
    let role = loop {
        if let ServerMessage::GuildRoles { roles, .. } = next_timeout(&mut w.owner).await
            && let Some(r) = roles.iter().find(|r| r.name == "Askers")
        {
            break r.id;
        }
    };
    set_access(
        &mut w,
        AppAccess {
            channels: None,
            roles: Some(vec![role]),
        },
    )
    .await;

    let text = w.text;
    said_in(&mut w, text, "@Helper hello?").await;
    assert!(
        agent_messages_within(&mut w, Duration::from_millis(1200))
            .await
            .is_empty(),
        "a member without the role is not heard"
    );

    w.owner
        .send_message(text, "@Helper owner here")
        .await
        .unwrap();
    let reply = agent_message(&mut w).await;
    assert!(
        reply.content.ends_with("Owner: @Helper owner here"),
        "{}",
        reply.content
    );

    let member_key = w.member.user.pubkey.clone();
    w.owner
        .send(&ClientMessage::AssignRole {
            guild_id: w.guild_id,
            role_id: role,
            user_pubkey: member_key,
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::MemberUpdate(m) = next_timeout(&mut w.member).await
            && m.roles.contains(&role)
        {
            break;
        }
    }
    said_in(&mut w, text, "@Helper now?").await;
    let reply = agent_message(&mut w).await;
    assert!(
        reply.content.ends_with("Member: @Helper now?"),
        "{}",
        reply.content
    );
}
