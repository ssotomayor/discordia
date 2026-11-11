use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::identity::config_dir;
use crate::state::SessionMode;

const FILE_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SavedSession {
    pub mode: SessionMode,
    /// True from connect until the session is left, so it is still true after a killed process.
    #[serde(default)]
    pub live: bool,
}

/// How long a resumed session must hold before a later start may resume it again:
/// a start that dies while resuming must reach Home the next time.
pub const RESUME_SETTLE: std::time::Duration = std::time::Duration::from_secs(20);

/// Android ends a backgrounded process whenever it likes, so only there is a live session
/// resumed at start. Hosting is never restarted unasked: it publishes a name.
pub fn resumes(saved: &SavedSession, mobile: bool) -> bool {
    mobile && saved.live && !matches!(saved.mode, SessionMode::SelfHost { .. })
}

pub fn set_live(live: bool) -> Result<(), String> {
    let Some(mut saved) = load()? else {
        return Ok(());
    };
    if saved.live == live {
        return Ok(());
    }
    saved.live = live;
    save(&saved)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn remote(live: bool) -> SavedSession {
        SavedSession {
            mode: SessionMode::Remote {
                server_url: "wss://example.test".into(),
            },
            live,
        }
    }

    #[test]
    fn a_session_saved_before_the_mark_existed_is_not_live() {
        let stored: Stored = serde_json::from_str(
            r#"{"version":1,"session":{"mode":{"kind":"remote","server_url":"wss://example.test"}}}"#,
        )
        .expect("old file parses");
        assert_eq!(stored.session, remote(false));
    }

    #[test]
    fn only_a_live_joined_session_resumes_and_only_on_a_phone() {
        let hosting = SavedSession {
            mode: SessionMode::SelfHost {
                allow_lan: false,
                manual_ip: None,
                rendezvous_url: None,
                publish_name: None,
                description: None,
                publish_public: false,
                location: None,
            },
            live: true,
        };
        for (saved, mobile, expected) in [
            (remote(true), true, true),
            (remote(true), false, false),
            (remote(false), true, false),
            (hosting, true, false),
        ] {
            assert_eq!(
                resumes(&saved, mobile),
                expected,
                "{saved:?} mobile={mobile}"
            );
        }
    }

    #[test]
    fn the_mark_is_set_and_cleared_on_the_saved_session_and_nothing_else() {
        let _dir = crate::identity::test_config_dir();
        assert!(set_live(true).is_ok(), "no file is not an error");
        assert_eq!(load().unwrap(), None);

        save(&remote(true)).unwrap();
        set_live(false).unwrap();
        assert_eq!(load().unwrap(), Some(remote(false)));
        set_live(true).unwrap();
        assert_eq!(load().unwrap(), Some(remote(true)));
    }
}
