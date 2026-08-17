//! A small Nostr relay client: connect, subscribe, publish, reconnect.
//!
//! Nothing in this repo had ever spoken to a relay before. This is the piece
//! that makes direct messages independent of whoever is hosting the guild —
//! the gateway carries none of it, and a friend on another server reaches the
//! same relays and the same conversation.
//!
//! The wire protocol is small enough to write out. We send:
//!
//! - `["EVENT", <event>]` to publish
//! - `["REQ", <sub-id>, <filter>]` to subscribe
//! - `["CLOSE", <sub-id>]` to stop
//!
//! and read back `EVENT`, `OK`, `EOSE`, `CLOSED` and `NOTICE`.
//!
//! **The pool is the unit, not the connection.** A relay is allowed to be down,
//! slow, or to have never heard of you; the answer to all three is the other
//! relays. So publishing goes to all of them and succeeds if any accepts, and
//! subscriptions run on all of them with results deduplicated by event id. That
//! is also the reason a failed connection is logged and retried rather than
//! surfaced — one relay refusing is not a condition the user can act on.

#![allow(dead_code)]

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, mpsc};

use super::event::Event;

/// Relays used when the user has not chosen any.
///
/// Deliberately several and deliberately unaffiliated: a single default would
/// make one operator able to see every Discordia user's DM metadata, which is
/// the shape of the problem moving off the host was meant to solve.
pub const DEFAULT_RELAYS: &[&str] = &[
    "wss://relay.damus.io",
    "wss://nos.lol",
    "wss://relay.primal.net",
    "wss://relay.nostr.band",
];

/// How long to wait before redialling, and the ceiling it backs off to.
const RECONNECT_MIN: Duration = Duration::from_secs(2);
const RECONNECT_MAX: Duration = Duration::from_secs(60);

/// What to ask a relay for.
///
/// Only the fields this client needs. `#p` is the tag filter that makes gift
/// wrap work at all: a wrap is addressed to us by a `p` tag and signed by a key
/// we have never seen, so the tag is the *only* thing we can select on.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Filter {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kinds: Option<Vec<u16>>,
    #[serde(rename = "#p", skip_serializing_if = "Option::is_none")]
    pub p: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authors: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// What the pool reports back to the app.
#[derive(Debug, Clone)]
pub enum RelayEvent {
    /// An event nobody has delivered before, from `relay`.
    Event {
        relay: String,
        event: Box<Event>,
    },
    /// A relay finished replaying stored events for a subscription.
    EndOfStored {
        relay: String,
    },
    /// A relay accepted or rejected something we published.
    Published {
        relay: String,
        id: String,
        accepted: bool,
        message: String,
    },
    /// Connection state, for a status line rather than for logic.
    Connected(String),
    Disconnected {
        relay: String,
        why: String,
    },
}

enum Command {
    Publish(Box<Event>),
    Subscribe(Vec<Filter>),
}

/// A set of relay connections, driven as one.
#[derive(Clone)]
pub struct RelayPool {
    cmd: mpsc::UnboundedSender<Command>,
}

impl RelayPool {
    /// Connect to `urls` and start delivering events on the returned channel.
    ///
    /// Each relay gets its own task and its own reconnect clock; a relay that
    /// never comes up costs nothing but its own retries.
    pub fn connect(urls: Vec<String>) -> (RelayPool, mpsc::UnboundedReceiver<RelayEvent>) {
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<Command>();
        let (out_tx, out_rx) = mpsc::unbounded_channel::<RelayEvent>();

        // One shared record of what has been delivered, so the same event
        // arriving from four relays reaches the app once. Bounded by eviction
        // below rather than growing for the life of the session.
        let seen: Arc<Mutex<SeenSet>> = Arc::new(Mutex::new(SeenSet::default()));
        // The current subscription, kept so a relay that reconnects — or one
        // that was down when it was issued — can pick it up.
        let filter: Arc<Mutex<Option<Vec<Filter>>>> = Arc::new(Mutex::new(None));

        let mut senders = Vec::new();
        for url in urls {
            let (relay_tx, relay_rx) = mpsc::unbounded_channel::<Command>();
            senders.push(relay_tx);
            tokio::spawn(run_relay(
                url,
                relay_rx,
                out_tx.clone(),
                Arc::clone(&seen),
                Arc::clone(&filter),
            ));
        }

        // Fan commands out to every relay, and remember the subscription.
        tokio::spawn(async move {
            while let Some(cmd) = cmd_rx.recv().await {
                if let Command::Subscribe(f) = &cmd {
                    *filter.lock().await = Some(f.clone());
                }
                for s in &senders {
                    let copy = match &cmd {
                        Command::Publish(e) => Command::Publish(e.clone()),
                        Command::Subscribe(f) => Command::Subscribe(f.clone()),
                    };
                    let _ = s.send(copy);
                }
            }
        });

        (RelayPool { cmd: cmd_tx }, out_rx)
    }

    /// Publish to every relay. Returns immediately; acceptance arrives as
    /// `RelayEvent::Published`.
    pub fn publish(&self, event: Event) {
        let _ = self.cmd.send(Command::Publish(Box::new(event)));
    }

    /// Replace the standing subscription on every relay.
    ///
    /// Several filters rather than one, because a `REQ` ORs them and the two
    /// things this client wants are unrelated: gift wraps addressed to us, and
    /// our own replaceable events (the contact list, the DM relay list). One
    /// filter cannot express both without also matching everything in between.
    pub fn subscribe(&self, filters: Vec<Filter>) {
        let _ = self.cmd.send(Command::Subscribe(filters));
    }
}

/// Event ids already delivered, with a bound.
///
/// A session that runs for days would otherwise accumulate one id per message
/// per relay forever. The cap is generous and the eviction is crude — clear
/// half when full — because the only cost of forgetting an id is delivering a
/// duplicate the layer above already tolerates.
#[derive(Default)]
struct SeenSet {
    ids: HashSet<String>,
    order: Vec<String>,
}

impl SeenSet {
    const CAP: usize = 8192;

    /// True if this id is new, recording it.
    fn insert_new(&mut self, id: &str) -> bool {
        if self.ids.contains(id) {
            return false;
        }
        if self.order.len() >= Self::CAP {
            for old in self.order.drain(..Self::CAP / 2) {
                self.ids.remove(&old);
            }
        }
        self.ids.insert(id.to_string());
        self.order.push(id.to_string());
        true
    }
}

/// One relay: dial, serve commands, read messages, redial.
async fn run_relay(
    url: String,
    mut cmds: mpsc::UnboundedReceiver<Command>,
    out: mpsc::UnboundedSender<RelayEvent>,
    seen: Arc<Mutex<SeenSet>>,
    filter: Arc<Mutex<Option<Vec<Filter>>>>,
) {
    let mut backoff = RECONNECT_MIN;
    // Commands that arrived while this relay was down. Only the newest
    // subscription matters, and publishes are dropped rather than queued —
    // another relay almost certainly took them, and a message that turns up
    // ten minutes late because one relay came back is worse than one that did
    // not go to that relay at all.
    loop {
        match tokio_tungstenite::connect_async(&url).await {
            Ok((stream, _)) => {
                backoff = RECONNECT_MIN;
                let _ = out.send(RelayEvent::Connected(url.clone()));
                let why = serve(&url, stream, &mut cmds, &out, &seen, &filter).await;
                let _ = out.send(RelayEvent::Disconnected {
                    relay: url.clone(),
                    why,
                });
            }
            Err(e) => {
                let _ = out.send(RelayEvent::Disconnected {
                    relay: url.clone(),
                    why: e.to_string(),
                });
            }
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(RECONNECT_MAX);
    }
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Pump one live connection until it fails. Returns why it ended.
async fn serve(
    url: &str,
    stream: Ws,
    cmds: &mut mpsc::UnboundedReceiver<Command>,
    out: &mpsc::UnboundedSender<RelayEvent>,
    seen: &Arc<Mutex<SeenSet>>,
    filter: &Arc<Mutex<Option<Vec<Filter>>>>,
) -> String {
    use tokio_tungstenite::tungstenite::Message;
    let (mut tx, mut rx) = stream.split();
    let sub_id = "dxf-dm";

    // Re-issue the standing subscription. This is what makes a reconnect
    // invisible: the relay has no memory of us, so a fresh connection with no
    // REQ is a connection that silently receives nothing.
    if let Some(f) = filter.lock().await.clone() {
        let mut req = vec![serde_json::json!("REQ"), serde_json::json!(sub_id)];
        req.extend(
            f.iter()
                .map(|x| serde_json::to_value(x).unwrap_or_default()),
        );
        let req = serde_json::Value::Array(req).to_string();
        if tx.send(Message::Text(req)).await.is_err() {
            return "could not send the subscription".into();
        }
    }

    loop {
        tokio::select! {
            cmd = cmds.recv() => match cmd {
                Some(Command::Publish(e)) => {
                    let msg = serde_json::json!(["EVENT", &*e]).to_string();
                    if tx.send(Message::Text(msg)).await.is_err() {
                        return "send failed".into();
                    }
                }
                Some(Command::Subscribe(f)) => {
                    let mut parts = vec![serde_json::json!("REQ"), serde_json::json!(sub_id)];
                    parts.extend(f.iter().map(|x| serde_json::to_value(x).unwrap_or_default()));
                    let req = serde_json::Value::Array(parts).to_string();
                    if tx.send(Message::Text(req)).await.is_err() {
                        return "send failed".into();
                    }
                }
                None => return "pool shut down".into(),
            },
            msg = rx.next() => match msg {
                Some(Ok(Message::Text(text))) => {
                    handle_relay_message(url, &text, out, seen).await;
                }
                Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Binary(_) | Message::Frame(_))) => {}
                Some(Ok(Message::Close(_))) => return "relay closed the connection".into(),
                Some(Err(e)) => return e.to_string(),
                None => return "stream ended".into(),
            },
        }
    }
}

/// Parse one relay frame and report it.
///
/// Anything unrecognised is ignored rather than treated as an error: relays
/// send `NOTICE`s and extensions we have no opinion about, and disconnecting
/// over one would make this client fragile against relays it works with fine.
async fn handle_relay_message(
    url: &str,
    text: &str,
    out: &mpsc::UnboundedSender<RelayEvent>,
    seen: &Arc<Mutex<SeenSet>>,
) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return;
    };
    let Some(kind) = v.get(0).and_then(|k| k.as_str()) else {
        return;
    };
    match kind {
        "EVENT" => {
            let Some(raw) = v.get(2) else { return };
            let Ok(event) = serde_json::from_value::<Event>(raw.clone()) else {
                return;
            };
            // Verified here, once, before anything downstream sees it. A relay
            // can send whatever it likes; an event that does not verify is not
            // a message, it is a forgery attempt.
            if !event.verify() {
                return;
            }
            if !seen.lock().await.insert_new(&event.id) {
                return;
            }
            let _ = out.send(RelayEvent::Event {
                relay: url.to_string(),
                event: Box::new(event),
            });
        }
        "EOSE" => {
            let _ = out.send(RelayEvent::EndOfStored {
                relay: url.to_string(),
            });
        }
        "OK" => {
            let id = v.get(1).and_then(|s| s.as_str()).unwrap_or_default();
            let accepted = v.get(2).and_then(|b| b.as_bool()).unwrap_or(false);
            let message = v
                .get(3)
                .and_then(|s| s.as_str())
                .unwrap_or_default()
                .to_string();
            let _ = out.send(RelayEvent::Published {
                relay: url.to_string(),
                id: id.to_string(),
                accepted,
                message,
            });
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The filter has to serialize to exactly what a relay expects, and `#p` is
    /// the field a Rust identifier cannot spell — so it is the one worth
    /// pinning. Absent fields must be absent, not null: a relay reading
    /// `"kinds": null` may reject the whole REQ.
    #[test]
    fn a_filter_serializes_the_way_relays_read_it() {
        let f = Filter {
            kinds: Some(vec![1059]),
            p: Some(vec!["abc".into()]),
            limit: Some(100),
            ..Default::default()
        };
        let json = serde_json::to_string(&f).expect("serialize");
        // `r##` rather than `r#`: the filter's own `"#p"` would otherwise
        // close a `r#"` literal in the middle of the expected string.
        assert_eq!(json, r##"{"kinds":[1059],"#p":["abc"],"limit":100}"##);
        assert!(!json.contains("null"), "absent fields must be omitted");
    }

    /// Deduplication is what stops four relays delivering one message four
    /// times.
    #[test]
    fn the_same_event_is_delivered_once() {
        let mut seen = SeenSet::default();
        assert!(seen.insert_new("a"));
        assert!(!seen.insert_new("a"));
        assert!(seen.insert_new("b"));
    }

    /// And the record is bounded, so a long session does not grow forever.
    #[test]
    fn the_seen_set_is_bounded() {
        let mut seen = SeenSet::default();
        for i in 0..SeenSet::CAP + 100 {
            seen.insert_new(&i.to_string());
        }
        assert!(
            seen.ids.len() <= SeenSet::CAP,
            "seen set grew to {}",
            seen.ids.len()
        );
        // The most recent are still remembered, which is what matters.
        assert!(!seen.insert_new(&(SeenSet::CAP + 99).to_string()));
    }
}
