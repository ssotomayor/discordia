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

use std::collections::{BTreeMap, HashSet};
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
    /// An event nobody has delivered before. Which relay carried it is not
    /// part of this: the pool deduplicates by event id, so "first to arrive"
    /// is a race rather than a fact about the message. The one place the
    /// source matters — a relay serving something that does not verify — names
    /// it at the point of the drop instead.
    Event(Box<Event>),
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
    Subscribe { id: String, filters: Vec<Filter> },
}

/// Standing subscriptions, by the id the relay knows them under.
///
/// A `REQ` replaces whatever the relay held under that id, which is what makes
/// this a map rather than one slot: the DM subscription and the metadata one
/// want different lifetimes — wraps are asked for once, names are re-asked
/// every time a new peer appears — and sharing an id meant the second `REQ`
/// silently cancelled the first. Ordered so a reconnect re-issues them the same
/// way every time.
type Subscriptions = BTreeMap<String, Vec<Filter>>;

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

        // Dedup across relays; bounded by eviction rather than growing for the
        // session.
        let seen: Arc<Mutex<SeenSet>> = Arc::new(Mutex::new(SeenSet::default()));
        // The standing subscriptions, kept so a relay that reconnects — or one
        // that was down when they were issued — can pick them up.
        let subs: Arc<Mutex<Subscriptions>> = Arc::new(Mutex::new(Subscriptions::new()));

        let mut senders = Vec::new();
        for url in urls {
            let (relay_tx, relay_rx) = mpsc::unbounded_channel::<Command>();
            senders.push(relay_tx);
            tokio::spawn(run_relay(
                url,
                relay_rx,
                out_tx.clone(),
                Arc::clone(&seen),
                Arc::clone(&subs),
            ));
        }

        tokio::spawn(async move {
            while let Some(cmd) = cmd_rx.recv().await {
                if let Command::Subscribe { id, filters } = &cmd {
                    subs.lock().await.insert(id.clone(), filters.clone());
                }
                for s in &senders {
                    let copy = match &cmd {
                        Command::Publish(e) => Command::Publish(e.clone()),
                        Command::Subscribe { id, filters } => Command::Subscribe {
                            id: id.clone(),
                            filters: filters.clone(),
                        },
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

    /// Open or replace the subscription `id` on every relay.
    ///
    /// Several filters rather than one, because a `REQ` ORs them and the things
    /// one subscription wants can be unrelated: gift wraps addressed to us, and
    /// our own replaceable events (the contact list, the DM relay list). One
    /// filter cannot express both without also matching everything in between.
    ///
    /// **`id` is the unit of replacement**, on the relay and here — re-issuing
    /// one leaves the others running, and re-issuing the same one replays its
    /// stored events. That is why the metadata subscription has an id of its
    /// own: it is re-asked whenever a new peer appears, and sharing an id with
    /// the DM filters would have replayed every gift wrap each time.
    pub fn subscribe(&self, id: &str, filters: Vec<Filter>) {
        let _ = self.cmd.send(Command::Subscribe {
            id: id.to_string(),
            filters,
        });
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
    subs: Arc<Mutex<Subscriptions>>,
) {
    let mut backoff = RECONNECT_MIN;
    // Only the newest subscription per id matters; publishes are dropped rather
    // than queued to avoid stale delivery.
    loop {
        match tokio_tungstenite::connect_async(&url).await {
            Ok((stream, _)) => {
                backoff = RECONNECT_MIN;
                let _ = out.send(RelayEvent::Connected(url.clone()));
                let why = serve(&url, stream, &mut cmds, &out, &seen, &subs).await;
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
    subs: &Arc<Mutex<Subscriptions>>,
) -> String {
    use tokio_tungstenite::tungstenite::Message;
    let (mut tx, mut rx) = stream.split();

    // Re-issue every standing subscription; a fresh connection without a REQ
    // receives nothing.
    for (id, filters) in subs.lock().await.iter() {
        if tx
            .send(Message::Text(req_frame(id, filters)))
            .await
            .is_err()
        {
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
                Some(Command::Subscribe { id, filters }) => {
                    if tx.send(Message::Text(req_frame(&id, &filters))).await.is_err() {
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

/// `["REQ", <id>, <filter>…]` — the frame that opens or replaces a
/// subscription.
fn req_frame(id: &str, filters: &[Filter]) -> String {
    let mut parts = vec![serde_json::json!("REQ"), serde_json::json!(id)];
    parts.extend(
        filters
            .iter()
            .map(|f| serde_json::to_value(f).unwrap_or_default()),
    );
    serde_json::Value::Array(parts).to_string()
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
            // Verify once here; unverified events are forgery attempts, and a
            // relay serving them is worth flagging.
            if !event.verify() {
                eprintln!(
                    "[nostr] {url}: dropped an event that does not verify (id {})",
                    event.id
                );
                return;
            }
            if !seen.lock().await.insert_new(&event.id) {
                return;
            }
            let _ = out.send(RelayEvent::Event(Box::new(event)));
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

    /// The id is what a relay replaces on, so a frame that carries the wrong
    /// one cancels a subscription somebody else is relying on. Two ids, two
    /// frames, and the filters ride along in order.
    ///
    /// The keys *within* a filter come out alphabetical rather than in
    /// declaration order, because the frame is built through
    /// `serde_json::Value` and its map is sorted. Pinned rather than corrected:
    /// a JSON object has no order to a relay, and the test above already fixes
    /// the shape a filter serializes to on its own.
    #[test]
    fn a_req_frame_names_its_own_subscription() {
        let dm = Filter {
            kinds: Some(vec![1059]),
            ..Default::default()
        };
        let meta = Filter {
            kinds: Some(vec![0]),
            authors: Some(vec!["abc".into()]),
            ..Default::default()
        };

        assert_eq!(
            req_frame("dxf-dm", std::slice::from_ref(&dm)),
            r#"["REQ","dxf-dm",{"kinds":[1059]}]"#
        );
        assert_eq!(
            req_frame("dxf-meta", &[meta]),
            r#"["REQ","dxf-meta",{"authors":["abc"],"kinds":[0]}]"#
        );
        // Several filters on one id are ORed by the relay, and their order is
        // ours to keep.
        assert!(req_frame("dxf-dm", &[dm.clone(), dm]).starts_with(r#"["REQ","dxf-dm","#));
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
        assert!(!seen.insert_new(&(SeenSet::CAP + 99).to_string()));
    }

    /// Drive one frame through the handler and collect whatever it emitted.
    ///
    /// `handle_relay_message` is the whole of what a relay can make this client
    /// believe, and until now nothing exercised it: every test above this point
    /// covers a helper it calls rather than the decision itself.
    async fn feed(frames: &[&str]) -> Vec<RelayEvent> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let seen = Arc::new(Mutex::new(SeenSet::default()));
        for f in frames {
            handle_relay_message("wss://r.test", f, &tx, &seen).await;
        }
        drop(tx);
        let mut out = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            out.push(ev);
        }
        out
    }

    fn signed_event() -> Event {
        let secret = secp256k1::SecretKey::from_slice(&[7; 32]).expect("valid key");
        super::super::event::sign_with(&secret, 1_700_000_000, 1, vec![], "hello".into())
    }

    /// A relay can send whatever it likes, and the signature is the only thing
    /// that makes an event a message rather than a claim. This is the guard the
    /// gift wrap's deniability rests on, so it gets a test of its own.
    #[tokio::test]
    async fn an_event_that_does_not_verify_never_reaches_the_app() {
        let mut forged = signed_event();
        forged.content = "hello, but rewritten in flight".into();
        let frame = serde_json::json!(["EVENT", "sub", forged]).to_string();

        assert!(
            feed(&[&frame]).await.is_empty(),
            "a rewritten event must not be delivered"
        );
    }

    /// And the honest one does arrive — once, however many relays repeat it.
    #[tokio::test]
    async fn a_verifying_event_arrives_exactly_once() {
        let frame = serde_json::json!(["EVENT", "sub", signed_event()]).to_string();
        let out = feed(&[&frame, &frame]).await;

        assert_eq!(out.len(), 1, "the duplicate must be dropped");
        assert!(matches!(out[0], RelayEvent::Event(_)));
    }

    /// A rejected publish used to be parsed and then discarded by the app. The
    /// reason a relay gives is the only account of why a message did not land,
    /// so it has to survive the trip.
    #[tokio::test]
    async fn a_rejection_keeps_the_reason_the_relay_gave() {
        let out = feed(&[r#"["OK","abc123",false,"rate-limited: slow down"]"#]).await;

        match out.as_slice() {
            [
                RelayEvent::Published {
                    relay,
                    id,
                    accepted,
                    message,
                },
            ] => {
                assert_eq!(relay, "wss://r.test");
                assert_eq!(id, "abc123");
                assert!(!accepted);
                assert_eq!(message, "rate-limited: slow down");
            }
            other => panic!("expected one Published, got {other:?}"),
        }
    }

    /// End-of-stored names its relay, which is what separates "this relay has
    /// no history for us" from "history is still coming".
    #[tokio::test]
    async fn end_of_stored_names_the_relay_that_finished() {
        let out = feed(&[r#"["EOSE","sub"]"#]).await;

        assert!(
            matches!(&out[..], [RelayEvent::EndOfStored { relay }] if relay == "wss://r.test"),
            "got {out:?}"
        );
    }

    /// Nothing a relay can put in the first slot should reach the app as a
    /// message — including frames this client does not implement.
    #[tokio::test]
    async fn unknown_and_malformed_frames_are_ignored() {
        let out = feed(&[
            "not json at all",
            "{}",
            r#"["NOTICE","the relay says something"]"#,
            r#"["EVENT"]"#,
        ])
        .await;

        assert!(out.is_empty(), "got {out:?}");
    }
}
