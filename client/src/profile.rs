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
