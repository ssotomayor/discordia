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
    session: SavedSession,
    /// Every server worth offering again, newest first.
    ///
    /// `#[serde(default)]` is the whole migration: a file written before this
    /// field existed reads back with an empty list, and `load_all` seeds it from
    /// `session`, which is the one server that file did remember. Bumping
    /// `FILE_VERSION` would have been the other option and a worse one — this
    /// file's version check *discards* on mismatch, so it would have thrown away
    /// the last session of everyone who upgraded.
    #[serde(default)]
    servers: Vec<SavedSession>,
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
        session: session.clone(),
        servers,
    })
}

/// Drop one server from the bar, leaving the rest and the last-session record.
pub fn forget(mode: &SessionMode) -> Result<(), String> {
    let mut servers = load_all();
    servers.retain(|s| &s.mode != mode);
    let session = match load()? {
        Some(s) => s,
        // Nothing to anchor the file to; removing the last row removes the file.
        None => return clear(),
    };
    write(&Stored {
        version: FILE_VERSION,
        session,
        servers,
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
    if stored.servers.is_empty() {
        return vec![stored.session];
    }
    stored.servers
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
    Ok(read_stored()?.map(|s| s.session))
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
