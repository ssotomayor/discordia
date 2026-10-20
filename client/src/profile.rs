use serde::{Deserialize, Serialize};

use crate::identity::config_dir;

const FILE_VERSION: u32 = 1;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalProfile {
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default)]
    pub banner: Option<String>,
    #[serde(default)]
    pub bio: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub custom_status: Option<String>,
    #[serde(default)]
    pub hide_guild_owner: bool,
    #[serde(default)]
    pub hide_operator: bool,
    /// Milliseconds at the last save on any device; 0 for a file older than
    /// the stamp, which then loses to every stamped copy.
    #[serde(default)]
    pub updated_at: i64,
}

impl LocalProfile {
    pub fn set_profile_message(self) -> crate::protocol::ClientMessage {
        // A link saved by an older build would be refused now and take the whole
        // profile with it, so it is dropped here rather than sent.
        let picture = |v: Option<String>| {
            v.filter(|p| !p.starts_with("http://") && !p.starts_with("https://"))
        };
        crate::protocol::ClientMessage::SetProfile {
            avatar: picture(self.avatar),
            banner: picture(self.banner),
            bio: self.bio,
            status: self.status,
            custom_status: self.custom_status,
            hide_guild_owner: self.hide_guild_owner,
            hide_operator: self.hide_operator,
            updated_at: self.updated_at,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Stored {
    version: u32,
    profile: LocalProfile,
}

fn profile_path() -> std::path::PathBuf {
    config_dir().join("profile.json")
}

pub fn save(profile: &LocalProfile) -> Result<(), String> {
    let path = profile_path();
    let parent = path
        .parent()
        .ok_or_else(|| "profile path has no parent".to_string())?;
    std::fs::create_dir_all(parent).map_err(|e| format!("create config dir: {e}"))?;
    let stored = Stored {
        version: FILE_VERSION,
        profile: profile.clone(),
    };
    let content =
        serde_json::to_string_pretty(&stored).map_err(|e| format!("serialize profile: {e}"))?;
    std::fs::write(&path, content).map_err(|e| format!("write profile: {e}"))?;
    Ok(())
}

pub fn load() -> Option<LocalProfile> {
    let path = profile_path();
    if !path.exists() {
        return None;
    }
    let content = std::fs::read_to_string(&path).ok()?;
    let stored: Stored = serde_json::from_str(&content).ok()?;
    if stored.version != FILE_VERSION {
        return None;
    }
    Some(stored.profile)
}
