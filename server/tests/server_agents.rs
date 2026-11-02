use std::collections::HashSet;
use std::net::SocketAddr;
use std::time::Duration;

use discordia_bot::{Bot, BotIdentity};
use discordia_server::livekit::LiveKitConfig;
use discordia_server::protocol::{
    AgentProvider, ChannelKind, ClientMessage, Id, ServerAgentSpec, ServerAgentView, ServerMessage,
};

async fn next_timeout(session: &mut Bot) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(10), session.next_event())
        .await
        .expect("timed out waiting for a gateway event")
        .expect("connection closed unexpectedly")
}

/// An OpenAI-compatible provider on loopback that always says the same thing.
async fn mock_provider() -> String {
    use axum::{Json, Router, routing::post};
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            Json(serde_json::json!({
                "choices": [{ "message": { "content": "pong from the mock" }, "finish_reason": "stop" }],
                "usage": { "total_tokens": 5 }
            }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}/v1")
}

const SECRET_KEY: &str = "gsk-never-shown-1234";

struct World {
    handle: discordia_server::ServerHandle,
    dir: std::path::PathBuf,
    operator: Bot,
    operator_id: BotIdentity,
    member: Bot,
    guild_id: Id,
    text: Id,
    provider: String,
}

fn config(dir: &std::path::Path, operator: &str) -> discordia_server::ServerConfig {
    discordia_server::ServerConfig {
        livekit: LiveKitConfig::from_env(dir),
        operators: HashSet::from([operator.to_string()]),
        identities: Default::default(),
        media_max_bytes: discordia_server::media::DEFAULT_MAX_BYTES,
        data_dir: dir.to_path_buf(),
        admission: Default::default(),
    }
}

async fn connect(url: &str, id: &BotIdentity, name: &str) -> Bot {
    let mut b = Bot::connect_as_user(url, id, name).await.unwrap();
    while !matches!(next_timeout(&mut b).await, ServerMessage::Ready { .. }) {}
    b
}

async fn world() -> World {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "discordia-server-agents-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let operator_id = BotIdentity::generate();
    let preferred: SocketAddr = "127.0.0.1:19700".parse().unwrap();
    let handle = discordia_server::spawn(preferred, 100, config(&dir, operator_id.pubkey()))
        .await
        .expect("spawn server");
    let url = format!("ws://{}", handle.addr);
    let mut operator = connect(&url, &operator_id, "Operator").await;
    operator
        .send(&ClientMessage::CreateGuild {
            name: "Ops".into(),
            template: None,
        })
        .await
        .unwrap();
    let (guild_id, text) = loop {
        if let ServerMessage::GuildJoined {
            guild, channels, ..
        } = next_timeout(&mut operator).await
        {
            let text = channels
                .iter()
                .find(|c| c.kind == ChannelKind::Text)
                .unwrap()
                .id;
            break (guild.id, text);
        }
    };
    let mut member = connect(&url, &BotIdentity::generate(), "Member").await;
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
    World {
        handle,
        dir,
        operator,
        operator_id,
        member,
        guild_id,
        text,
        provider: mock_provider().await,
    }
}

fn spec(w: &World, guilds: Vec<Id>) -> ServerAgentSpec {
    ServerAgentSpec {
        name: "Helper".into(),
        prompt: "Be brief.".into(),
        provider: AgentProvider::OpenAiCompatible,
        provider_label: "Groq".into(),
        base_url: w.provider.clone(),
        model: "openai/gpt-oss-20b".into(),
        per_member_per_hour: 20,
        daily_tokens: 100_000,
        guilds,
        offered: false,
        guild_daily_tokens: 0,
    }
}

async fn agents_until(
    session: &mut Bot,
    done: impl Fn(&[ServerAgentView]) -> bool,
) -> Vec<ServerAgentView> {
    loop {
        if let ServerMessage::ServerAgents { agents } = next_timeout(session).await
            && done(&agents)
        {
            return agents;
        }
    }
}

#[tokio::test]
async fn only_the_operator_may_run_an_agent_in_the_server() {
    let mut w = world().await;
    let spec = spec(&w, vec![]);
    w.member
        .send(&ClientMessage::SaveServerAgent {
            pubkey: None,
            spec,
            api_key: Some(SECRET_KEY.into()),
        })
        .await
        .unwrap();
    let refused = loop {
        if let ServerMessage::Error { message } = next_timeout(&mut w.member).await {
            break message;
        }
    };
    assert!(refused.contains("operator"), "{refused}");
    w.member
        .send(&ClientMessage::FetchServerAgents)
        .await
        .unwrap();
    let refused = loop {
        if let ServerMessage::Error { message } = next_timeout(&mut w.member).await {
            break message;
        }
    };
    assert!(refused.contains("operator"), "{refused}");
}

#[tokio::test]
async fn an_operator_agent_joins_its_guild_answers_and_never_shows_its_key() {
    let mut w = world().await;
    let spec = spec(&w, vec![w.guild_id]);
    w.operator
        .send(&ClientMessage::SaveServerAgent {
            pubkey: None,
            spec,
            api_key: Some(SECRET_KEY.into()),
        })
        .await
        .unwrap();
    let agents = agents_until(&mut w.operator, |a| a.first().is_some_and(|a| a.online)).await;
    let agent = &agents[0];
    assert_eq!(agent.key_hint, "1234");
    assert!(!serde_json::to_string(&agents).unwrap().contains(SECRET_KEY));

    let seat = loop {
        if let ServerMessage::MemberJoin(m) = next_timeout(&mut w.member).await
            && m.user.pubkey == agent.pubkey
        {
            break m;
        }
    };
    assert_eq!(
        seat.agent.as_ref().map(|a| a.provider.as_str()),
        Some("Groq")
    );

    // Commands arrive once the agent has its Ready; then it listens.
    while !matches!(
        next_timeout(&mut w.member).await,
        ServerMessage::BotCommands(_)
    ) {}
    w.member.send_message(w.text, "@Helper ping").await.unwrap();
    let reply = loop {
        if let ServerMessage::MessageCreate(m) = next_timeout(&mut w.member).await
            && m.author.pubkey == agent.pubkey
        {
            break m;
        }
    };
    assert_eq!(reply.content, "pong from the mock");
}

#[tokio::test]
async fn deleting_an_agent_takes_its_seat_and_a_restart_brings_the_rest_back() {
    let mut w = world().await;
    let mut kept = spec(&w, vec![]);
    kept.name = "Kept".into();
    w.operator
        .send(&ClientMessage::SaveServerAgent {
            pubkey: None,
            spec: kept,
            api_key: Some(SECRET_KEY.into()),
        })
        .await
        .unwrap();
    agents_until(&mut w.operator, |a| a.len() == 1).await;
    let gone = spec(&w, vec![w.guild_id]);
    w.operator
        .send(&ClientMessage::SaveServerAgent {
            pubkey: None,
            spec: gone,
            api_key: Some(SECRET_KEY.into()),
        })
        .await
        .unwrap();
    let agents = agents_until(&mut w.operator, |a| a.len() == 2).await;
    let helper = agents
        .iter()
        .find(|a| a.spec.name == "Helper")
        .unwrap()
        .pubkey
        .clone();

    w.operator
        .send(&ClientMessage::DeleteServerAgent {
            pubkey: helper.clone(),
        })
        .await
        .unwrap();
    agents_until(&mut w.operator, |a| a.len() == 1).await;
    loop {
        if let ServerMessage::MemberRemove { user_pubkey, .. } = next_timeout(&mut w.member).await
            && user_pubkey == helper
        {
            break;
        }
    }

    w.handle.abort();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let handle = discordia_server::spawn(
        "127.0.0.1:19700".parse().unwrap(),
        100,
        config(&w.dir, w.operator_id.pubkey()),
    )
    .await
    .expect("respawn server");
    let url = format!("ws://{}", handle.addr);
    let mut operator = connect(&url, &w.operator_id, "Operator").await;
    operator
        .send(&ClientMessage::FetchServerAgents)
        .await
        .unwrap();
    let agents = agents_until(&mut operator, |_| true).await;
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].spec.name, "Kept");
    assert_eq!(agents[0].key_hint, "1234");
}
