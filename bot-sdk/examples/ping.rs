use dioxusfun_bot::protocol::ServerMessage;
use dioxusfun_bot::{Bot, BotIdentity};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server = std::env::var("SERVER_URL").unwrap_or_else(|_| "ws://localhost:9000".into());

    let identity = match std::env::var("BOT_SECRET") {
        Ok(secret) => BotIdentity::from_base58_secret(&secret)?,
        Err(_) => {
            let id = BotIdentity::generate();
            eprintln!("── No BOT_SECRET set; generated a new identity ──");
            eprintln!("  pubkey (install this in the app): {}", id.pubkey());
            eprintln!(
                "  secret (save as BOT_SECRET):       {}",
                id.secret_base58()
            );
            eprintln!("─────────────────────────────────────────────────");
            id
        }
    };

    eprintln!("connecting to {server} as PingBot ({})", identity.pubkey());
    let mut bot = Bot::connect(&server, &identity, "PingBot").await?;

    while let Some(event) = bot.next_event().await {
        match event {
            ServerMessage::Ready { guilds, .. } => {
                if guilds.is_empty() {
                    eprintln!(
                        "connected, but not installed in any guild yet — install {} as the guild owner",
                        identity.pubkey()
                    );
                } else {
                    let names: Vec<_> = guilds.iter().map(|g| g.name.as_str()).collect();
                    eprintln!("ready — active in: {}", names.join(", "));
                }
            }
            ServerMessage::MessageCreate(msg) => {
                if msg.author.pubkey == bot.user.pubkey {
                    continue;
                }
                match msg.content.trim() {
                    "!ping" => {
                        eprintln!("!ping from {} — replying", msg.author.username);
                        bot.send_message(msg.channel_id, "pong 🏓").await?;
                    }
                    "!roll" => {
                        bot.react(msg.channel_id, msg.id, "🎲").await?;
                    }
                    "" => {
                        eprintln!(
                            "saw a message but its content was withheld (grant the Message content intent)"
                        );
                    }
                    _ => {}
                }
            }
            ServerMessage::Error { message } => eprintln!("server error: {message}"),
            _ => {}
        }
    }

    eprintln!("disconnected");
    Ok(())
}
