//! Wizards: a turn-based duel for two or three, installable as an app.
//!
//! `cargo run -p dioxusfun-bot --example wizards -- [SERVER_URL]`
//!
//! SERVER_URL defaults to `ws://127.0.0.1:9000`, a self-host on this machine;
//! elsewhere pass the host's `quic://…` share string. The key is kept in
//! `wizards.key` (or `WIZARDS_KEY_FILE`) so every guild pins the same app.

mod game;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use dioxusfun_bot::protocol::{
    ActivityCapability, ActivityInstance, ActivityManifest, ActivityRole, AppManifest, Id,
    ServerMessage,
};
use dioxusfun_bot::{Bot, BotIdentity};
use game::{Game, Phase, Spell};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Bump with every change to `index.html`: a host keeps a manifest only if it
/// is newer than the one it holds.
const VERSION: u64 = 1;
const BUNDLE: &str = include_str!("index.html");
const BUNDLE_URL: &str = "https://raw.githubusercontent.com/ssotomayor/discordia/master/bot-sdk/examples/wizards/index.html";
const TURN_LIMIT: Duration = Duration::from_secs(60);

fn manifest() -> AppManifest {
    AppManifest {
        name: "Wizards".into(),
        description: "A turn-based duel for 2–3 wizards: roll initiative, then fireball (40%, 30–40) or freeze (60%, 15–22) until one stands.".into(),
        icon: "🧙".into(),
        version: VERSION,
        permissions: Vec::new(),
        intents: Vec::new(),
        commands: Vec::new(),
        activity: Some(ActivityManifest {
            sha256: hex::encode(Sha256::digest(BUNDLE.as_bytes())),
            capabilities: vec![ActivityCapability::Instance],
            max_players: game::MAX_PLAYERS as u8,
            url: Some(BUNDLE_URL.into()),
        }),
    }
}

fn identity() -> Result<BotIdentity, Box<dyn std::error::Error>> {
    let path = std::env::var("WIZARDS_KEY_FILE").unwrap_or_else(|_| "wizards.key".into());
    if let Ok(secret) = std::fs::read_to_string(&path) {
        return Ok(BotIdentity::from_base58_secret(secret.trim())?);
    }
    let id = BotIdentity::generate();
    std::fs::write(&path, id.secret_base58())?;
    eprintln!("Made a new key and saved it to {path}. Keep it: guilds install this key.");
    Ok(id)
}

struct Table {
    channel_id: Id,
    game: Game,
    players: Vec<String>,
    turn_started: Instant,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("SERVER_URL").ok())
        .unwrap_or_else(|| "ws://127.0.0.1:9000".into());
    let id = identity()?;
    let m = manifest();
    eprintln!("Wizards v{VERSION}");
    eprintln!(
        "  app key (paste into Integrations → Install an app): {}",
        id.pubkey()
    );
    eprintln!(
        "  the activity reaches the host by itself; its sha256 is {}",
        m.activity.as_ref().map(|a| a.sha256.as_str()).unwrap_or("")
    );
    loop {
        match run(&server, &id).await {
            Ok(()) => eprintln!("the host closed the connection; reconnecting in 5 s"),
            Err(e) => eprintln!("{e}; reconnecting in 5 s"),
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn run(server: &str, id: &BotIdentity) -> Result<(), Box<dyn std::error::Error>> {
    let mut bot = Bot::connect(server, id, "Wizards").await?;
    bot.register_manifest(id, &manifest()).await?;
    eprintln!("connected to {server}");
    let mut tables: HashMap<Id, Table> = HashMap::new();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            event = bot.next_event() => match event {
                None => return Ok(()),
                Some(ServerMessage::ActivityInstances { channel_id, instances }) => {
                    for changed in sync(&mut tables, channel_id, instances) {
                        broadcast(&mut bot, &tables, changed).await?;
                    }
                }
                Some(ServerMessage::ActivityEvent { instance_id, from, data }) => {
                    act(&mut bot, &mut tables, instance_id, &from, &data).await?;
                }
                Some(ServerMessage::BundleWanted { sha256 }) => {
                    if manifest().activity.is_some_and(|a| a.sha256 == sha256) {
                        bot.offer_bundle(BUNDLE.as_bytes()).await?;
                        eprintln!("sent the activity to the host");
                    }
                }
                Some(ServerMessage::Error { message }) => eprintln!("host: {message}"),
                Some(_) => {}
            },
            _ = tick.tick() => {
                let stalled: Vec<Id> = tables
                    .iter()
                    .filter(|(_, t)| t.game.phase == Phase::Playing && t.turn_started.elapsed() >= TURN_LIMIT)
                    .map(|(id, _)| *id)
                    .collect();
                for instance_id in stalled {
                    if let Some(t) = tables.get_mut(&instance_id) {
                        t.game.skip();
                        t.turn_started = Instant::now();
                    }
                    broadcast(&mut bot, &tables, instance_id).await?;
                }
            }
        }
    }
}

/// The host sends every instance of this app in a channel; one missing from
/// the list has ended, and a player missing from one has left.
fn sync(
    tables: &mut HashMap<Id, Table>,
    channel_id: Id,
    instances: Vec<ActivityInstance>,
) -> Vec<Id> {
    tables.retain(|id, t| t.channel_id != channel_id || instances.iter().any(|i| i.id == *id));
    let mut changed = Vec::new();
    for instance in instances {
        let players: Vec<String> = instance
            .participants
            .iter()
            .filter(|p| p.role == ActivityRole::Player)
            .map(|p| p.pubkey.clone())
            .collect();
        let table = tables.entry(instance.id).or_insert_with(|| Table {
            channel_id,
            game: Game::default(),
            players: Vec::new(),
            turn_started: Instant::now(),
        });
        let before = table.game.current().map(str::to_string);
        let gone: Vec<String> = table
            .game
            .wizards
            .iter()
            .map(|w| w.key.clone())
            .filter(|k| !players.contains(k))
            .collect();
        for key in gone {
            table.game.forfeit(&key);
        }
        if table.game.current().map(str::to_string) != before {
            table.turn_started = Instant::now();
        }
        table.players = players;
        changed.push(instance.id);
    }
    changed
}

async fn act(
    bot: &mut Bot,
    tables: &mut HashMap<Id, Table>,
    instance_id: Id,
    from: &str,
    data: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(table) = tables.get_mut(&instance_id) else {
        return Ok(());
    };
    let msg: Value = serde_json::from_str(data).unwrap_or(Value::Null);
    let mut rng = rand::thread_rng();
    let outcome = match msg["t"].as_str() {
        Some("start") if table.players.iter().any(|p| p == from) => {
            table.game.start(&table.players, &mut rng)
        }
        Some("start") => Err("only a player starts the duel".into()),
        Some("attack") => match Spell::parse(msg["spell"].as_str().unwrap_or("")) {
            Some(spell) => {
                table
                    .game
                    .attack(from, spell, msg["target"].as_str().unwrap_or(""), &mut rng)
            }
            None => Err("unknown spell".into()),
        },
        Some("hello") => {
            let state = table.game.state().to_string();
            bot.activity_send(instance_id, Some(from), &state).await?;
            return Ok(());
        }
        _ => return Ok(()),
    };
    match outcome {
        Ok(()) => {
            table.turn_started = Instant::now();
            broadcast(bot, tables, instance_id).await
        }
        Err(message) => {
            let reply = json!({ "t": "error", "message": message }).to_string();
            bot.activity_send(instance_id, Some(from), &reply).await?;
            Ok(())
        }
    }
}

async fn broadcast(
    bot: &mut Bot,
    tables: &HashMap<Id, Table>,
    instance_id: Id,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(t) = tables.get(&instance_id) {
        bot.activity_send(instance_id, None, &t.game.state().to_string())
            .await?;
    }
    Ok(())
}
