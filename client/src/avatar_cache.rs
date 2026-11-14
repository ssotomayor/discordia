use std::path::Path;

use serde::{Deserialize, Serialize};

const MAX_ENTRIES: usize = 2048;
const MAX_INDEX_BYTES: u64 = 512 * 1024;

#[derive(Serialize, Deserialize)]
struct Entry {
    owner: String,
    image: Option<String>,
    #[serde(default)]
    updated_at: i64,
}

fn entries(root: &Path) -> Vec<Entry> {
    let path = root.join("peer-avatars.json");
    if !std::fs::metadata(&path).is_ok_and(|m| m.len() <= MAX_INDEX_BYTES) {
        return Vec::new();
    }
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub(crate) fn store(root: &Path, updates: Vec<(String, Option<String>, i64)>) {
    let mut saved = entries(root);
    for (owner, image, updated_at) in updates {
        if !crate::protocol::is_canonical_pubkey(&owner)
            || image
                .as_deref()
                .is_some_and(|image| !discordia_server::media::is_image_address(image))
        {
            continue;
        }
        if saved
            .iter()
            .any(|entry| entry.owner == owner && entry.updated_at > updated_at)
        {
            continue;
        }
        saved.retain(|entry| entry.owner != owner);
        saved.push(Entry {
            owner,
            image,
            updated_at,
        });
    }
    saved.drain(..saved.len().saturating_sub(MAX_ENTRIES));
    match serde_json::to_string(&saved) {
        Ok(json) => {
            if let Err(error) =
                crate::identity::write_private(&root.join("peer-avatars.json"), &json)
            {
                tracing::debug!(%error, "could not cache peer avatars");
            }
        }
        Err(error) => tracing::debug!(%error, "could not serialize peer avatars"),
    }
}

pub(crate) fn load(root: &Path, owner: &str, address: Option<&str>) -> Option<String> {
    let saved;
    let address = match address {
        Some(address) => address,
        None => {
            saved = entries(root);
            saved
                .iter()
                .rev()
                .find(|entry| entry.owner == owner)?
                .image
                .as_deref()?
        }
    };
    crate::emoji::load_cached_in(root, address.strip_prefix("media:")?)
        .filter(|data| !data.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_offline_dm_uses_received_bytes_and_honors_avatar_removal() {
        let root = std::env::temp_dir().join(format!("avatars-{}", uuid::Uuid::new_v4()));
        let owner = "ab".repeat(32);
        let blob = format!("{}.png", "cd".repeat(32));
        let address = format!("media:{blob}");
        let data = "data:image/png;base64,YXZhdGFy";
        store(&root, vec![(owner.clone(), Some(address.clone()), 1)]);
        assert_eq!(load(&root, &owner, None), None);
        crate::emoji::store_cached(&root, &blob, data);
        assert_eq!(load(&root, &owner, None).as_deref(), Some(data));
        assert_eq!(load(&root, "other", None), None);
        store(&root, vec![(owner.clone(), None, 2)]);
        store(&root, vec![(owner.clone(), Some(address.clone()), 1)]);
        assert_eq!(load(&root, &owner, None), None);
        assert_eq!(load(&root, &owner, Some(&address)).as_deref(), Some(data));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_corrupt_index_or_an_external_url_cannot_supply_an_avatar() {
        let root = std::env::temp_dir().join(format!("avatars-{}", uuid::Uuid::new_v4()));
        let owner = "ab".repeat(32);
        store(
            &root,
            vec![(
                owner.clone(),
                Some("https://example.com/avatar.png".into()),
                1,
            )],
        );
        assert_eq!(load(&root, &owner, None), None);
        std::fs::write(root.join("peer-avatars.json"), "invalid").unwrap();
        assert_eq!(load(&root, &owner, None), None);
        std::fs::remove_dir_all(root).unwrap();
    }
}
