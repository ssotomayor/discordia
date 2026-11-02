use std::collections::HashSet;
use std::net::SocketAddr;
use std::time::Duration;

use discordia_bot::{Bot, BotIdentity};
use discordia_server::livekit::LiveKitConfig;
use discordia_server::protocol::{
    AgentProvider, AppsPolicy, ClientMessage, Id, Intent, Permission, ServerAgentSpec,
    ServerMessage,
};

async fn next_timeout(session: &mut Bot) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(10), session.next_event())
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

struct World {
    _handle: discordia_server::ServerHandle,
    operator: Bot,
    operator_guild: Id,
    /// Owns a guild of their own; not an operator.
    admin: Bot,
    admin_guild: Id,
}

async fn connect(url: &str, id: &BotIdentity, name: &str) -> Bot {
    let mut b = Bot::connect_as_user(url, id, name).await.unwrap();
    while !matches!(next_timeout(&mut b).await, ServerMessage::Ready { .. }) {}
    b
}

async fn guild_of(owner: &mut Bot, name: &str) -> Id {
    owner
        .send(&ClientMessage::CreateGuild {
            name: name.into(),
            template: None,
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::GuildJoined { guild, .. } = next_timeout(owner).await {
            return guild.id;
        }
    }
}

async fn world() -> World {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "discordia-server-apps-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let operator_id = BotIdentity::generate();
    let config = discordia_server::ServerConfig {
        livekit: LiveKitConfig::from_env(&dir),
        operators: HashSet::from([operator_id.pubkey().to_string()]),
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
    let mut operator = connect(&url, &operator_id, "Operator").await;
    let operator_guild = guild_of(&mut operator, "Ops").await;
    let mut admin = connect(&url, &BotIdentity::generate(), "Admin").await;
    let admin_guild = guild_of(&mut admin, "Elsewhere").await;
    World {
        _handle: handle,
        operator,
        operator_guild,
        admin,
        admin_guild,
    }
}

fn install(guild_id: Id, app: &str) -> ClientMessage {
    ClientMessage::InstallBot {
        guild_id,
        bot_pubkey: app.into(),
        name: "App".into(),
        permissions: vec![Permission::SendMessages],
        intents: vec![Intent::GuildMessages],
        manifest: None,
    }
}

async fn installed(session: &mut Bot, guild_id: Id, app: &str) -> bool {
    loop {
        if let ServerMessage::GuildIntegrations { guild_id: g, bots } = next_timeout(session).await
            && g == guild_id
        {
            return bots.iter().any(|b| b.bot_pubkey == app);
        }
    }
}

async fn policy(w: &mut World, policy: AppsPolicy) {
    w.operator
        .send(&ClientMessage::SetAppsPolicy { policy })
        .await
        .unwrap();
    loop {
        if let ServerMessage::ServerApps { policy: p, .. } = next_timeout(&mut w.operator).await
            && p == policy
        {
            return;
        }
    }
}

#[tokio::test]
async fn under_approved_only_a_key_the_operator_approved_installs() {
    let mut w = world().await;
    policy(&mut w, AppsPolicy::Approved).await;
    let app = BotIdentity::generate().pubkey().to_string();

    w.admin.send(&install(w.admin_guild, &app)).await.unwrap();
    assert!(next_error(&mut w.admin).await.contains("approved"));

    w.operator
        .send(&install(w.operator_guild, &app))
        .await
        .unwrap();
    assert!(
        installed(&mut w.operator, w.operator_guild, &app).await,
        "the operator is never refused"
    );

    w.operator
        .send(&ClientMessage::SetAppApproved {
            pubkey: app.clone(),
            approved: true,
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::ServerApps { approved, .. } = next_timeout(&mut w.operator).await
            && approved.contains(&app)
        {
            break;
        }
    }
    w.admin.send(&install(w.admin_guild, &app)).await.unwrap();
    assert!(installed(&mut w.admin, w.admin_guild, &app).await);
}

#[tokio::test]
async fn off_refuses_every_guild_install_but_the_operators() {
    let mut w = world().await;
    policy(&mut w, AppsPolicy::Off).await;
    let app = BotIdentity::generate().pubkey().to_string();
    w.admin.send(&install(w.admin_guild, &app)).await.unwrap();
    assert!(next_error(&mut w.admin).await.contains("doesn't allow"));
}

#[tokio::test]
async fn a_ban_takes_an_app_out_of_every_guild_and_keeps_it_out() {
    let mut w = world().await;
    let app = BotIdentity::generate().pubkey().to_string();
    w.admin.send(&install(w.admin_guild, &app)).await.unwrap();
    assert!(installed(&mut w.admin, w.admin_guild, &app).await);

    w.operator
        .send(&ClientMessage::FetchServerApps)
        .await
        .unwrap();
    let listed = loop {
        if let ServerMessage::ServerApps { installs, .. } = next_timeout(&mut w.operator).await {
            break installs;
        }
    };
    assert!(
        listed
            .iter()
            .any(|i| i.install.bot_pubkey == app && i.guild_name == "Elsewhere"),
        "the operator sees installs in guilds they are not in"
    );

    w.operator
        .send(&ClientMessage::SetAppBanned {
            pubkey: app.clone(),
            banned: true,
        })
        .await
        .unwrap();
    assert!(
        !installed(&mut w.admin, w.admin_guild, &app).await,
        "removed from the admin's guild"
    );
    w.admin.send(&install(w.admin_guild, &app)).await.unwrap();
    assert!(next_error(&mut w.admin).await.contains("banned"));

    w.admin.send(&ClientMessage::FetchServerApps).await.unwrap();
    assert!(next_error(&mut w.admin).await.contains("operator"));
}

fn agent_spec(offered: bool) -> ServerAgentSpec {
    ServerAgentSpec {
        name: "Helper".into(),
        prompt: String::new(),
        provider: AgentProvider::OpenAiCompatible,
        provider_label: "Local".into(),
        base_url: "http://127.0.0.1:9/v1".into(),
        model: "m".into(),
        per_member_per_hour: 10,
        daily_tokens: 10_000,
        guilds: Vec::new(),
        offered,
        guild_daily_tokens: 1_000,
    }
}

#[tokio::test]
async fn an_offered_agent_is_installable_elsewhere_and_an_operator_save_leaves_it_there() {
    let mut w = world().await;
    policy(&mut w, AppsPolicy::Off).await;
    w.operator
        .send(&ClientMessage::SaveServerAgent {
            pubkey: None,
            spec: agent_spec(false),
            api_key: None,
        })
        .await
        .unwrap();
    let pubkey = loop {
        if let ServerMessage::ServerAgents { agents } = next_timeout(&mut w.operator).await
            && let Some(a) = agents.first()
        {
            break a.pubkey.clone();
        }
    };

    w.admin
        .send(&install(w.admin_guild, &pubkey))
        .await
        .unwrap();
    assert!(next_error(&mut w.admin).await.contains("hasn't offered"));

    w.operator
        .send(&ClientMessage::SaveServerAgent {
            pubkey: Some(pubkey.clone()),
            spec: agent_spec(true),
            api_key: None,
        })
        .await
        .unwrap();
    // Another connection: the offer exists once the operator hears it saved.
    loop {
        if let ServerMessage::ServerAgents { agents } = next_timeout(&mut w.operator).await
            && agents.first().is_some_and(|a| a.spec.offered)
        {
            break;
        }
    }
    w.admin
        .send(&ClientMessage::FetchOfferedAgents)
        .await
        .unwrap();
    let offered = loop {
        if let ServerMessage::OfferedAgents { agents } = next_timeout(&mut w.admin).await {
            break agents;
        }
    };
    assert_eq!(offered.len(), 1);
    assert_eq!(offered[0].guild_daily_tokens, 1_000);

    w.admin
        .send(&install(w.admin_guild, &pubkey))
        .await
        .unwrap();
    assert!(
        installed(&mut w.admin, w.admin_guild, &pubkey).await,
        "offered beats the Off policy"
    );

    w.operator
        .send(&ClientMessage::SaveServerAgent {
            pubkey: Some(pubkey.clone()),
            spec: agent_spec(true),
            api_key: None,
        })
        .await
        .unwrap();
    w.admin
        .send(&ClientMessage::FetchIntegrations {
            guild_id: w.admin_guild,
        })
        .await
        .unwrap();
    assert!(
        installed(&mut w.admin, w.admin_guild, &pubkey).await,
        "the admin's own install stays"
    );
}
