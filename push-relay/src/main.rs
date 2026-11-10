use std::path::PathBuf;

use clap::{CommandFactory, Parser, Subcommand};
use discordia_push_relay::run::{Options, drop_empty_env, ping, run};
use tracing_subscriber::EnvFilter;

/// Holds the FCM credential and forwards opaque pushes to phones.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// TOML file; its `[push_relay]` table fills what flags and variables leave unset
    #[arg(long, env = "DISCORDIA_CONFIG")]
    config: Option<PathBuf>,
    #[command(flatten)]
    opts: Options,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Send a test push (`t = ping`) to every device of a key, through the running relay's database
    Ping { pubkey: String },
}

fn main() {
    drop_empty_env(&Cli::command());
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let file = match cli.config.as_deref().map(Options::from_file).transpose() {
        Ok(file) => file.unwrap_or_default(),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    };
    let opts = cli.opts.or(file);
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: tokio runtime: {e}");
            std::process::exit(1);
        }
    };
    let result = match cli.command {
        None => runtime.block_on(run(opts)),
        Some(Command::Ping { pubkey }) => runtime.block_on(ping(opts, &pubkey)),
    };
    if let Err(e) = result {
        tracing::error!("{e}");
        std::process::exit(1);
    }
}
