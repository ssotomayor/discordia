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
    /// The dial origin at its last Ready; a mention notification names a server by it.
    #[serde(default)]
    pub origin: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KnownGuild {
    pub id: Id,
    pub name: String,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub icon_image: Option<String>,
}

impl KnownServer {
    pub fn new(mode: SessionMode) -> Self {
        Self {
            mode,
            name: None,
            guilds: Vec::new(),
            origin: None,
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

/// What was taken out, so whatever it held elsewhere can go with it.
pub fn remove(list: &mut Vec<KnownServer>, key: &str) -> Vec<KnownServer> {
    let (removed, kept) = std::mem::take(list)
        .into_iter()
        .partition(|s| s.key() == key);
    *list = kept;
    removed
}

/// Origins no saved server and not the live session still answers to: a
/// server's push handle is per origin, and another entry may dial the same one.
pub fn released_origins(
    removed: &[KnownServer],
    kept: &[KnownServer],
    live: Option<&str>,
) -> Vec<String> {
    let mut origins: Vec<String> = removed
        .iter()
        .filter_map(|server| server.origin.clone())
        .filter(|origin| live != Some(origin.as_str()))
        .filter(|origin| !kept.iter().any(|s| s.origin.as_ref() == Some(origin)))
        .collect();
    origins.dedup();
    origins
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
            icon_image: None,
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
    fn removing_a_server_releases_its_origin_unless_something_still_dials_it() {
        let at = |code: &str, origin: Option<&str>| KnownServer {
            origin: origin.map(str::to_owned),
            ..KnownServer::new(by_code(code))
        };
        let mut list = vec![
            at("a", Some("quic://a@1.2.3.4:1")),
            at("b", Some("quic://b@1.2.3.4:2")),
            at("c", Some("quic://b@1.2.3.4:2")),
            at("d", None),
        ];
        let removed = remove(&mut list, &server_key(&by_code("a")));
        assert_eq!(removed, [at("a", Some("quic://a@1.2.3.4:1"))]);
        assert_eq!(list.len(), 3);
        assert_eq!(
            released_origins(&removed, &list, None),
            ["quic://a@1.2.3.4:1"]
        );
        assert!(
            released_origins(&removed, &list, Some("quic://a@1.2.3.4:1")).is_empty(),
            "the live session's handle is the gateway loop's"
        );

        let removed = remove(&mut list, &server_key(&by_code("b")));
        assert!(
            released_origins(&removed, &list, None).is_empty(),
            "another entry still dials that origin"
        );
        let removed = remove(&mut list, &server_key(&by_code("d")));
        assert!(released_origins(&removed, &list, None).is_empty());
        assert!(remove(&mut list, "nothing").is_empty());
    }

    #[test]
    fn saved_community_photos_survive_reload_and_can_be_removed() {
        let root = crate::identity::test_config_dir();
        let guild = KnownGuild {
            id: Id::new_v4(),
            name: "Community".into(),
            icon: Some("C".into()),
            icon_image: Some(format!("media:{}.png", "a".repeat(64))),
        };
        let mut list = Vec::new();
        upsert(&mut list, by_code("a"), Some(vec![guild.clone()]));
        save(&list);
        assert_eq!(load(), list);
        let mut list = load();
        upsert(&mut list, by_code("a"), None);
        assert_eq!(list[0].guilds[0].icon_image, guild.icon_image);
        let mut without_photo = guild;
        without_photo.icon_image = None;
        upsert(&mut list, by_code("a"), Some(vec![without_photo]));
        save(&list);
        assert_eq!(load()[0].guilds[0].icon_image, None);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn older_saved_communities_without_photos_still_load() {
        let guild: KnownGuild = serde_json::from_value(serde_json::json!({
            "id": Id::nil(), "name": "Community", "icon": "C"
        }))
        .unwrap();
        assert_eq!(guild.icon.as_deref(), Some("C"));
        assert_eq!(guild.icon_image, None);
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
