use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::identity::config_dir;
use crate::protocol::Id;
use crate::state::SessionMode;

/// A server the rail offers, with the guilds it had when last connected — the
/// only way to show them without being connected to it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KnownServer {
    pub mode: SessionMode,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub guilds: Vec<KnownGuild>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KnownGuild {
    pub id: Id,
    pub name: String,
    #[serde(default)]
    pub icon: Option<String>,
}

impl KnownServer {
    pub fn new(mode: SessionMode) -> Self {
        Self {
            mode,
            name: None,
            guilds: Vec::new(),
        }
    }

    pub fn key(&self) -> String {
        server_key(&self.mode)
    }

    pub fn label(&self) -> String {
        if let Some(name) = self.name.as_ref().filter(|n| !n.trim().is_empty()) {
            return name.clone();
        }
        match &self.mode {
            SessionMode::SelfHost { publish_name, .. } => publish_name
                .clone()
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(|| "Your server".into()),
            SessionMode::ByCode { code, .. } => code.clone(),
            SessionMode::Remote { server_url } => remote_label(server_url),
        }
    }

    /// Where it is reached, for the tooltip.
    pub fn route(&self) -> String {
        match &self.mode {
            SessionMode::SelfHost { .. } => "Runs on this machine while you are on it".into(),
            SessionMode::ByCode {
                rendezvous_url,
                code,
            } => format!(
                "Code {code} via {}",
                rendezvous_url
                    .split_once("://")
                    .map_or(rendezvous_url.as_str(), |(_, r)| r)
            ),
            SessionMode::Remote { server_url } => {
                if server_url.starts_with("quic://") {
                    "Direct address from its host".into()
                } else {
                    server_url.clone()
                }
            }
        }
    }
}

/// One machine hosts one server, and a code means nothing without its rendezvous.
pub fn server_key(mode: &SessionMode) -> String {
    match mode {
        SessionMode::SelfHost { .. } => "selfhost".into(),
        SessionMode::ByCode {
            rendezvous_url,
            code,
        } => {
            let code = crate::protocol::rendezvous::join_code(code)
                .unwrap_or_else(|| code.trim().to_string());
            format!(
                "code:{}|{code}",
                rendezvous_url.trim().trim_end_matches('/')
            )
        }
        SessionMode::Remote { server_url } => format!("remote:{}", server_url.trim()),
    }
}

fn remote_label(url: &str) -> String {
    if let Some(share) = crate::protocol::parse_quic_share(url) {
        let short: String = share.key.chars().take(8).collect();
        return format!("Direct {short}");
    }
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split(['/', ':']).next().unwrap_or(rest).to_string()
}

/// Seen now: kept in place if already listed, so the rail does not reshuffle.
pub fn upsert(list: &mut Vec<KnownServer>, mode: SessionMode, guilds: Option<Vec<KnownGuild>>) {
    let key = server_key(&mode);
    match list.iter_mut().find(|s| s.key() == key) {
        Some(known) => {
            known.mode = mode;
            if let Some(guilds) = guilds {
                known.guilds = guilds;
            }
        }
        None => list.push(KnownServer {
            guilds: guilds.unwrap_or_default(),
            ..KnownServer::new(mode)
        }),
    }
}

pub fn remove(list: &mut Vec<KnownServer>, key: &str) {
    list.retain(|s| s.key() != key);
}

fn path() -> PathBuf {
    config_dir().join("servers.json")
}

pub fn load() -> Vec<KnownServer> {
    std::fs::read_to_string(path())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn save(list: &[KnownServer]) {
    let path = path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match serde_json::to_string_pretty(list) {
        Ok(text) => {
            if let Err(e) = std::fs::write(&path, text) {
                tracing::warn!(error = %e, "could not save the server list");
            }
        }
        Err(e) => tracing::warn!(error = %e, "could not encode the server list"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn by_code(code: &str) -> SessionMode {
        SessionMode::ByCode {
            rendezvous_url: "ws://r.example:7700/".into(),
            code: code.into(),
        }
    }

    #[test]
    fn a_code_typed_two_ways_is_one_server() {
        assert_eq!(
            server_key(&by_code("Purple Fox")),
            server_key(&by_code("purple-fox"))
        );
    }

    #[test]
    fn seeing_a_server_again_keeps_its_place_and_refreshes_its_guilds() {
        let mut list = Vec::new();
        upsert(&mut list, by_code("a"), None);
        upsert(&mut list, by_code("b"), None);
        let guild = KnownGuild {
            id: Id::nil(),
            name: "General".into(),
            icon: None,
        };
        upsert(&mut list, by_code("a"), Some(vec![guild.clone()]));
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].guilds, [guild]);
        upsert(&mut list, by_code("a"), None);
        assert_eq!(list[0].guilds.len(), 1, "no guild list is not an empty one");
        remove(&mut list, &server_key(&by_code("a")));
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn a_label_never_shows_a_whole_key() {
        let quic = KnownServer::new(SessionMode::Remote {
            server_url: format!("quic://{}@1.2.3.4:9001", "a".repeat(64)),
        });
        assert_eq!(quic.label(), "Direct aaaaaaaa");
        let wss = KnownServer::new(SessionMode::Remote {
            server_url: "wss://chat.example.org/gateway".into(),
        });
        assert_eq!(wss.label(), "chat.example.org");
    }
}
