use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use discordia_server::protocol::IceServer;
use serde::{Deserialize, Serialize};

pub const DEFAULT_ADDR: &str = "0.0.0.0:9000";
pub const DEFAULT_DATA_DIR: &str = "./discordia-data";

/// One setting, three spellings: `--flag`, the variable beside it, and the key in a
/// config file's `[server]` table — in that order of precedence.
#[derive(Debug, Default, Clone, PartialEq, clap::Args, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerOptions {
    /// Plaintext gateway bind, for loopback and a TLS proxy; clients refuse ws:// to anything else [default: 0.0.0.0:9000]
    #[arg(long, env = "DISCORDIA_ADDR")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub addr: Option<SocketAddr>,
    /// UDP port of the QUIC gateway, the one to open; 0 takes a random free one [default: 9001]
    #[arg(long, env = "DISCORDIA_QUIC_PORT")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quic_port: Option<u16>,
    /// SQLite, media blobs, livekit-keys and quic-secret (back that one up) [default: ./discordia-data]
    #[arg(long, env = "DISCORDIA_DATA_DIR")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_dir: Option<PathBuf>,
    /// The IP or DNS name friends reach this machine at: put in the share string and advertised by the bundled SFU
    #[arg(long, env = "DISCORDIA_PUBLIC_ADDRESS")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_address: Option<String>,
    /// Names clients dial through a TLS proxy (host, host:port or URL); loopback and interface IPs are always accepted
    #[arg(long, env = "DISCORDIA_PUBLIC_HOSTS", value_delimiter = ',')]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_hosts: Option<Vec<String>>,
    /// An iroh relay (a rendezvous's /config names one) for friends behind NAT
    #[arg(long, env = "DISCORDIA_RELAY_URL")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relay_url: Option<String>,
    /// Hex pubkeys who moderate system guilds
    #[arg(long, env = "DISCORDIA_OPERATORS", value_delimiter = ',')]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operators: Option<Vec<String>>,
    /// Cap on <data dir>/media; uploads are refused past it [default: 2 GiB]
    #[arg(long, env = "DISCORDIA_MEDIA_MAX_BYTES")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_max_bytes: Option<u64>,
    /// JSON list of {urls, username, credential} handed to every voice client
    #[arg(long, env = "DISCORDIA_ICE_SERVERS")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ice_servers: Option<IceServers>,
    /// An external SFU; unset runs the bundled one [default: derived per connection]
    #[arg(long, env = "LIVEKIT_URL")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub livekit_url: Option<String>,
    /// With the secret, overrides <data dir>/livekit-keys; must match an external SFU
    #[arg(long, env = "LIVEKIT_API_KEY")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub livekit_api_key: Option<String>,
    #[arg(long, env = "LIVEKIT_API_SECRET", hide_env_values = true)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub livekit_api_secret: Option<String>,
    /// Start the bundled SFU when no livekit_url is set [default: true]
    #[arg(long, env = "DISCORDIA_LIVEKIT_AUTOSPAWN", value_parser = clap::builder::BoolishValueParser::new())]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub livekit_autospawn: Option<bool>,
}

/// A JSON string on the command line and in the environment, a TOML array in the file.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct IceServers(pub Vec<IceServer>);

impl std::str::FromStr for IceServers {
    type Err = String;
    fn from_str(json: &str) -> Result<Self, String> {
        serde_json::from_str(json)
            .map(IceServers)
            .map_err(|e| format!("not a JSON list of ICE servers: {e}"))
    }
}

impl ServerOptions {
    /// `self` wins; `file` fills only what `self` leaves unset.
    pub fn or(self, file: ServerOptions) -> ServerOptions {
        ServerOptions {
            addr: self.addr.or(file.addr),
            quic_port: self.quic_port.or(file.quic_port),
            data_dir: self.data_dir.or(file.data_dir),
            public_address: self.public_address.or(file.public_address),
            public_hosts: self.public_hosts.or(file.public_hosts),
            relay_url: self.relay_url.or(file.relay_url),
            operators: self.operators.or(file.operators),
            media_max_bytes: self.media_max_bytes.or(file.media_max_bytes),
            ice_servers: self.ice_servers.or(file.ice_servers),
            livekit_url: self.livekit_url.or(file.livekit_url),
            livekit_api_key: self.livekit_api_key.or(file.livekit_api_key),
            livekit_api_secret: self.livekit_api_secret.or(file.livekit_api_secret),
            livekit_autospawn: self.livekit_autospawn.or(file.livekit_autospawn),
        }
    }

    pub fn data_dir(&self) -> PathBuf {
        self.data_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from(DEFAULT_DATA_DIR))
    }
}

/// The whole file. Each role reads its own table; an unknown table is a typo.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server: Option<ServerOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rendezvous: Option<discordia_rendezvous::run::Options>,
}

impl ConfigFile {
    pub fn load(path: &Path) -> Result<ConfigFile, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn render(&self) -> String {
        let body = toml::to_string_pretty(self).expect("config serializes");
        format!(
            "# Each key is also a --flag and a variable (`discordia-server --help`,\n\
             # `discordia-server rendezvous --help`), and those win over this file.\n\n{body}"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rendered_file_reads_back_the_same() {
        let file = ConfigFile {
            server: Some(ServerOptions {
                addr: Some("127.0.0.1:9000".parse().unwrap()),
                public_address: Some("203.0.113.7".into()),
                operators: Some(vec!["ab".repeat(32)]),
                ice_servers: Some(IceServers(vec![IceServer {
                    urls: vec!["turn:t.example:3478".into()],
                    username: "u".into(),
                    credential: "c".into(),
                }])),
                livekit_autospawn: Some(true),
                ..ServerOptions::default()
            }),
            rendezvous: Some(discordia_rendezvous::run::Options {
                turn_ports: Some("7710-7809".into()),
                ..Default::default()
            }),
        };
        let text = file.render();
        assert_eq!(toml::from_str::<ConfigFile>(&text).unwrap(), file);
    }

    #[test]
    fn a_misspelt_table_is_refused() {
        assert!(toml::from_str::<ConfigFile>("[sever]\nquic_port = 1\n").is_err());
        assert!(toml::from_str::<ConfigFile>("[server]\nquic_prot = 1\n").is_err());
    }

    #[test]
    fn ice_servers_parse_from_the_variable_form() {
        let parsed: IceServers = r#"[{"urls":["stun:s.example:3478"]}]"#.parse().unwrap();
        assert_eq!(parsed.0[0].urls, ["stun:s.example:3478"]);
        assert!("not json".parse::<IceServers>().is_err());
    }
}
