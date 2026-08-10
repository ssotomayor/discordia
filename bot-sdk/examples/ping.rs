//! A minimal example bot.
//!
//! It replies "pong" to "!ping" and reacts 🎲 to "!roll". To run it:
//!
//! 1. Start a gateway (the desktop client's Self-host mode, or `cargo run -p
//!    dioxusfun-server`).
//! 2. `cargo run -p dioxusfun-bot --example ping`
//!    On first run it prints a freshly generated **pubkey** and **secret**.
//!    Save the secret as `BOT_SECRET` so the bot keeps its identity:
//!    `BOT_SECRET=<printed> cargo run -p dioxusfun-bot --example ping`
//! 3. In the app, as the guild owner, open Integrations and install the bot by
//!    its pubkey. Grant it **Send messages** + **Message content** (the latter
//!    is privileged — without it the bot can't read "!ping", which is the whole
//!    point of the demo) and, for the 🎲 reaction, **Add reactions**.
//!
//! Override the server with `SERVER_URL` (default `ws://localhost:9000`).

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
                // Ignore our own posts so we don't loop.
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
                        // Empty content means we were installed WITHOUT the
                        // privileged MessageContent intent — a useful signal.
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
