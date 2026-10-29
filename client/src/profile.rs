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
    /// The account name as last renamed on any device. The key file holds it
    /// too, but has no edit time to tell an old name from a new one.
    #[serde(default)]
    pub name: Option<String>,
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

#[derive(Debug, Clone, Default)]
pub struct Pictures {
    pub avatar: Option<std::sync::Arc<String>>,
    pub banner: Option<std::sync::Arc<String>>,
}

impl From<LocalProfile> for Pictures {
    fn from(profile: LocalProfile) -> Self {
        Self {
            avatar: profile.avatar.map(std::sync::Arc::new),
            banner: profile.banner.map(std::sync::Arc::new),
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

pub fn save_name(pubkey: &str, name: &str, now_ms: i64) -> Result<LocalProfile, String> {
    let local = renamed(load(pubkey).unwrap_or_default(), name, now_ms);
    save(pubkey, &local)?;
    Ok(local)
}

/// Stamped past the last save, so the rename outranks every device's older
/// copy even when this clock runs behind the one that saved it.
fn renamed(mut local: LocalProfile, name: &str, now_ms: i64) -> LocalProfile {
    local.name = Some(name.to_owned());
    local.updated_at = now_ms.max(local.updated_at.saturating_add(1));
    local
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

pub(crate) fn use_avatar_recovery(
    state: dioxus::prelude::Signal<crate::state::AppState>,
    owner: String,
) {
    use dioxus::prelude::*;
    let key = owner.clone();
    let available = use_memo(move || {
        let state = state.read();
        if state
            .account_pictures
            .as_ref()
            .is_some_and(|(account, pictures)| account == &key && pictures.avatar.is_some())
        {
            return None;
        }
        let profile = state.profiles.get(&key)?;
        let address = profile.avatar.as_deref()?;
        let data = match address.strip_prefix("media:") {
            Some(blob) => state.emoji_images.get_shared(blob)?,
            None => std::sync::Arc::new(address.to_owned()),
        };
        Some((profile.updated_at, data))
    });
    use_effect(move || {
        let Some((updated_at, avatar)) = available() else {
            return;
        };
        let local = load(&owner).unwrap_or_default();
        let Some(recovered) = recover_avatar(&local, updated_at, &avatar) else {
            return;
        };
        if let Err(error) = save(&owner, &recovered) {
            tracing::warn!(%error, "account avatar recovery could not be saved");
            return;
        }
        let mut state = state;
        state.write().account_pictures = Some((owner.clone(), recovered.into()));
    });
}

fn recover_avatar(
    local: &LocalProfile,
    server_updated_at: i64,
    avatar: &str,
) -> Option<LocalProfile> {
    if local.avatar.is_some()
        || local.updated_at > server_updated_at
        || !avatar.starts_with("data:image/")
        || avatar.len() > crate::protocol::MAX_IMAGE_BYTES.div_ceil(3) * 4 + 128
    {
        return None;
    }
    let mut recovered = local.clone();
    recovered.avatar = Some(avatar.to_owned());
    // Recovering cached bytes is not a profile edit and must not republish older metadata.
    Some(recovered)
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
    #[test]
    fn a_missing_avatar_is_recovered_for_home_and_survives_a_restart_without_editing_metadata() {
        let root = std::env::temp_dir().join(format!("avatar-recovery-{}", uuid::Uuid::new_v4()));
        let owner = "a".repeat(64);
        let local = LocalProfile {
            bio: Some("Keep my bio".into()),
            updated_at: 10,
            ..Default::default()
        };
        let avatar = "data:image/png;base64,Zmlyc3Q=";
        let recovered = recover_avatar(&local, 20, avatar).unwrap();
        assert_eq!(recovered.bio, local.bio);
        assert_eq!(recovered.updated_at, 10);
        save_at(&root, &owner, &recovered).unwrap();
        let restored = load_at(&profile_path(&root, &owner).unwrap()).unwrap();
        assert_eq!(restored.avatar.as_deref(), Some(avatar));
        let mut state = crate::state::AppState::empty();
        state.account_pictures = Some((owner.clone(), restored.into()));
        state.clear_server_session();
        assert_eq!(state.avatar_address(&owner), Some(avatar));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn avatar_recovery_preserves_personal_images_and_newer_deletions() {
        let avatar = "data:image/png;base64,Zmlyc3Q=";
        let mut local = LocalProfile {
            avatar: Some("personal".into()),
            updated_at: 10,
            ..Default::default()
        };
        assert!(recover_avatar(&local, 20, avatar).is_none());
        local.avatar = None;
        assert!(recover_avatar(&local, 9, avatar).is_none());
        assert!(recover_avatar(&local, 20, "media:server-only.png").is_none());
        assert!(recover_avatar(&local, 20, "https://example.com/avatar.png").is_none());
        assert!(recover_avatar(&local, 10, avatar).is_some());
    }

    use super::*;

    #[test]
    fn a_rename_outranks_the_last_save_even_from_a_slow_clock() {
        let local = LocalProfile {
            bio: Some("kept".into()),
            updated_at: 5_000,
            ..Default::default()
        };
        let ahead = renamed(local.clone(), "New", 9_000);
        assert_eq!(ahead.name.as_deref(), Some("New"));
        assert_eq!(ahead.updated_at, 9_000);
        assert_eq!(ahead.bio, local.bio);
        assert_eq!(renamed(local, "New", 1_000).updated_at, 5_001);
    }

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
