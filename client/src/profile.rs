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

fn profile_path(root: &std::path::Path, pubkey: &str) -> Option<std::path::PathBuf> {
    (pubkey.len() == 64 && pubkey.bytes().all(|c| c.is_ascii_hexdigit()))
        .then(|| root.join("profiles").join(format!("{pubkey}.json")))
}

pub fn save(pubkey: &str, profile: &LocalProfile) -> Result<(), String> {
    save_at(&config_dir(), pubkey, profile)
}

fn save_at(root: &std::path::Path, pubkey: &str, profile: &LocalProfile) -> Result<(), String> {
    let path = profile_path(root, pubkey).ok_or("invalid profile owner")?;
    let stored = Stored {
        version: FILE_VERSION,
        profile: profile.clone(),
    };
    let content =
        serde_json::to_string_pretty(&stored).map_err(|e| format!("serialize profile: {e}"))?;
    crate::identity::write_private(&path, &content)
}

pub fn load(pubkey: &str) -> Option<LocalProfile> {
    load_at(&profile_path(&config_dir(), pubkey)?)
}

fn load_at(path: &std::path::Path) -> Option<LocalProfile> {
    let content = std::fs::read_to_string(path).ok()?;
    let stored: Stored = serde_json::from_str(&content).ok()?;
    if stored.version != FILE_VERSION {
        return None;
    }
    Some(stored.profile)
}

pub fn migrate_legacy(pubkey: &str) -> Result<(), String> {
    migrate_at(&config_dir(), pubkey)
}

fn migrate_at(root: &std::path::Path, pubkey: &str) -> Result<(), String> {
    let legacy = root.join("profile.json");
    let destination = profile_path(root, pubkey).ok_or("invalid profile owner")?;
    if !legacy.exists() {
        return Ok(());
    }
    if !destination.exists() {
        let profile = load_at(&legacy).ok_or("could not read legacy profile")?;
        save_at(root, pubkey, &profile)?;
    }
    std::fs::rename(legacy, root.join("profile.migrated.json"))
        .map_err(|e| format!("archive legacy profile: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accounts_keep_separate_profiles_and_legacy_moves_only_once() {
        let root = std::env::temp_dir().join(format!("profiles-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let alice = "a".repeat(64);
        let bob = "b".repeat(64);
        let old = LocalProfile {
            bio: Some("Alice".into()),
            ..Default::default()
        };
        let stored = Stored {
            version: FILE_VERSION,
            profile: old.clone(),
        };
        std::fs::write(
            root.join("profile.json"),
            serde_json::to_string(&stored).unwrap(),
        )
        .unwrap();
        migrate_at(&root, &alice).unwrap();
        migrate_at(&root, &bob).unwrap();
        assert_eq!(
            load_at(&profile_path(&root, &alice).unwrap()),
            Some(old.clone())
        );
        assert_eq!(load_at(&profile_path(&root, &bob).unwrap()), None);
        let other = LocalProfile {
            bio: Some("Bob".into()),
            ..Default::default()
        };
        save_at(&root, &bob, &other).unwrap();
        save_at(&root, &bob, &other).unwrap();
        assert_eq!(load_at(&profile_path(&root, &alice).unwrap()), Some(old));
        assert_eq!(load_at(&profile_path(&root, &bob).unwrap()), Some(other));
        assert!(profile_path(&root, "../profile").is_none());
        std::fs::remove_dir_all(root).unwrap();
    }
}
