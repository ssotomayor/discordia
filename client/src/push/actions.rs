//! A DM notification's Reply and Mark as read when no account is running:
//! the process may have no Activity, so `headless`'s rules hold (trap 40).
//! A running account takes them instead (`dm_notifications`).

use std::path::Path;
use std::time::Duration;

use zeroize::Zeroizing;

use crate::nostr::event::Event;
use crate::nostr::relay::{Filter, RelayEvent, RelayPool};
use crate::nostr::{delivery, nip17};
use crate::protocol::is_canonical_pubkey;

/// The composer's cap.
pub const MAX_REPLY_CHARS: usize = 2000;
/// Waiting longer holds the notification's spinner; the outbox already has the reply.
const PUBLISH_PATIENCE: Duration = Duration::from_secs(6);
const INBOX_PATIENCE: Duration = Duration::from_secs(2);
const SUB_INBOX: &str = "reply-inbox";

/// Marks `peer` read up to `now`, which the app syncs at its next start. A
/// `settings.json` that cannot be read is left alone, never replaced by defaults.
pub fn mark_read(dir: &Path, peer: &str, now: i64) -> Result<(), String> {
    if !is_canonical_pubkey(peer) {
        return Err("not a key".into());
    }
    let path = dir.join("settings.json");
    let mut settings = match std::fs::read_to_string(&path) {
        Ok(content) => crate::settings::parse(&content).ok_or("unreadable settings")?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Default::default(),
        Err(error) => return Err(error.to_string()),
    };
    if settings.mark_dm_read(peer, now) {
        crate::settings::save_in(dir, &settings)?;
    }
    Ok(())
}

/// Signs `text` to `peer` and queues it in the account's outbox before trying
/// to deliver it, so a reply the relays did not take goes at the app's next start.
pub fn reply(
    dir: &Path,
    account: &str,
    peer: &str,
    text: &str,
    now: i64,
    passphrase: impl FnOnce() -> Result<String, String>,
) -> Result<(), String> {
    let text = text.trim();
    if text.is_empty() || text.chars().count() > MAX_REPLY_CHARS {
        return Err("no reply to send".into());
    }
    if !is_canonical_pubkey(peer) || crate::identity::active_in(dir).as_deref() != Some(account) {
        return Err("not this device's account".into());
    }
    let locked = crate::identity::ncryptsec_in(dir, account).ok_or("no key")?;
    if crate::keyvault::cost(&locked).is_none_or(|cost| cost > super::headless::MAX_KEY_COST) {
        return Err("key too costly to open here".into());
    }
    let passphrase = Zeroizing::new(passphrase()?);
    let secret =
        super::headless::unlock(&locked, &passphrase, account).ok_or("the key did not open")?;
    let rumor = nip17::chat_rumor(account, peer, text, None, &[], now);
    let (theirs, ours) = nip17::wrap_both(&secret, peer, &rumor, now)?;
    delivery::queue_in(dir, account, peer, &theirs, &ours)?;
    if let Err(error) = mark_read(dir, peer, now) {
        tracing::warn!(%error, "reply sent, but the conversation was not marked read");
    }
    let relays = crate::features::account::dm_relays(&super::headless::settings_in(dir));
    if let Err(error) = block_on(deliver(peer.to_string(), theirs, ours, relays)) {
        tracing::info!(%error, "reply queued for the app's next start");
    }
    Ok(())
}

fn block_on(work: impl std::future::Future<Output = Result<(), String>>) -> Result<(), String> {
    if tokio::runtime::Handle::try_current().is_ok() {
        return Err("already inside an async runtime".into());
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(async {
        tokio::time::timeout(PUBLISH_PATIENCE, work)
            .await
            .map_err(|_| "no relay accepted it in time".to_string())?
    })
}

/// Ours to our relays; theirs to the peer's inbox list (kind 10050), else to ours.
async fn deliver(
    peer: String,
    theirs: Event,
    ours: Event,
    relays: Vec<String>,
) -> Result<(), String> {
    let (pool, mut events) = RelayPool::connect(relays.clone());
    pool.publish(ours);
    pool.subscribe(
        SUB_INBOX,
        vec![Filter {
            kinds: Some(vec![nip17::KIND_DM_RELAYS]),
            authors: Some(vec![peer.clone()]),
            limit: Some(1),
            ..Default::default()
        }],
    );
    let mut routes = delivery::Routes::default();
    let _ = tokio::time::timeout(INBOX_PATIENCE, async {
        while let Some(event) = events.recv().await {
            match event {
                RelayEvent::Event { event, .. } => {
                    routes.note(&event);
                    if event.kind == nip17::KIND_DM_RELAYS && event.pubkey == peer {
                        return;
                    }
                }
                RelayEvent::EndOfStored { subscription, .. } if subscription == SUB_INBOX => {
                    return;
                }
                _ => {}
            }
        }
    })
    .await;
    let targets = routes.targets(&peer, &relays);
    let (inbox, mut answers) = if targets == relays {
        (pool.clone(), events)
    } else {
        RelayPool::connect(targets)
    };
    let id = theirs.id.clone();
    inbox.publish(theirs);
    while let Some(event) = answers.recv().await {
        if let RelayEvent::Published {
            id: answered,
            accepted: true,
            ..
        } = event
            && answered == id
        {
            return Ok(());
        }
    }
    Err("every relay closed".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_read_mark_joins_the_saved_settings_and_only_moves_forward() {
        let dir = crate::identity::test_config_dir();
        let peer = "b".repeat(64);
        let saved = crate::settings::ClientSettings {
            dm_relays: vec!["wss://kept.example".into()],
            ..Default::default()
        };
        crate::settings::save_in(&dir, &saved).unwrap();
        mark_read(&dir, &peer, 200).unwrap();
        mark_read(&dir, &peer, 100).unwrap();
        let read = |dir: &Path| {
            crate::settings::parse(&std::fs::read_to_string(dir.join("settings.json")).unwrap())
                .unwrap()
        };
        let settings = read(&dir);
        assert_eq!(settings.dm_read_at, [(peer.clone(), 200)]);
        assert_eq!(settings.dm_relays, ["wss://kept.example"]);
    }

    #[test]
    fn an_unreadable_settings_file_is_not_replaced() {
        let dir = crate::identity::test_config_dir();
        std::fs::write(dir.join("settings.json"), "not json").unwrap();
        assert!(mark_read(&dir, &"b".repeat(64), 1).is_err());
        assert_eq!(
            std::fs::read_to_string(dir.join("settings.json")).unwrap(),
            "not json"
        );
    }

    #[test]
    fn a_reply_needs_the_device_account_and_some_text() {
        let dir = crate::identity::test_config_dir();
        let peer = "b".repeat(64);
        let never = || -> Result<String, String> { panic!("no key is opened for a refused reply") };
        assert!(reply(&dir, &"a".repeat(64), &peer, "  ", 1, never).is_err());
        assert!(reply(&dir, &"a".repeat(64), &peer, "hi", 1, never).is_err());
    }
}
