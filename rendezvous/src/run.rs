use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::registry::{self, Registry};
use crate::{AppCtx, Config, relay_server, router, turn_relay};

/// One setting, three spellings: `--flag`, the variable beside it, and the key in a
/// config file's `[rendezvous]` table — in that order of precedence.
#[derive(Debug, Default, Clone, PartialEq, clap::Args, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Options {
    /// HTTP bind: /control, /discover, /resolve, /config, /voice-token, /voice-evict [default: 0.0.0.0:7700]
    #[arg(long, env = "DISCORDIA_RENDEZVOUS_ADDR")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub addr: Option<SocketAddr>,
    /// Persisted name reservations [default: ./rendezvous-data]
    #[arg(long, env = "DISCORDIA_RENDEZVOUS_DATA_DIR")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_dir: Option<PathBuf>,
    /// iroh relay bind, carrying ciphertext for hosts behind NAT [default: 0.0.0.0:7701]
    #[arg(long, env = "DISCORDIA_RENDEZVOUS_RELAY_ADDR")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relay_addr: Option<SocketAddr>,
    /// How clients reach that relay, e.g. http://rendezvous.example:7701; unset runs no relay
    #[arg(long, env = "DISCORDIA_RENDEZVOUS_RELAY_URL")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relay_url: Option<String>,
    /// Enables the TURN relay: what clients dial, e.g. turn:rendezvous.example:7702
    #[arg(long, env = "DISCORDIA_RENDEZVOUS_TURN_URL")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_url: Option<String>,
    /// UDP bind for the TURN control socket [default: 0.0.0.0:7702]
    #[arg(long, env = "DISCORDIA_RENDEZVOUS_TURN_ADDR")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_addr: Option<SocketAddr>,
    /// UDP range relayed media is allocated from, low-high [default: 7710-7809]
    #[arg(long, env = "DISCORDIA_RENDEZVOUS_TURN_PORTS")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_ports: Option<String>,
    /// Public IPv4 handed out in allocations [default: resolved from the TURN URL]
    #[arg(long, env = "DISCORDIA_RENDEZVOUS_TURN_RELAY_IP")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_relay_ip: Option<IpAddr>,
    /// HMAC secret behind TURN credentials [default: random per start]
    #[arg(long, env = "DISCORDIA_RENDEZVOUS_TURN_SECRET", hide_env_values = true)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_secret: Option<String>,
    /// Comma-separated host names (or URLs) hosts dial this rendezvous by; loopback and interface addresses are always included
    #[arg(long, env = "DISCORDIA_RENDEZVOUS_PUBLIC_HOSTS")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_hosts: Option<String>,
    /// Shared SFU for hosts nobody can reach and that have no relay
    #[arg(long, env = "LIVEKIT_URL")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub livekit_url: Option<String>,
    #[arg(long, env = "LIVEKIT_API_KEY")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub livekit_api_key: Option<String>,
    #[arg(long, env = "LIVEKIT_API_SECRET", hide_env_values = true)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub livekit_api_secret: Option<String>,
}

impl Options {
    /// `self` wins; `file` fills only what `self` leaves unset.
    pub fn or(self, file: Options) -> Options {
        Options {
            addr: self.addr.or(file.addr),
            data_dir: self.data_dir.or(file.data_dir),
            relay_addr: self.relay_addr.or(file.relay_addr),
            relay_url: self.relay_url.or(file.relay_url),
            turn_url: self.turn_url.or(file.turn_url),
            turn_addr: self.turn_addr.or(file.turn_addr),
            turn_ports: self.turn_ports.or(file.turn_ports),
            turn_relay_ip: self.turn_relay_ip.or(file.turn_relay_ip),
            turn_secret: self.turn_secret.or(file.turn_secret),
            public_hosts: self.public_hosts.or(file.public_hosts),
            livekit_url: self.livekit_url.or(file.livekit_url),
            livekit_api_key: self.livekit_api_key.or(file.livekit_api_key),
            livekit_api_secret: self.livekit_api_secret.or(file.livekit_api_secret),
        }
    }

    /// The `[rendezvous]` table of a config file; other tables are another role's.
    pub fn from_file(path: &Path) -> Result<Options, String> {
        #[derive(Deserialize)]
        struct File {
            #[serde(default)]
            rendezvous: Options,
        }
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        toml::from_str::<File>(&text)
            .map(|f| f.rendezvous)
            .map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// Unset and empty read the same, so a compose file may pass `${VAR:-}`.
fn set(value: Option<String>) -> Option<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

pub async fn run(opts: Options) -> Result<(), String> {
    // 7700, not 7000: macOS reserves 7000 for AirPlay Receiver.
    let addr = opts.addr.unwrap_or(([0, 0, 0, 0], 7700).into());
    let relay_url = set(opts.relay_url);
    let relay_bind = opts
        .relay_addr
        .unwrap_or(([0, 0, 0, 0], relay_server::DEFAULT_RELAY_PORT).into());
    let _relay = relay_server::spawn(relay_bind, relay_url.clone()).await;

    let (turn, _turn_server) = match set(opts.turn_url) {
        Some(url) => {
            let bind = opts
                .turn_addr
                .unwrap_or(([0, 0, 0, 0], turn_relay::DEFAULT_PORT).into());
            let relay_ip = match opts.turn_relay_ip {
                Some(ip) => ip,
                None => turn_relay::resolve_relay_ip(&url).await.map_err(|e| {
                    format!("the TURN URL must name this machine's public address: {e}")
                })?,
            };
            // Credentials are minted per registration and renewed over the control
            // stream, so a secret that changes with the process costs nothing.
            let secret =
                set(opts.turn_secret).unwrap_or_else(|| hex::encode(rand::random::<[u8; 32]>()));
            let ports = match set(opts.turn_ports) {
                Some(spec) => turn_relay::parse_ports(&spec)?,
                None => turn_relay::DEFAULT_PORTS,
            };
            let issuer = turn_relay::Issuer::new(vec![url], secret);
            match turn_relay::spawn(bind, relay_ip, ports, &issuer).await {
                Ok(server) => (Some(issuer), Some(server)),
                Err(e) => {
                    tracing::error!(%e, "turn relay not started; hosts behind NAT fall back to the shared SFU");
                    (None, None)
                }
            }
        }
        None => (None, None),
    };

    let mut audiences = crate::local_audiences();
    for host in set(opts.public_hosts).iter().flat_map(|l| l.split(',')) {
        let host = host.trim();
        let parsed = if host.contains("://") {
            discordia_protocol::rendezvous::rendezvous_audience(host)
        } else {
            discordia_protocol::rendezvous::rendezvous_audience(&format!("ws://{host}"))
        };
        match parsed {
            Some(a) => {
                audiences.insert(a);
            }
            None if host.is_empty() => {}
            None => return Err(format!("not a host name: {host}")),
        }
    }

    let config = Config {
        relay_url,
        audiences,
        livekit_url: set(opts.livekit_url),
        livekit_api_key: set(opts.livekit_api_key),
        livekit_api_secret: set(opts.livekit_api_secret),
        turn,
        ..Config::default()
    };
    tracing::info!(
        ?config.relay_url,
        ?config.livekit_url,
        shared_credentials = config.livekit_api_secret.is_some(),
        turn = config.turn.as_ref().map(|t| t.urls().to_vec()).unwrap_or_default().join(","),
        audiences = ?config.audiences,
        "rendezvous configured"
    );

    let data_dir = opts
        .data_dir
        .unwrap_or_else(|| PathBuf::from("./rendezvous-data"));
    let ctx = AppCtx {
        registry: Arc::new(Registry::open(&data_dir.join("reservations.db"))?),
        config: Arc::new(config),
    };
    let sweeper = ctx.registry.clone();
    tokio::spawn(async move {
        let mut every = tokio::time::interval(std::time::Duration::from_secs(60 * 60));
        loop {
            every.tick().await;
            sweeper.sweep(registry::now_unix());
        }
    });

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("cannot bind {addr}: {e}"))?;
    tracing::info!(%addr, "discordia-rendezvous listening");
    axum::serve(
        listener,
        router(ctx).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .map_err(|e| e.to_string())
}

/// Compose passes `${VAR:-}` for every knob, and clap parses an empty variable
/// as a value. Must run before any thread exists: it removes variables.
pub fn drop_empty_env(cmd: &clap::Command) {
    for arg in cmd.get_arguments() {
        if let Some(name) = arg.get_env()
            && std::env::var_os(name).is_some_and(|v| v.to_string_lossy().trim().is_empty())
        {
            // SAFETY: the caller is single-threaded, so nothing reads the environment concurrently.
            unsafe { std::env::remove_var(name) };
        }
    }
    for sub in cmd.get_subcommands() {
        drop_empty_env(sub);
    }
}

/// SIGTERM too: it is what systemd and `docker stop` send.
pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await.ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn earlier_source_wins_and_later_fills_gaps() {
        let flag = Options {
            relay_url: Some("http://flag:7701".into()),
            ..Options::default()
        };
        let file = Options {
            relay_url: Some("http://file:7701".into()),
            turn_ports: Some("8000-8010".into()),
            ..Options::default()
        };
        let merged = flag.or(file);
        assert_eq!(merged.relay_url.as_deref(), Some("http://flag:7701"));
        assert_eq!(merged.turn_ports.as_deref(), Some("8000-8010"));
    }

    #[test]
    fn a_misspelt_key_is_refused_and_other_tables_ignored() {
        let dir = std::env::temp_dir().join(format!("rv-opts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("discordia.toml");
        std::fs::write(
            &path,
            "[server]\nquic_port = 9001\n[rendezvous]\nturn_url = \"turn:x:7702\"\n",
        )
        .unwrap();
        assert_eq!(
            Options::from_file(&path).unwrap().turn_url.as_deref(),
            Some("turn:x:7702")
        );
        std::fs::write(&path, "[rendezvous]\nturn_ulr = \"turn:x:7702\"\n").unwrap();
        assert!(Options::from_file(&path).unwrap_err().contains("turn_ulr"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
