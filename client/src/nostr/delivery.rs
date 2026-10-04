use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use super::event::Event;
use super::relay::{RelayEvent, RelayPool};
use crate::protocol::Id;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    Pending,
    Accepted,
    Failed,
}

pub struct Pending {
    pub peer: String,
    pub theirs: Event,
    pub ours: Event,
    pub due: Instant,
    pub started: bool,
}

#[derive(Default)]
pub struct Routes(HashMap<String, (i64, String, Vec<String>)>);

impl Routes {
    pub fn note(&mut self, event: &Event) {
        if event.kind != super::nip17::KIND_DM_RELAYS {
            return;
        }
        let replace = self.0.get(&event.pubkey).is_none_or(|(ts, id, _)| {
            event.created_at > *ts || (event.created_at == *ts && event.id < *id)
        });
        if replace {
            let mut urls = super::nip17::parse_dm_relay_list(event);
            urls.retain(|url| valid_relay(url));
            urls.sort();
            urls.dedup();
            urls.truncate(8);
            self.0.insert(
                event.pubkey.clone(),
                (event.created_at, event.id.clone(), urls),
            );
        }
    }

    pub fn targets(&self, peer: &str, fallback: &[String]) -> Vec<String> {
        self.0
            .get(peer)
            .map(|(_, _, urls)| urls.clone())
            .unwrap_or_else(|| fallback.iter().take(8).cloned().collect())
    }
}

fn valid_relay(raw: &str) -> bool {
    let Ok(url) = url::Url::parse(raw) else {
        return false;
    };
    url.scheme() == "wss"
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
}

pub async fn publish(event: Event, urls: Vec<String>) -> Delivery {
    if urls.is_empty() {
        return Delivery::Failed;
    }
    let expected: HashSet<_> = urls.iter().cloned().collect();
    let mut rejected = HashSet::new();
    let (pool, mut events) = RelayPool::connect(urls);
    pool.publish(event.clone());
    let deadline = tokio::time::sleep(Duration::from_secs(15));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => return Delivery::Failed,
            update = events.recv() => match update {
                Some(RelayEvent::Published { relay, id, accepted, .. }) if id == event.id && expected.contains(&relay) => {
                    if accepted { return Delivery::Accepted; }
                    rejected.insert(relay);
                    if rejected.len() == expected.len() { return Delivery::Failed; }
                }
                None => return Delivery::Failed,
                _ => {}
            }
        }
    }
}

pub async fn publish_call(event: Event, urls: Vec<String>, expires_at: i64) -> Delivery {
    let targets: HashSet<_> = urls.into_iter().collect();
    let results = futures_util::future::join_all(targets.into_iter().map(|url| {
        let event = event.clone();
        async move {
            for attempt in 0..2 {
                if chrono::Utc::now().timestamp() >= expires_at {
                    break;
                }
                let remaining = expires_at
                    .saturating_sub(chrono::Utc::now().timestamp())
                    .max(0) as u64;
                if tokio::time::timeout(
                    Duration::from_secs(remaining),
                    publish(event.clone(), vec![url.clone()]),
                )
                .await
                    == Ok(Delivery::Accepted)
                {
                    return Delivery::Accepted;
                }
                if attempt == 0 {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
            Delivery::Failed
        }
    }))
    .await;
    if results.contains(&Delivery::Accepted) {
        Delivery::Accepted
    } else {
        Delivery::Failed
    }
}

pub type Outbox = HashMap<Id, Pending>;

#[derive(serde::Serialize, serde::Deserialize)]
struct Stored {
    peer: String,
    theirs: Event,
    ours: Event,
}

fn path(pubkey: &str) -> std::path::PathBuf {
    crate::identity::config_dir().join(format!("dm-outbox-{pubkey}.json"))
}

pub async fn load(pubkey: &str) -> Vec<(String, Event, Event)> {
    let path = path(pubkey);
    tokio::task::spawn_blocking(move || load_from(path))
        .await
        .unwrap_or_default()
}

fn load_from(path: std::path::PathBuf) -> Vec<(String, Event, Event)> {
    let Ok(meta) = std::fs::metadata(&path) else {
        return Vec::new();
    };
    if meta.len() > 4 * 1024 * 1024 {
        return Vec::new();
    }
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(records) = serde_json::from_str::<Vec<Stored>>(&raw) else {
        return Vec::new();
    };
    records
        .into_iter()
        .take(128)
        .filter(|record| record.ours.verify() && record.theirs.verify())
        .map(|record| (record.peer, record.theirs, record.ours))
        .collect()
}

pub async fn persist(pubkey: &str, outbox: &Outbox) -> Result<(), String> {
    let stored: Vec<_> = outbox
        .values()
        .map(|entry| Stored {
            peer: entry.peer.clone(),
            theirs: entry.theirs.clone(),
            ours: entry.ours.clone(),
        })
        .collect();
    let raw = serde_json::to_string(&stored).map_err(|error| error.to_string())?;
    let path = path(pubkey);
    tokio::task::spawn_blocking(move || crate::identity::write_private(&path, &raw))
        .await
        .map_err(|error| error.to_string())?
}

#[cfg(test)]
mod tests {
    use super::super::{event, nip17};
    use super::*;

    #[test]
    fn newest_signed_recipient_list_wins_and_empty_does_not_fall_back() {
        let secret = secp256k1::SecretKey::from_slice(&[7; 32]).unwrap();
        let peer = event::xonly_hex(&secret);
        let mut routes = Routes::default();
        let list = |urls: &[&str], ts| {
            nip17::dm_relay_list(
                &secret,
                &urls.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                ts,
            )
        };
        routes.note(&list(
            &[
                "wss://recipient.example",
                "file:///tmp/key",
                "wss://recipient.example",
            ],
            2,
        ));
        routes.note(&list(&["wss://stale.example"], 1));
        assert_eq!(
            routes.targets(&peer, &["wss://sender.example".into()]),
            ["wss://recipient.example"]
        );
        routes.note(&list(&[], 3));
        assert!(
            routes
                .targets(&peer, &["wss://sender.example".into()])
                .is_empty()
        );
    }

    async fn fake_relay(accepted: bool) -> (String, tokio::task::JoinHandle<()>) {
        fake_relay_delayed(accepted, Duration::ZERO).await
    }

    async fn fake_relay_delayed(
        accepted: bool,
        delay: Duration,
    ) -> (String, tokio::task::JoinHandle<()>) {
        use futures_util::{SinkExt, StreamExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            tokio::time::sleep(delay).await;
            while let Some(Ok(tokio_tungstenite::tungstenite::Message::Text(raw))) =
                socket.next().await
            {
                let frame: serde_json::Value = serde_json::from_str(&raw).unwrap();
                if frame[0] == "EVENT" {
                    let response =
                        serde_json::json!(["OK", frame[1]["id"], accepted, "test"]).to_string();
                    socket
                        .send(tokio_tungstenite::tungstenite::Message::Text(response))
                        .await
                        .unwrap();
                    break;
                }
            }
        });
        (url, task)
    }

    #[tokio::test]
    async fn call_signals_reach_slower_relays_after_the_first_acceptance() {
        let secret = secp256k1::SecretKey::from_slice(&[7; 32]).unwrap();
        let event = super::super::event::sign_with(&secret, 123, 1059, vec![], "call".into());
        let (fast, fast_task) = fake_relay(true).await;
        let (slow, slow_task) = fake_relay_delayed(true, Duration::from_millis(150)).await;
        assert_eq!(
            publish_call(event, vec![fast, slow], chrono::Utc::now().timestamp() + 60).await,
            Delivery::Accepted
        );
        fast_task.await.unwrap();
        slow_task.await.unwrap();
    }

    #[tokio::test]
    async fn call_publication_retries_the_same_event_after_rejection() {
        use futures_util::{SinkExt, StreamExt};
        let secret = secp256k1::SecretKey::from_slice(&[7; 32]).unwrap();
        let event = super::super::event::sign_with(&secret, 123, 1059, vec![], "cancel".into());
        let id = event.id.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            for accepted in [false, true] {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                let raw = socket.next().await.unwrap().unwrap().into_text().unwrap();
                let frame: serde_json::Value = serde_json::from_str(&raw).unwrap();
                assert_eq!(frame[0], "EVENT");
                assert_eq!(frame[1]["id"], id);
                socket
                    .send(tokio_tungstenite::tungstenite::Message::Text(
                        serde_json::json!(["OK", id, accepted, "test"]).to_string(),
                    ))
                    .await
                    .unwrap();
            }
        });
        assert_eq!(
            publish_call(event, vec![url], chrono::Utc::now().timestamp() + 60).await,
            Delivery::Accepted
        );
        task.await.unwrap();
    }

    #[tokio::test]
    async fn recipient_acceptance_and_all_rejections_have_distinct_results() {
        let secret = secp256k1::SecretKey::from_slice(&[7; 32]).unwrap();
        let event = super::super::event::sign_with(&secret, 123, 1, vec![], "test".into());
        let (rejected, task1) = fake_relay(false).await;
        let (accepted, task2) = fake_relay(true).await;
        assert_eq!(
            publish(event.clone(), vec![rejected, accepted]).await,
            Delivery::Accepted
        );
        task1.await.unwrap();
        task2.await.unwrap();
        let (rejected, task) = fake_relay(false).await;
        assert_eq!(publish(event, vec![rejected]).await, Delivery::Failed);
        task.await.unwrap();
        assert_eq!(
            publish(
                super::super::event::sign_with(&secret, 124, 1, vec![], "test".into()),
                vec![]
            )
            .await,
            Delivery::Failed
        );
    }
}
