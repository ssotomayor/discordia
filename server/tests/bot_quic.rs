use std::time::Duration;

use discordia_bot::{Bot, BotError, BotIdentity};
use discordia_server::livekit::LiveKitConfig;
use discordia_server::protocol::{self, ServerMessage};
use discordia_server::quic::{self, Coordination, QuicHandle};

fn test_config() -> discordia_server::ServerConfig {
    let dir = std::env::temp_dir().join(format!(
        "discordia-bot-quic-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    discordia_server::ServerConfig {
        livekit: LiveKitConfig::from_env(&dir),
        operators: Default::default(),
        identities: Default::default(),
        media_max_bytes: discordia_server::media::DEFAULT_MAX_BYTES,
        data_dir: dir,
    }
}

/// `answers_to_key` off is a gateway whose identity set lacks `quic:<key>`.
async fn spawn_quic_gateway(
    coordination: Coordination,
    answers_to_key: bool,
) -> (
    iroh::EndpointId,
    Vec<String>,
    QuicHandle,
    discordia_server::GatewayShutdown,
) {
    let endpoint = quic::bind_quic(None, &coordination, quic::RANDOM_PORT)
        .await
        .expect("bind quic");
    let mut cfg = test_config();
    if answers_to_key {
        cfg.identities
            .insert(protocol::quic_origin(&endpoint.id().to_string()));
    }
    let addrs = match &coordination {
        Coordination::Relay(url) => {
            endpoint.online().await;
            vec![url.clone()]
        }
        Coordination::None => quic::dialable_addrs(&endpoint.bound_sockets())
            .iter()
            .map(ToString::to_string)
            .collect(),
    };
    let (router, monitor) = discordia_server::build_gateway(cfg).await.expect("router");
    let handle = quic::serve_on_with(endpoint, router, coordination).expect("serve quic");
    (handle.endpoint_id, addrs, handle, monitor)
}

async fn first_verdict(bot: &mut Bot) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(10), bot.next_event())
        .await
        .expect("timed out waiting for the gateway's verdict")
        .expect("connection closed before a verdict")
}

#[tokio::test]
async fn a_bot_joins_by_share_string_over_direct_quic() {
    let (key, addrs, handle, monitor) = spawn_quic_gateway(Coordination::None, true).await;
    let addrs: Vec<_> = addrs
        .into_iter()
        .filter(|addr| !addr.starts_with('['))
        .collect();
    let share = protocol::format_quic_share(&key.to_string(), &addrs);
    let identity = BotIdentity::generate();

    let mut bot = Bot::connect(&share, &identity, "QuicBot").await.unwrap();
    match first_verdict(&mut bot).await {
        ServerMessage::Ready { user, .. } => assert_eq!(user.pubkey, identity.pubkey()),
        other => panic!("expected Ready, got {other:?}"),
    }
    let routes = monitor.connections();
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].pubkey, identity.pubkey());
    assert_eq!(routes[0].name, "QuicBot");
    assert_eq!(
        routes[0].route,
        discordia_server::connection_routes::GatewayRoute::Ipv4
    );
    drop(bot);
    tokio::time::timeout(Duration::from_secs(2), async {
        while !monitor.connections().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("disconnected clients disappear from the snapshot");
    handle.shutdown().await;
}

#[tokio::test]
async fn a_host_reachable_only_through_its_relay_still_takes_the_bot() {
    tokio::time::timeout(Duration::from_secs(45), async {
        let relay = discordia_rendezvous::relay_server::spawn(
            "127.0.0.1:0".parse().unwrap(),
            Some("http://localhost/".into()),
        )
        .await
        .expect("relay");
        let url = format!("http://{}/", relay.http_addr().unwrap());
        let (key, addrs, handle, monitor) =
            spawn_quic_gateway(Coordination::Relay(url.clone()), true).await;
        assert_eq!(
            addrs,
            vec![url],
            "the share string names nothing but the relay"
        );
        let share = protocol::format_quic_share(&key.to_string(), &addrs);

        let mut bot = Bot::connect_as_user(&share, &BotIdentity::generate(), "relayed")
            .await
            .unwrap();
        assert!(matches!(
            first_verdict(&mut bot).await,
            ServerMessage::Ready { .. }
        ));
        let routes = monitor.connections();
        assert_eq!(routes.len(), 1);
        assert_ne!(
            routes[0].route,
            discordia_server::connection_routes::GatewayRoute::Unknown
        );
        drop(bot);
        handle.shutdown().await;
    })
    .await
    .expect("the relayed join did not finish within 45 seconds");
}

#[tokio::test]
async fn host_monitor_observes_a_direct_ipv6_gateway() {
    let (key, addrs, handle, monitor) = spawn_quic_gateway(Coordination::None, true).await;
    let addrs: Vec<_> = addrs
        .into_iter()
        .filter(|addr| addr.starts_with('['))
        .collect();
    if addrs.is_empty() {
        handle.shutdown().await;
        return;
    }
    let share = protocol::format_quic_share(&key.to_string(), &addrs);
    let mut bot = Bot::connect(&share, &BotIdentity::generate(), "IPv6Bot")
        .await
        .unwrap();
    assert!(matches!(
        first_verdict(&mut bot).await,
        ServerMessage::Ready { .. }
    ));
    let routes = monitor.connections();
    assert_eq!(routes.len(), 1);
    assert_eq!(
        routes[0].route,
        discordia_server::connection_routes::GatewayRoute::Ipv6
    );
    drop(bot);
    handle.shutdown().await;
}

#[tokio::test]
async fn the_signed_origin_is_the_key_so_a_host_that_does_not_answer_to_it_refuses() {
    let (key, addrs, handle, monitor) = spawn_quic_gateway(Coordination::None, false).await;
    let share = protocol::format_quic_share(&key.to_string(), &addrs);

    let mut bot = Bot::connect(&share, &BotIdentity::generate(), "QuicBot")
        .await
        .unwrap();
    match first_verdict(&mut bot).await {
        ServerMessage::Error { message } => assert!(
            message.contains("does not answer to"),
            "refused for the origin: {message}"
        ),
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert!(
        monitor.connections().is_empty(),
        "refused clients are not counted"
    );
    drop(bot);
    handle.shutdown().await;
}

#[tokio::test]
async fn a_wrong_key_in_the_share_string_is_a_quic_error() {
    let (_key, addrs, handle, _monitor) = spawn_quic_gateway(Coordination::None, true).await;
    let impostor = iroh::SecretKey::generate().public().to_string();
    let share = protocol::format_quic_share(&impostor, &addrs);

    match Bot::connect(&share, &BotIdentity::generate(), "QuicBot").await {
        Err(BotError::Quic(_)) => {}
        Err(other) => panic!("expected a QUIC error, got {other}"),
        Ok(_) => panic!("connected to a host whose key we got wrong"),
    }
    handle.shutdown().await;
}
