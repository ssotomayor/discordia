use std::path::PathBuf;

use clap::{CommandFactory, Parser};
use discordia_rendezvous::run::{Options, drop_empty_env, run};
use tracing_subscriber::EnvFilter;

/// Discovery and NAT relay for Discordia hosts. `discordia-server rendezvous` is the same thing.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// TOML file; its `[rendezvous]` table fills what flags and variables leave unset
    #[arg(long, env = "DISCORDIA_CONFIG")]
    config: Option<PathBuf>,
    #[command(flatten)]
    opts: Options,
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
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    if let Err(e) = runtime.block_on(run(cli.opts.or(file))) {
        tracing::error!("{e}");
        std::process::exit(1);
    }
}
