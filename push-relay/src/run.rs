use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::fcm::FcmSender;
use crate::limits::Limits;
use crate::sender::{RecordingSender, Sender};
use crate::store::{Caps, Store};
use crate::{Config, Relay, local_audiences, router};

/// One setting, three spellings: `--flag`, the variable beside it, and the key in a
/// config file's `[push_relay]` table — in that order of precedence.
#[derive(Debug, Default, Clone, PartialEq, clap::Args, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Options {
    /// HTTP bind, behind a TLS proxy [default: 127.0.0.1:7720]
    #[arg(long, env = "DISCORDIA_PUSH_ADDR")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub addr: Option<SocketAddr>,
    /// Devices and handles, `push.db` (SQLite) [default: ./push-relay-data]
    #[arg(long, env = "DISCORDIA_PUSH_DATA_DIR")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_dir: Option<PathBuf>,
    /// Comma-separated host names (or URLs) phones dial this relay by; loopback always counts
    #[arg(long, env = "DISCORDIA_PUSH_PUBLIC_HOSTS")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_hosts: Option<String>,
    /// Peers whose X-Forwarded-For names the client, such as the TLS proxy's address; nobody's by default
    #[arg(long, env = "DISCORDIA_PUSH_TRUSTED_PROXIES", value_delimiter = ',')]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trusted_proxies: Option<Vec<IpAddr>>,
    /// Service-account JSON for FCM; unset, sends are logged instead of made
    #[arg(long, env = "DISCORDIA_PUSH_FCM_CREDENTIALS")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fcm_credentials: Option<PathBuf>,
    /// Comma-separated relays the DM watcher may dial [default: its built-in list]
    #[arg(long, env = "DISCORDIA_PUSH_DM_RELAYS")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dm_relays: Option<String>,
}

impl Options {
    /// `self` wins; `file` fills only what `self` leaves unset.
    pub fn or(self, file: Options) -> Options {
        Options {
            addr: self.addr.or(file.addr),
            data_dir: self.data_dir.or(file.data_dir),
            public_hosts: self.public_hosts.or(file.public_hosts),
            trusted_proxies: self.trusted_proxies.or(file.trusted_proxies),
            fcm_credentials: self.fcm_credentials.or(file.fcm_credentials),
            dm_relays: self.dm_relays.or(file.dm_relays),
        }
    }

    /// The `[push_relay]` table of a config file; other tables are another role's.
    pub fn from_file(path: &Path) -> Result<Options, String> {
        #[derive(Deserialize)]
        struct File {
            #[serde(default)]
            push_relay: Options,
        }
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        toml::from_str::<File>(&text)
            .map(|f| f.push_relay)
            .map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from("./push-relay-data"))
            .join("push.db")
    }

    pub fn audiences(&self) -> Result<std::collections::HashSet<String>, String> {
        let mut audiences = local_audiences();
        for host in self.public_hosts.iter().flat_map(|l| l.split(',')) {
            let host = host.trim();
            if host.is_empty() {
                continue;
            }
            let url = if host.contains("://") {
                host.to_string()
            } else {
                format!("https://{host}")
            };
            let audience = discordia_protocol::push::push_audience(&url)
                .ok_or_else(|| format!("not a host name: {host}"))?;
            audiences.insert(audience);
        }
        Ok(audiences)
    }

    pub fn dm_relays(&self) -> Result<Option<Vec<String>>, String> {
        let Some(list) = self.dm_relays.as_deref().filter(|l| !l.trim().is_empty()) else {
            return Ok(None);
        };
        list.split(',')
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .map(|r| {
                if r.starts_with("wss://") || r.starts_with("ws://") {
                    Ok(r.to_string())
                } else {
                    Err(format!("not a relay URL: {r}"))
                }
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some)
    }

    /// The real sender when credentials are configured, else the logging one.
    pub fn sender(&self) -> Result<(Arc<dyn Sender>, bool), String> {
        match &self.fcm_credentials {
            Some(path) if !path.as_os_str().is_empty() => {
                let fcm = FcmSender::from_file(path)?;
                tracing::info!(project = fcm.project_id(), "FCM credentials loaded");
                Ok((Arc::new(fcm), true))
            }
            _ => {
                tracing::warn!(
                    "no FCM credentials: registrations are accepted and sends only logged"
                );
                Ok((Arc::new(RecordingSender::default()), false))
            }
        }
    }
}

pub async fn run(opts: Options) -> Result<(), String> {
    let addr = opts.addr.unwrap_or(([127, 0, 0, 1], 7720).into());
    let (sender, live) = opts.sender()?;
    let config = Config {
        audiences: opts.audiences()?,
        trusted_proxies: opts.trusted_proxies.clone().unwrap_or_default(),
        dm_relays: opts.dm_relays()?,
        validate_tokens: live,
        ..Config::default()
    };
    tracing::info!(
        audiences = ?config.audiences,
        trusted_proxies = ?config.trusted_proxies,
        dm_relays = ?config.dm_relays,
        "push relay configured"
    );
    let store = Store::open(&opts.db_path(), Caps::default())?;
    tracing::info!(
        devices = store.device_count().map_err(|e| format!("{e:?}"))?,
        "devices loaded"
    );
    let relay = Arc::new(Relay::new(store, sender, config, Limits::default()));

    let sweeper = relay.clone();
    tokio::spawn(async move {
        let mut every = tokio::time::interval(std::time::Duration::from_secs(60 * 60));
        loop {
            every.tick().await;
            match sweeper.store.sweep(crate::now_unix()) {
                Ok(0) => {}
                Ok(n) => tracing::info!(n, "expired devices swept"),
                Err(e) => tracing::error!(?e, "sweep failed"),
            }
        }
    });

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("cannot bind {addr}: {e}"))?;
    tracing::info!(%addr, "discordia-push-relay listening");
    axum::serve(
        listener,
        router(relay).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .map_err(|e| e.to_string())
}

/// Opens the running relay's database beside it: SQLite in WAL mode with a
/// busy timeout serialises the one write a ping may make (a dead token's row).
pub async fn ping(opts: Options, pubkey: &str) -> Result<(), String> {
    if !discordia_protocol::is_canonical_pubkey(pubkey) {
        return Err("the key must be 64 lowercase hex characters".into());
    }
    let (sender, live) = opts.sender()?;
    let store = Store::open(&opts.db_path(), Caps::default())?;
    let relay = Relay::new(store, sender, Config::default(), Limits::default());
    let results = relay.ping(pubkey).await?;
    if results.is_empty() {
        return Err("no unexpired device is registered for that key".into());
    }
    for (i, r) in results.iter().enumerate() {
        println!(
            "device {}: {r:?}{}",
            i + 1,
            if live { "" } else { " (logged only)" }
        );
    }
    Ok(())
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
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(_) => {
                tokio::signal::ctrl_c().await.ok();
            }
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
            public_hosts: Some("flag.example".into()),
            ..Options::default()
        };
        let file = Options {
            public_hosts: Some("file.example".into()),
            dm_relays: Some("wss://r.example".into()),
            ..Options::default()
        };
        let merged = flag.or(file);
        assert_eq!(merged.public_hosts.as_deref(), Some("flag.example"));
        assert_eq!(merged.dm_relays.as_deref(), Some("wss://r.example"));
    }

    #[test]
    fn a_misspelt_key_is_refused_and_other_tables_ignored() {
        let dir = std::env::temp_dir().join(format!("push-opts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("discordia.toml");
        std::fs::write(
            &path,
            "[server]\nquic_port = 9001\n[push_relay]\npublic_hosts = \"push.example\"\n",
        )
        .unwrap();
        assert_eq!(
            Options::from_file(&path).unwrap().public_hosts.as_deref(),
            Some("push.example")
        );
        std::fs::write(&path, "[push_relay]\npublic_host = \"x\"\n").unwrap();
        assert!(
            Options::from_file(&path)
                .unwrap_err()
                .contains("public_host")
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn public_hosts_become_audiences_beside_loopback() {
        let opts = Options {
            public_hosts: Some("Push.Example, https://alt.example:8443/".into()),
            ..Options::default()
        };
        let a = opts.audiences().unwrap();
        assert!(a.contains("push.example") && a.contains("alt.example") && a.contains("127.0.0.1"));
        let bad = Options {
            public_hosts: Some("http://".into()),
            ..Options::default()
        };
        assert!(bad.audiences().is_err());
    }

    #[test]
    fn dm_relays_parse_or_default() {
        assert_eq!(Options::default().dm_relays().unwrap(), None);
        let opts = Options {
            dm_relays: Some("wss://a.example, wss://b.example".into()),
            ..Options::default()
        };
        assert_eq!(
            opts.dm_relays().unwrap().unwrap(),
            ["wss://a.example", "wss://b.example"]
        );
        let bad = Options {
            dm_relays: Some("https://a.example".into()),
            ..Options::default()
        };
        assert!(bad.dm_relays().is_err());
    }
}
