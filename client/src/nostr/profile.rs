//! Kind 30078 `discordia:profile`: the profile follows the key, not the machine,
//! encrypted to ourselves so a bio shown to one server is not a public note.

use secp256k1::SecretKey;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use super::event::{self, Event};
use super::{blossom, nip44};
use crate::profile::LocalProfile;

pub use super::xp::KIND_APP_DATA;

pub const D_TAG: &str = "discordia:profile";

const FETCH_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Picture {
    /// Never uploaded — no blob server, or the upload failed — so a device
    /// reading this keeps the picture it has rather than losing it.
    #[default]
    Unshared,
    None,
    Blob {
        url: String,
        sha256: String,
        mime: String,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncedProfile {
    /// Milliseconds, the saving device's clock. The event's `created_at` is
    /// this too, not the publish time, so a relay keeps the newest *edit*.
    #[serde(default)]
    pub updated_at: i64,
    #[serde(default)]
    pub avatar: Picture,
    #[serde(default)]
    pub banner: Picture,
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
    /// `None` from a build older than name sync: it claims no name.
    #[serde(default)]
    pub name: Option<String>,
}

pub fn profile_event(
    secret: &SecretKey,
    our_pubkey: &str,
    profile: &SyncedProfile,
) -> Result<Event, String> {
    let key = nip44::conversation_key(secret, our_pubkey)?;
    let json = serde_json::to_string(profile).map_err(|e| e.to_string())?;
    Ok(event::sign_with(
        secret,
        profile.updated_at.div_euclid(1000),
        KIND_APP_DATA,
        vec![vec!["d".to_string(), D_TAG.to_string()]],
        nip44::encrypt(&key, &json)?,
    ))
}

/// `None` for anything that is not our own profile event.
pub fn parse_profile(secret: &SecretKey, our_pubkey: &str, event: &Event) -> Option<SyncedProfile> {
    if event.kind != KIND_APP_DATA || event.pubkey != our_pubkey || event.tag("d") != Some(D_TAG) {
        return None;
    }
    let key = nip44::conversation_key(secret, our_pubkey).ok()?;
    let json = nip44::decrypt(&key, &event.content).ok()?;
    let mut profile: SyncedProfile = serde_json::from_str(&json).ok()?;
    // Every server refuses an unknown status, and with it the whole profile.
    profile.status = profile
        .status
        .filter(|s| crate::protocol::PRESENCES.contains(&s.as_str()));
    profile.name = profile
        .name
        .filter(|n| !crate::protocol::truncate_username(n).trim().is_empty())
        .map(|n| crate::protocol::canonical_username(&n));
    Some(profile)
}

impl SyncedProfile {
    /// `None` keeps what `current` holds: the picture was never shared.
    pub fn into_local(
        self,
        current: &LocalProfile,
        avatar: Option<Option<String>>,
        banner: Option<Option<String>>,
    ) -> LocalProfile {
        LocalProfile {
            avatar: avatar.unwrap_or_else(|| current.avatar.clone()),
            banner: banner.unwrap_or_else(|| current.banner.clone()),
            bio: self.bio,
            status: self.status,
            custom_status: self.custom_status,
            hide_guild_owner: self.hide_guild_owner,
            hide_operator: self.hide_operator,
            name: self.name.or_else(|| current.name.clone()),
            updated_at: self.updated_at,
        }
    }

    pub async fn share(
        local: LocalProfile,
        previous: &SyncedProfile,
        server: Option<&str>,
        secret: &SecretKey,
        now: i64,
    ) -> SyncedProfile {
        let avatar = share_picture(
            server,
            secret,
            local.avatar.as_deref(),
            &previous.avatar,
            now,
        )
        .await;
        let banner = share_picture(
            server,
            secret,
            local.banner.as_deref(),
            &previous.banner,
            now,
        )
        .await;
        SyncedProfile {
            updated_at: local.updated_at,
            avatar,
            banner,
            bio: local.bio,
            status: local.status,
            custom_status: local.custom_status,
            hide_guild_owner: local.hide_guild_owner,
            hide_operator: local.hide_operator,
            name: local.name,
        }
    }
}

/// Waits between attempts at a picture: a blip is retried, a dead blob server
/// is given up on.
const RETRY_WAITS: [Duration; 2] = [Duration::from_secs(2), Duration::from_secs(10)];

/// What a device that reads `picture` should hold: `None` to keep its own.
/// An unreachable blob is kept-own too, so it cannot hold back the bio and status.
pub async fn resolve(picture: &Picture) -> Option<Option<String>> {
    resolve_with(picture, &RETRY_WAITS).await
}

async fn resolve_with(picture: &Picture, waits: &[Duration]) -> Option<Option<String>> {
    let Picture::Blob { url, sha256, mime } = picture else {
        return (*picture == Picture::None).then_some(None);
    };
    let mut waits = waits.iter();
    loop {
        match fetch(url, sha256, mime).await {
            Ok(data_url) => return Some(Some(data_url)),
            Err(e) => {
                tracing::warn!(%url, error = %e, "synced profile picture unavailable");
                tokio::time::sleep(*waits.next()?).await;
            }
        }
    }
}

/// Reuses `previous` when the bytes have not changed, so a bio edit is not
/// another two-megabyte upload.
async fn share_picture(
    server: Option<&str>,
    secret: &SecretKey,
    picture: Option<&str>,
    previous: &Picture,
    now: i64,
) -> Picture {
    let Some(picture) = picture else {
        return Picture::None;
    };
    let Some((mime, bytes)) = blossom::split_data_url(picture) else {
        return Picture::Unshared;
    };
    let sha256 = blossom::sha256_hex(&bytes);
    if let Picture::Blob { sha256: had, .. } = previous
        && *had == sha256
    {
        return previous.clone();
    }
    let Some(server) = server else {
        return Picture::Unshared;
    };
    match blossom::upload(server, secret, picture, now).await {
        Ok(url) => Picture::Blob { url, sha256, mime },
        Err(e) => {
            tracing::warn!(error = %e, "profile picture upload failed");
            Picture::Unshared
        }
    }
}

async fn fetch(url: &str, sha256: &str, mime: &str) -> Result<String, String> {
    use base64::Engine as _;
    if !url.starts_with("https://") || !mime.starts_with("image/") {
        return Err("not an https image".into());
    }
    let http = reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .build()
        .map_err(|e| e.to_string())?;
    let res = http.get(url).send().await.map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        return Err(format!("answered {}", res.status()));
    }
    if res
        .content_length()
        .is_some_and(|n| n > crate::protocol::MAX_IMAGE_BYTES as u64)
    {
        return Err("larger than a profile picture may be".into());
    }
    let bytes = res.bytes().await.map_err(|e| e.to_string())?;
    if bytes.len() > crate::protocol::MAX_IMAGE_BYTES {
        return Err("larger than a profile picture may be".into());
    }
    if !blossom::sha256_hex(&bytes).eq_ignore_ascii_case(sha256) {
        return Err("the server returned different bytes".into());
    }
    Ok(format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&bytes)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(seed: u8) -> SecretKey {
        SecretKey::from_slice(&[seed; 32]).expect("valid key")
    }

    fn pubkey(secret: &SecretKey) -> String {
        event::xonly_hex(secret)
    }

    fn sample() -> SyncedProfile {
        SyncedProfile {
            updated_at: 1_700_000_000_123,
            avatar: Picture::Blob {
                url: "https://b.example/a.png".into(),
                sha256: "ab".repeat(32),
                mime: "image/png".into(),
            },
            banner: Picture::None,
            bio: Some("hi".into()),
            status: Some("dnd".into()),
            custom_status: None,
            hide_guild_owner: true,
            hide_operator: false,
            name: Some("Ana".into()),
        }
    }

    #[test]
    fn a_profile_round_trips_and_is_unreadable_to_the_relay() {
        let secret = key(3);
        let me = pubkey(&secret);
        let event = profile_event(&secret, &me, &sample()).unwrap();
        assert!(event.verify());
        assert_eq!(event.created_at, 1_700_000_000, "the edit's time, not now");
        assert!(serde_json::from_str::<SyncedProfile>(&event.content).is_err());
        let unrelated_key = nip44::conversation_key(&key(4), &me).unwrap();
        assert!(nip44::decrypt(&unrelated_key, &event.content).is_err());
        assert_eq!(parse_profile(&secret, &me, &event), Some(sample()));
    }

    #[test]
    fn another_keys_profile_is_not_ours() {
        let theirs = key(4);
        let event = profile_event(&theirs, &pubkey(&theirs), &sample()).unwrap();
        let mine = key(3);
        assert_eq!(parse_profile(&mine, &pubkey(&mine), &event), None);
    }

    #[test]
    fn a_profile_without_a_name_keeps_ours_and_a_blank_one_is_no_claim() {
        let current = LocalProfile {
            name: Some("Ours".into()),
            ..Default::default()
        };
        let mut unnamed = sample();
        unnamed.name = None;
        assert_eq!(
            unnamed.into_local(&current, None, None).name.as_deref(),
            Some("Ours")
        );
        assert_eq!(
            sample().into_local(&current, None, None).name.as_deref(),
            Some("Ana")
        );

        let secret = key(3);
        let me = pubkey(&secret);
        let mut blank = sample();
        blank.name = Some(" \u{0007} ".into());
        let event = profile_event(&secret, &me, &blank).unwrap();
        assert_eq!(parse_profile(&secret, &me, &event).unwrap().name, None);
    }

    #[test]
    fn an_unknown_status_is_dropped_rather_than_refused_by_every_server() {
        let secret = key(3);
        let me = pubkey(&secret);
        let mut odd = sample();
        odd.status = Some("napping".into());
        let event = profile_event(&secret, &me, &odd).unwrap();
        assert_eq!(parse_profile(&secret, &me, &event).unwrap().status, None);
    }

    #[test]
    fn an_unshared_picture_keeps_the_one_already_held() {
        let current = LocalProfile {
            avatar: Some("data:image/png;base64,AAAA".into()),
            banner: Some("data:image/png;base64,BBBB".into()),
            ..Default::default()
        };
        let local = sample().into_local(&current, None, Some(None));
        assert_eq!(local.avatar, current.avatar);
        assert_eq!(local.banner, None);
        assert_eq!(local.updated_at, 1_700_000_000_123);
    }

    #[test]
    fn unchanged_bytes_are_not_uploaded_again() {
        let picture = "data:image/png;base64,cG5n";
        let previous = Picture::Blob {
            url: "https://b.example/x.png".into(),
            sha256: blossom::sha256_hex(b"png"),
            mime: "image/png".into(),
        };
        let shared = block_on(share_picture(None, &key(1), Some(picture), &previous, 0));
        assert_eq!(shared, previous);
        let without_server = block_on(share_picture(
            None,
            &key(1),
            Some(picture),
            &Picture::None,
            0,
        ));
        assert_eq!(without_server, Picture::Unshared);
        assert_eq!(
            block_on(share_picture(None, &key(1), None, &previous, 0)),
            Picture::None
        );
    }

    #[test]
    fn an_unreachable_picture_is_kept_rather_than_blocking_the_rest() {
        let gone = Picture::Blob {
            url: "https://127.0.0.1:1/gone.png".into(),
            sha256: "ab".repeat(32),
            mime: "image/png".into(),
        };
        assert_eq!(block_on(resolve_with(&gone, &[Duration::ZERO])), None);
        assert_eq!(block_on(resolve_with(&Picture::None, &[])), Some(None));
        assert_eq!(block_on(resolve_with(&Picture::Unshared, &[])), None);
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(f)
    }
}
