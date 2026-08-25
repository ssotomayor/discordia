use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, mpsc};

use super::event::Event;

pub const DEFAULT_RELAYS: &[&str] = &[
    "wss://relay.damus.io",
    "wss://nos.lol",
    "wss://relay.primal.net",
    "wss://relay.nostr.band",
];

const RECONNECT_MIN: Duration = Duration::from_secs(2);
const RECONNECT_MAX: Duration = Duration::from_secs(60);

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

#[derive(Debug, Clone)]
pub enum RelayEvent {
    Event(Box<Event>),
    EndOfStored {
        relay: String,
    },
    Published {
        relay: String,
        id: String,
        accepted: bool,
        message: String,
    },
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

#[derive(Clone)]
pub struct RelayPool {
    cmd: mpsc::UnboundedSender<Command>,
}

impl RelayPool {
    pub fn connect(urls: Vec<String>) -> (RelayPool, mpsc::UnboundedReceiver<RelayEvent>) {
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<Command>();
        let (out_tx, out_rx) = mpsc::unbounded_channel::<RelayEvent>();

        let seen: Arc<Mutex<SeenSet>> = Arc::new(Mutex::new(SeenSet::default()));
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

    pub fn publish(&self, event: Event) {
        let _ = self.cmd.send(Command::Publish(Box::new(event)));
    }

    pub fn subscribe(&self, filters: Vec<Filter>) {
        let _ = self.cmd.send(Command::Subscribe(filters));
    }
}

#[derive(Default)]
struct SeenSet {
    ids: HashSet<String>,
    order: Vec<String>,
}

impl SeenSet {
    const CAP: usize = 8192;

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

async fn run_relay(
    url: String,
    mut cmds: mpsc::UnboundedReceiver<Command>,
    out: mpsc::UnboundedSender<RelayEvent>,
    seen: Arc<Mutex<SeenSet>>,
    filter: Arc<Mutex<Option<Vec<Filter>>>>,
) {
    let mut backoff = RECONNECT_MIN;
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

    #[test]
    fn a_filter_serializes_the_way_relays_read_it() {
        let f = Filter {
            kinds: Some(vec![1059]),
            p: Some(vec!["abc".into()]),
            limit: Some(100),
            ..Default::default()
        };
        let json = serde_json::to_string(&f).expect("serialize");
        assert_eq!(json, r##"{"kinds":[1059],"#p":["abc"],"limit":100}"##);
        assert!(!json.contains("null"), "absent fields must be omitted");
    }

    #[test]
    fn the_same_event_is_delivered_once() {
        let mut seen = SeenSet::default();
        assert!(seen.insert_new("a"));
        assert!(!seen.insert_new("a"));
        assert!(seen.insert_new("b"));
    }

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

    #[tokio::test]
    async fn a_verifying_event_arrives_exactly_once() {
        let frame = serde_json::json!(["EVENT", "sub", signed_event()]).to_string();
        let out = feed(&[&frame, &frame]).await;

        assert_eq!(out.len(), 1, "the duplicate must be dropped");
        assert!(matches!(out[0], RelayEvent::Event(_)));
    }

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

    #[tokio::test]
    async fn end_of_stored_names_the_relay_that_finished() {
        let out = feed(&[r#"["EOSE","sub"]"#]).await;

        assert!(
            matches!(&out[..], [RelayEvent::EndOfStored { relay }] if relay == "wss://r.test"),
            "got {out:?}"
        );
    }

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
