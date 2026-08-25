use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::identity::config_dir;
use crate::state::SessionMode;

const FILE_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SavedSession {
    pub mode: SessionMode,
    pub username: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Stored {
    version: u32,
    session: SavedSession,
}

fn session_path() -> PathBuf {
    config_dir().join("session.json")
}

pub fn save(session: &SavedSession) -> Result<(), String> {
    let path = session_path();
    let parent = path
        .parent()
        .ok_or_else(|| "session path has no parent".to_string())?;
    std::fs::create_dir_all(parent).map_err(|e| format!("create config dir: {e}"))?;
    let stored = Stored {
        version: FILE_VERSION,
        session: session.clone(),
    };
    let content =
        serde_json::to_string_pretty(&stored).map_err(|e| format!("serialize session: {e}"))?;
    std::fs::write(&path, content).map_err(|e| format!("write session: {e}"))?;
    Ok(())
}

pub fn load() -> Result<Option<SavedSession>, String> {
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
    Ok(Some(stored.session))
}

pub fn clear() -> Result<(), String> {
    let path = session_path();
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| format!("remove session: {e}"))?;
    }
    Ok(())
}

pub fn label(session: &SavedSession) -> String {
    match &session.mode {
        SessionMode::Remote { server_url } => format!("Remote · {server_url}"),
        SessionMode::SelfHost { .. } => "Self-host".to_string(),
        SessionMode::ByCode { code, .. } => format!("Code · {code}"),
    }
}
