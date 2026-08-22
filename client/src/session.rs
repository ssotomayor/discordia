//! Persistent record of the last session config (mode + display name).
//! Identity lives separately in `identity.json` — this file is just for
//! offering a one-click "Reconnect" on the next launch.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::identity::config_dir;
use crate::state::SessionMode;

const FILE_VERSION: u32 = 1;

/// How many servers the bar will remember. Enough for anyone's real list, and
/// small enough that a row of them still fits across a window.
const MAX_SERVERS: usize = 12;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SavedSession {
    pub mode: SessionMode,
    pub username: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Stored {
    version: u32,
    /// The last session, for the reconnect pill. Optional so that forgetting
    /// the only server can also stop offering to reconnect to it.
    #[serde(default)]
    session: Option<SavedSession>,
    /// Every server worth offering again, newest first.
    ///
    /// `#[serde(default)]` is the whole migration: a file written before this
    /// field existed reads back with an empty list, and `load_all` seeds it from
    /// `session`, which is the one server that file did remember. Bumping
    /// `FILE_VERSION` would have been the other option and a worse one — this
    /// file's version check *discards* on mismatch, so it would have thrown away
    /// the last session of everyone who upgraded.
    ///
    /// `Option`, not a bare `Vec`, because an empty list and no list at all are
    /// different facts and only one of them should be seeded from `session`.
    /// Conflating them made forgetting the last server a no-op that looked like
    /// a broken button: the row was removed, then `load_all` read the empty
    /// list as "written before this field existed" and put it straight back.
    #[serde(default)]
    servers: Option<Vec<SavedSession>>,
}

fn session_path() -> PathBuf {
    config_dir().join("session.json")
}

/// Remember `session` as the most recent one *and* keep it in the server list.
///
/// The two are different questions and the file answers both: `session` is
/// "reconnect to what I was on", the list is "everywhere I might want to go".
pub fn save(session: &SavedSession) -> Result<(), String> {
    let mut servers = load_all();
    // Newest first, de-duplicated by where it points rather than by the whole
    // record: reconnecting under a different display name is the same server,
    // and two entries for it would be two rows in the bar.
    servers.retain(|s| s.mode != session.mode);
    servers.insert(0, session.clone());
    servers.truncate(MAX_SERVERS);
    write(&Stored {
        version: FILE_VERSION,
        session: Some(session.clone()),
        servers: Some(servers),
    })
}

/// Drop one server from the bar, leaving the rest and the last-session record.
pub fn forget(mode: &SessionMode) -> Result<(), String> {
    let mut servers = load_all();
    servers.retain(|s| &s.mode != mode);
    // Forgetting a server also stops offering to reconnect to it. Leaving the
    // pill behind would be the same row under another name.
    let session = load()?.filter(|s| &s.mode != mode);
    write(&Stored {
        version: FILE_VERSION,
        session,
        servers: Some(servers),
    })
}

fn write(stored: &Stored) -> Result<(), String> {
    let path = session_path();
    let parent = path
        .parent()
        .ok_or_else(|| "session path has no parent".to_string())?;
    std::fs::create_dir_all(parent).map_err(|e| format!("create config dir: {e}"))?;
    let content =
        serde_json::to_string_pretty(stored).map_err(|e| format!("serialize session: {e}"))?;
    std::fs::write(&path, content).map_err(|e| format!("write session: {e}"))?;
    Ok(())
}

/// Every server to offer in the bar, newest first.
///
/// Never an error: an unreadable or unrecognised file means an empty bar, and
/// the connect screen still works. Seeds itself from `session` for a file
/// written before the list existed.
pub fn load_all() -> Vec<SavedSession> {
    let Ok(Some(stored)) = read_stored() else {
        return Vec::new();
    };
    servers_from(stored)
}

/// The list a stored file offers, split out so the migration is testable
/// without a disk.
fn servers_from(stored: Stored) -> Vec<SavedSession> {
    match stored.servers {
        Some(servers) => servers,
        // Written before the list existed: the one server it did remember.
        None => stored.session.into_iter().collect(),
    }
}

fn read_stored() -> Result<Option<Stored>, String> {
    let path = session_path();
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path).map_err(|e| format!("read session: {e}"))?;
    let stored: Stored =
        serde_json::from_str(&content).map_err(|e| format!("parse session: {e}"))?;
    if stored.version != FILE_VERSION {
        return Ok(None);
    }
    Ok(Some(stored))
}

pub fn load() -> Result<Option<SavedSession>, String> {
    Ok(read_stored()?.and_then(|s| s.session))
}

pub fn clear() -> Result<(), String> {
    let path = session_path();
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| format!("remove session: {e}"))?;
    }
    Ok(())
}

/// Human-friendly summary used in the "Reconnect" pill on the connect screen.
pub fn label(session: &SavedSession) -> String {
    match &session.mode {
        SessionMode::Remote { server_url } => format!("Remote · {server_url}"),
        SessionMode::SelfHost { .. } => "Self-host".to_string(),
        SessionMode::ByCode { code, .. } => format!("Code · {code}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote(url: &str) -> SavedSession {
        SavedSession {
            mode: SessionMode::Remote {
                server_url: url.into(),
            },
            username: "me".into(),
        }
    }

    fn stored(session: Option<SavedSession>, servers: Option<Vec<SavedSession>>) -> Stored {
        Stored {
            version: FILE_VERSION,
            session,
            servers,
        }
    }

    /// The whole migration: a file from before the list existed still offers
    /// the one server it remembered.
    #[test]
    fn a_file_without_a_list_falls_back_to_its_one_session() {
        let s = stored(Some(remote("ws://a")), None);
        assert_eq!(servers_from(s), vec![remote("ws://a")]);
    }

    /// The bug this split exists for. An empty list means "you forgot them
    /// all", and reading it as "no list here" put the last one straight back —
    /// so the forget button removed a row that reappeared, looking broken.
    #[test]
    fn an_empty_list_stays_empty_even_with_a_last_session() {
        let s = stored(Some(remote("ws://a")), Some(Vec::new()));
        assert!(servers_from(s).is_empty());
    }

    #[test]
    fn a_stored_list_is_used_as_is() {
        let s = stored(Some(remote("ws://a")), Some(vec![remote("ws://b")]));
        assert_eq!(servers_from(s), vec![remote("ws://b")]);
    }
}
