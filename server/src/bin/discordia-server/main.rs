mod archive;
mod options;
mod serve;
mod setup;

use std::path::{Path, PathBuf};

use clap::{CommandFactory, Parser, Subcommand};
use discordia_rendezvous::run::drop_empty_env;
use tracing_subscriber::EnvFilter;

use options::{ConfigFile, ServerOptions};

/// Where `setup` writes, and so where every other command looks when told nothing.
pub const SYSTEM_CONFIG: &str = "/etc/discordia/discordia.toml";

/// A Discordia server. Run with no command to serve; `sudo discordia-server setup` installs one.
#[derive(Parser)]
#[command(version, args_conflicts_with_subcommands = true)]
struct Cli {
    /// TOML file with [server] and [rendezvous] tables; flags and variables win over it
    /// [default: /etc/discordia/discordia.toml when it exists]
    #[arg(long, global = true, env = "DISCORDIA_CONFIG")]
    config: Option<PathBuf>,
    #[command(flatten)]
    server: ServerOptions,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Discovery and NAT relay for hosts behind NAT
    Rendezvous(Box<discordia_rendezvous::run::Options>),
    /// Install this machine as a server, a rendezvous or both: systemd, or --docker
    Setup(setup::SetupArgs),
    /// Print the quic:// address friends paste to join
    Share,
    /// Write guilds to JSON archives
    Export {
        /// One guild; without it, every guild into <OUT> as a directory
        #[arg(long)]
        guild: Option<uuid::Uuid>,
        out: PathBuf,
    },
    /// Read a guild archive in under fresh ids; pubkeys are kept
    Import { archive: PathBuf },
}

fn main() {
    drop_empty_env(&Cli::command());
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let config_path = cli.config.clone().or_else(|| {
        let system = PathBuf::from(SYSTEM_CONFIG);
        system.exists().then_some(system)
    });
    let file = match config_path.as_deref().map(ConfigFile::load).transpose() {
        Ok(file) => file.unwrap_or_default(),
        Err(e) => fail(2, &e),
    };

    if let Some(Command::Setup(args)) = cli.command {
        let target = cli.config.unwrap_or_else(|| args.default_config_path());
        if let Err(e) = setup::run(args, &target) {
            fail(1, &e);
        }
        return;
    }

    let server = cli.server.or(file.server.clone().unwrap_or_default());
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let result = runtime.block_on(async {
        match cli.command {
            None => {
                if let Some(path) = &config_path {
                    tracing::info!(path = %path.display(), "config file");
                }
                serve::serve(server).await
            }
            Some(Command::Rendezvous(opts)) => {
                discordia_rendezvous::run::run((*opts).or(file.rendezvous.unwrap_or_default()))
                    .await
            }
            Some(Command::Share) => share(&server).map(|s| println!("{s}")),
            Some(Command::Export { guild, out }) => {
                archive::export(&server.data_dir(), guild, &out).await;
                Ok(())
            }
            Some(Command::Import { archive }) => {
                archive::import(&server.data_dir(), &archive).await;
                Ok(())
            }
            Some(Command::Setup(_)) => unreachable!(),
        }
    });
    if let Err(e) = result {
        fail(1, &e);
    }
}

fn fail(code: i32, message: &str) -> ! {
    eprintln!("error: {message}");
    std::process::exit(code)
}

/// Read-only: a missing key means the server has not run yet, and minting one
/// here would be a key it never serves under.
pub fn share(opts: &ServerOptions) -> Result<String, String> {
    let data_dir = opts.data_dir();
    let key_file: &Path = &data_dir.join("quic-secret");
    if !key_file.exists() {
        return Err(format!(
            "no {} yet: start the server once first",
            key_file.display()
        ));
    }
    let port = opts
        .quic_port
        .unwrap_or(discordia_server::quic::DEFAULT_PORT);
    if port == discordia_server::quic::RANDOM_PORT {
        return Err(
            "quic_port is 0, a new port each start: the server logs the share string".into(),
        );
    }
    let secret = discordia_server::quic::persistent_secret(&data_dir).map_err(|e| e.to_string())?;
    let coordination = match opts.relay_url.as_deref().map(str::trim) {
        Some(url) if !url.is_empty() => discordia_server::quic::Coordination::Relay(url.into()),
        _ => discordia_server::quic::Coordination::None,
    };
    Ok(serve::share_string(
        &secret.public().to_string(),
        opts.public_address.as_deref(),
        port,
        &coordination,
    ))
}
