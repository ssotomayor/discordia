//! The one holder of the FCM credential, so no self-hosted server needs it.
//! Nothing it forwards is readable to it.

pub mod auth;
pub mod fcm;
pub mod limits;
pub mod routes;
pub mod run;
pub mod sender;
pub mod store;
pub mod watcher;

use std::collections::HashSet;
use std::net::IpAddr;
use std::sync::Arc;

use discordia_protocol::push::{DmWrap, INLINE_WRAP_MAX_BYTES, PushData};

use crate::limits::Limits;
use crate::sender::{Delivery, Sender, short};
use crate::store::{Store, Target};

pub use routes::router;

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

pub struct Config {
    /// The host names phones dial this relay by; a signature naming any other is refused.
    pub audiences: HashSet<String>,
    pub trusted_proxies: Vec<IpAddr>,
    /// Relays the DM watcher may dial; `None` is its built-in list.
    pub dm_relays: Option<Vec<String>>,
    /// Check each new token with an FCM `validate_only` send. Off without credentials.
    pub validate_tokens: bool,
    /// Sends `/v1/push` may have waiting on FCM at once; past it the caller gets 503.
    pub max_in_flight: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            audiences: local_audiences(),
            trusted_proxies: Vec::new(),
            dm_relays: None,
            validate_tokens: true,
            max_in_flight: 512,
        }
    }
}

impl Config {
    /// What the DM watcher may dial.
    pub fn dm_allow_list(&self) -> Vec<String> {
        match &self.dm_relays {
            Some(list) => list.clone(),
            None => discordia_nostr::relay::DEFAULT_RELAYS
                .iter()
                .map(|r| r.to_string())
                .collect(),
        }
    }
}

/// Loopback by every name: what a relay on a developer's machine is dialled by.
pub fn local_audiences() -> HashSet<String> {
    ["127.0.0.1", "localhost", "[::1]"]
        .into_iter()
        .map(String::from)
        .collect()
}

pub struct Relay {
    pub store: Store,
    pub sender: Arc<dyn Sender>,
    pub config: Config,
    pub limits: Limits,
    in_flight: Arc<tokio::sync::Semaphore>,
}

/// What `push_dm` did, for the watcher's log and the tests.
#[derive(Debug, PartialEq, Eq)]
pub enum DmPush {
    /// The key has no unexpired device with DMs on.
    NoDevices,
    RateLimited,
    /// Sends attempted, and how many FCM accepted.
    Sent {
        devices: usize,
        delivered: usize,
    },
}

/// A gift wrap the watcher saw, and where.
pub struct DmEvent<'a> {
    pub id: &'a str,
    /// The event as JSON; the phone verifies it, so field order is free.
    pub json: &'a str,
    pub relay: &'a str,
}

impl Relay {
    pub fn new(store: Store, sender: Arc<dyn Sender>, config: Config, limits: Limits) -> Self {
        let in_flight = Arc::new(tokio::sync::Semaphore::new(config.max_in_flight));
        Self {
            store,
            sender,
            config,
            limits,
            in_flight,
        }
    }

    /// Sends to one device and drops it if FCM says the token is dead.
    pub async fn deliver(&self, target: &Target, data: &PushData) -> Delivery {
        let outcome = self.sender.send(&target.token, data, false).await;
        if outcome == Delivery::TokenGone {
            match self.store.delete_device_id(target.device) {
                Ok(_) => tracing::info!(
                    token = short(&target.token),
                    "token unregistered; device dropped"
                ),
                Err(e) => tracing::error!(?e, "could not drop an unregistered device"),
            }
        }
        outcome
    }

    pub(crate) fn try_in_flight(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        self.in_flight.clone().try_acquire_owned().ok()
    }

    /// The DM watcher's entry point: wakes every DM-enabled device of `pubkey`
    /// with the wrap inline when it fits FCM, else by id for the phone to fetch.
    pub async fn push_dm(&self, pubkey: &str, event: &DmEvent<'_>) -> Result<DmPush, String> {
        let targets = self
            .store
            .devices_of(pubkey, true, now_unix())
            .map_err(|e| format!("{e:?}"))?;
        if targets.is_empty() {
            return Ok(DmPush::NoDevices);
        }
        // Counted only for keys that have devices, so a flood at strangers grows nothing.
        if !self.limits.dm_per_key.admit(&pubkey.to_string()) {
            tracing::debug!(pubkey = short(pubkey), "DM push rate limited");
            return Ok(DmPush::RateLimited);
        }
        let wrap = if event.json.len() <= INLINE_WRAP_MAX_BYTES {
            DmWrap::Inline {
                e: event.json.to_string(),
            }
        } else {
            DmWrap::ById {
                id: event.id.to_string(),
                r: event.relay.to_string(),
            }
        };
        let data = PushData::Dm {
            p: pubkey.to_string(),
            wrap,
        };
        let mut delivered = 0;
        for target in &targets {
            if self.deliver(target, &data).await == Delivery::Sent {
                delivered += 1;
            }
        }
        Ok(DmPush::Sent {
            devices: targets.len(),
            delivered,
        })
    }

    /// `discordia-push-relay ping`: every device of the key, DMs on or not, past every limit.
    pub async fn ping(&self, pubkey: &str) -> Result<Vec<Delivery>, String> {
        let targets = self
            .store
            .devices_of(pubkey, false, now_unix())
            .map_err(|e| format!("{e:?}"))?;
        let mut out = Vec::with_capacity(targets.len());
        for target in &targets {
            out.push(self.deliver(target, &PushData::Ping).await);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sender::RecordingSender;
    use crate::store::Caps;
    use discordia_protocol::push::RegisterDevice;

    fn relay() -> (Relay, Arc<RecordingSender>) {
        let rec = Arc::new(RecordingSender::default());
        let relay = Relay::new(
            Store::in_memory(Caps::default()).unwrap(),
            rec.clone(),
            Config::default(),
            Limits::default(),
        );
        (relay, rec)
    }

    fn key(n: u8) -> String {
        format!("{n:02x}").repeat(32)
    }

    fn register(relay: &Relay, pubkey: &str, token: &str, dm: bool) {
        relay
            .store
            .upsert_device(
                pubkey,
                &RegisterDevice {
                    token: token.into(),
                    package: "com.discordia.app".into(),
                    dm,
                    relays: vec![],
                    replaces: None,
                },
                now_unix(),
            )
            .unwrap();
    }

    fn event(json: &str) -> DmEvent<'_> {
        DmEvent {
            id: "ev1",
            json,
            relay: "wss://relay.example",
        }
    }

    #[tokio::test]
    async fn a_wrap_up_to_the_threshold_rides_inline_and_a_larger_one_by_id() {
        let (relay, rec) = relay();
        register(&relay, &key(1), "t1", true);
        let fits = "x".repeat(INLINE_WRAP_MAX_BYTES);
        let over = "x".repeat(INLINE_WRAP_MAX_BYTES + 1);
        relay.push_dm(&key(1), &event(&fits)).await.unwrap();
        relay.push_dm(&key(1), &event(&over)).await.unwrap();
        let sent = rec.delivered();
        assert_eq!(
            sent[0].data,
            PushData::Dm {
                p: key(1),
                wrap: DmWrap::Inline { e: fits }
            }
        );
        assert_eq!(
            sent[1].data,
            PushData::Dm {
                p: key(1),
                wrap: DmWrap::ById {
                    id: "ev1".into(),
                    r: "wss://relay.example".into()
                }
            }
        );
    }

    #[tokio::test]
    async fn only_devices_with_dms_on_are_woken() {
        let (relay, rec) = relay();
        register(&relay, &key(1), "dm-on", true);
        register(&relay, &key(1), "dm-off", false);
        register(&relay, &key(2), "other", true);
        assert_eq!(
            relay.push_dm(&key(1), &event("{}")).await.unwrap(),
            DmPush::Sent {
                devices: 1,
                delivered: 1
            }
        );
        let tokens: Vec<_> = rec.delivered().into_iter().map(|r| r.token).collect();
        assert_eq!(tokens, ["dm-on"]);
        assert_eq!(
            relay.push_dm(&key(3), &event("{}")).await.unwrap(),
            DmPush::NoDevices
        );
    }

    #[tokio::test]
    async fn a_key_gets_thirty_dm_pushes_a_minute() {
        let (relay, rec) = relay();
        register(&relay, &key(1), "t", true);
        register(&relay, &key(2), "u", true);
        for _ in 0..30 {
            relay.push_dm(&key(1), &event("{}")).await.unwrap();
        }
        assert_eq!(
            relay.push_dm(&key(1), &event("{}")).await.unwrap(),
            DmPush::RateLimited
        );
        assert!(matches!(
            relay.push_dm(&key(2), &event("{}")).await.unwrap(),
            DmPush::Sent { .. }
        ));
        assert_eq!(rec.delivered().len(), 31);
    }

    #[tokio::test]
    async fn an_unregistered_token_loses_its_device() {
        let (relay, rec) = relay();
        register(&relay, &key(1), "gone", true);
        rec.answer(Delivery::TokenGone);
        relay.push_dm(&key(1), &event("{}")).await.unwrap();
        assert_eq!(relay.store.device_count().unwrap(), 0);
    }
}
