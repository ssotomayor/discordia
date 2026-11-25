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

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, mpsc, watch};

use crate::event::Event;

/// Relays used when the user has not chosen any.
///
/// Deliberately several and deliberately unaffiliated: a single default would
/// make one operator able to see every Discordia user's DM metadata, which is
/// the shape of the problem moving off the host was meant to solve.
pub const DEFAULT_RELAYS: &[&str] = &[
    "wss://relay.damus.io",
    "wss://nos.lol",
    "wss://relay.primal.net",
    "wss://nostr.mom",
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
    #[serde(rename = "#d", skip_serializing_if = "Option::is_none")]
    pub d: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authors: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// What the pool reports back to the app.
#[derive(Debug, Clone)]
pub enum RelayEvent {
    /// `relay` is whichever copy won the dedup race, not a fact about the message: enough
    /// to say whether that relay was still replaying, never which relays hold it.
    Event {
        relay: String,
        subscription: String,
        event: Box<Event>,
    },
    /// A relay finished replaying stored events for a subscription. `count` is how
    /// many it sent under that id since the request, duplicates included, so 0
    /// means it holds nothing that matched.
    EndOfStored {
        relay: String,
        subscription: String,
        count: usize,
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

#[derive(Clone)]
enum Command {
    Publish(Box<Event>),
    Subscribe(String, Standing),
    Unsubscribe {
        id: String,
    },
    FetchOlder {
        id: String,
        filters: Vec<Filter>,
        base: String,
    },
}

#[derive(Clone)]
struct Standing {
    filters: Vec<Filter>,
    /// Seconds before the newest event a relay delivered that a reconnect asks
    /// from; `None` replays the filters as given.
    resume: Option<i64>,
}

/// What one relay delivered, counted before dedup: dedup credits an event to
/// whichever relay was first, but `since` and `until` are each relay's own.
#[derive(Default)]
struct Delivered {
    /// Oldest and newest `created_at` per standing id, the newest never past our clock.
    span: HashMap<String, (i64, i64)>,
    /// One-shot id → the standing id whose span its events extend.
    once: HashMap<String, String>,
    /// Events per id since its `REQ` on this connection.
    counts: HashMap<String, usize>,
}

impl Delivered {
    fn note(&mut self, subscription: &str, created_at: i64, now: i64) {
        *self.counts.entry(subscription.to_string()).or_default() += 1;
        let key = self
            .once
            .get(subscription)
            .cloned()
            .unwrap_or_else(|| subscription.to_string());
        // A far-future stamp, which anyone can put on a gift wrap to us, must not
        // push the next `since` past what is still unread.
        let newest = created_at.min(now);
        let span = self.span.entry(key).or_insert((created_at, newest));
        span.0 = span.0.min(created_at);
        span.1 = span.1.max(newest);
    }

    fn request(&mut self, id: &str, standing: &Standing) -> String {
        self.counts.remove(id);
        let since = standing
            .resume
            .zip(self.span.get(id))
            .map(|(slack, (_, newest))| newest - slack);
        let filters: Vec<Filter> = standing
            .filters
            .iter()
            .map(|f| Filter {
                since: f.since.max(since),
                ..f.clone()
            })
            .collect();
        req_frame(id, &filters)
    }

    /// `None` when this relay delivered nothing under `base`: it has nothing older.
    fn older(&mut self, id: &str, filters: &[Filter], base: &str) -> Option<String> {
        let (oldest, _) = *self.span.get(base)?;
        self.counts.remove(id);
        self.once.insert(id.to_string(), base.to_string());
        let filters: Vec<Filter> = filters
            .iter()
            .map(|f| Filter {
                until: Some(f.until.map_or(oldest, |until| until.min(oldest))),
                ..f.clone()
            })
            .collect();
        Some(req_frame(id, &filters))
    }

    fn reconnected(&mut self) {
        self.once.clear();
        self.counts.clear();
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Standing subscriptions, by the id the relay knows them under.
///
/// A `REQ` replaces whatever the relay held under that id, which is what makes
/// this a map rather than one slot: the DM subscription and the metadata one
/// want different lifetimes — wraps are asked for once, names are re-asked
/// every time a new peer appears — and sharing an id meant the second `REQ`
/// silently cancelled the first. Ordered so a reconnect re-issues them the same
/// way every time.
type Subscriptions = BTreeMap<String, Standing>;

/// A set of relay connections, driven as one.
#[derive(Clone)]
pub struct RelayPool {
    cmd: mpsc::UnboundedSender<Command>,
    _tasks: Arc<PoolTasks>,
    _refresh: watch::Sender<u64>,
}

struct PoolTasks(Vec<tokio::task::JoinHandle<()>>);

impl Drop for PoolTasks {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

impl RelayPool {
    /// Connect to `urls` and start delivering events on the returned channel.
    ///
    /// Each relay gets its own task and its own reconnect clock; a relay that
    /// never comes up costs nothing but its own retries.
    pub fn connect(urls: Vec<String>) -> (RelayPool, mpsc::UnboundedReceiver<RelayEvent>) {
        Self::connect_with_wakeups(urls, None)
    }

    /// As `connect`, and every change on `wakeups` redials each relay at once:
    /// an Android resume or network change leaves sockets that look open but are dead.
    pub fn connect_with_wakeups(
        urls: Vec<String>,
        wakeups: Option<watch::Receiver<u64>>,
    ) -> (RelayPool, mpsc::UnboundedReceiver<RelayEvent>) {
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<Command>();
        let (out_tx, out_rx) = mpsc::unbounded_channel::<RelayEvent>();

        // Dedup across relays; bounded by eviction rather than growing for the
        // session.
        let seen: Arc<Mutex<SeenSet>> = Arc::new(Mutex::new(SeenSet::default()));
        // The standing subscriptions, kept so a relay that reconnects — or one
        // that was down when they were issued — can pick them up.
        let subs: Arc<Mutex<Subscriptions>> = Arc::new(Mutex::new(Subscriptions::new()));

        let mut senders = Vec::new();
        let mut tasks = Vec::new();
        let (refresh, _) = watch::channel(0);
        for url in urls {
            let (relay_tx, relay_rx) = mpsc::unbounded_channel::<Command>();
            senders.push(relay_tx);
            tasks.push(tokio::spawn(run_relay(
                url,
                relay_rx,
                out_tx.clone(),
                Arc::clone(&seen),
                Arc::clone(&subs),
                refresh.subscribe(),
            )));
        }

        if let Some(wakeups) = wakeups {
            tasks.push(tokio::spawn(refresh_after_wakeups(
                wakeups,
                refresh.clone(),
            )));
        }

        tasks.push(tokio::spawn(async move {
            while let Some(cmd) = cmd_rx.recv().await {
                match &cmd {
                    Command::Subscribe(id, standing) => {
                        subs.lock().await.insert(id.clone(), standing.clone());
                    }
                    Command::Unsubscribe { id } => {
                        subs.lock().await.remove(id);
                    }
                    Command::Publish(_) | Command::FetchOlder { .. } => {}
                }
                for s in &senders {
                    let _ = s.send(cmd.clone());
                }
            }
        }));

        (
            RelayPool {
                cmd: cmd_tx,
                _tasks: Arc::new(PoolTasks(tasks)),
                _refresh: refresh,
            },
            out_rx,
        )
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
        let standing = Standing {
            filters,
            resume: None,
        };
        let _ = self.cmd.send(Command::Subscribe(id.to_string(), standing));
    }

    /// As `subscribe`, but a relay that reconnects is asked only for events from
    /// `slack` seconds before the newest it already delivered under `id`, not
    /// for its whole stored window again.
    pub fn subscribe_resuming(&self, id: &str, filters: Vec<Filter>, slack: i64) {
        let standing = Standing {
            filters,
            resume: Some(slack),
        };
        let _ = self.cmd.send(Command::Subscribe(id.to_string(), standing));
    }

    /// Ask each relay once, under `id`, for what is older than the oldest event
    /// it delivered under the standing `base`; closed at its `EndOfStored`. A
    /// relay that delivered nothing under `base` answers at once with a count of 0.
    pub fn fetch_older(&self, id: &str, filters: Vec<Filter>, base: &str) {
        let _ = self.cmd.send(Command::FetchOlder {
            id: id.to_string(),
            filters,
            base: base.to_string(),
        });
    }

    /// Close the subscription `id` on every relay and stop re-issuing it on reconnect.
    pub fn unsubscribe(&self, id: &str) {
        let _ = self.cmd.send(Command::Unsubscribe { id: id.to_string() });
    }
}

async fn refresh_after_wakeups(mut wakeups: watch::Receiver<u64>, refresh: watch::Sender<u64>) {
    while wakeups.changed().await.is_ok() {
        tokio::time::sleep(Duration::from_millis(250)).await;
        drop(wakeups.borrow_and_update());
        refresh.send_modify(|revision| *revision = revision.wrapping_add(1));
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
    mut refresh: watch::Receiver<u64>,
) {
    let mut backoff = RECONNECT_MIN;
    let mut delivered = Delivered::default();
    // Only the newest subscription per id matters; publishes are dropped rather
    // than queued to avoid stale delivery.
    loop {
        let attempt = tokio::select! {
            request = refresh.changed() => {
                if request.is_err() { return; }
                backoff = RECONNECT_MIN;
                continue;
            }
            attempt = tokio::time::timeout(Duration::from_secs(10), tokio_tungstenite::connect_async(&url)) => attempt,
        };
        let mut renewed = false;
        match attempt {
            Ok(Ok((stream, _))) => {
                backoff = RECONNECT_MIN;
                let _ = out.send(RelayEvent::Connected(url.clone()));
                let why = tokio::select! {
                    request = refresh.changed() => {
                        if request.is_err() { return; }
                        renewed = true;
                        "connection refreshed after resume or network change".into()
                    }
                    why = serve(&url, stream, &mut cmds, &out, &seen, &subs, &mut delivered) => why,
                };
                let _ = out.send(RelayEvent::Disconnected {
                    relay: url.clone(),
                    why,
                });
            }
            Ok(Err(e)) => {
                let _ = out.send(RelayEvent::Disconnected {
                    relay: url.clone(),
                    why: e.to_string(),
                });
            }
            Err(_) => {
                let _ = out.send(RelayEvent::Disconnected {
                    relay: url.clone(),
                    why: "connection timed out after 10 seconds".into(),
                });
            }
        }
        if renewed {
            backoff = RECONNECT_MIN;
            continue;
        }
        tokio::select! {
            request = refresh.changed() => {
                if request.is_err() { return; }
                backoff = RECONNECT_MIN;
            }
            _ = tokio::time::sleep(backoff) => { backoff = (backoff * 2).min(RECONNECT_MAX); }
        }
    }
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

#[derive(Clone, Copy)]
struct RelayHealth {
    ping_every: Duration,
    pong_wait: Duration,
}

impl Default for RelayHealth {
    fn default() -> Self {
        Self {
            ping_every: Duration::from_secs(30),
            pong_wait: Duration::from_secs(15),
        }
    }
}

async fn send_frame(
    tx: &mut futures_util::stream::SplitSink<Ws, tokio_tungstenite::tungstenite::Message>,
    message: tokio_tungstenite::tungstenite::Message,
) -> Result<(), ()> {
    match tokio::time::timeout(Duration::from_secs(5), tx.send(message)).await {
        Ok(Ok(())) => Ok(()),
        _ => Err(()),
    }
}

/// Pump one live connection until it fails. Returns why it ended.
async fn serve(
    url: &str,
    stream: Ws,
    cmds: &mut mpsc::UnboundedReceiver<Command>,
    out: &mpsc::UnboundedSender<RelayEvent>,
    seen: &Arc<Mutex<SeenSet>>,
    subs: &Arc<Mutex<Subscriptions>>,
    delivered: &mut Delivered,
) -> String {
    serve_with_health(
        url,
        stream,
        cmds,
        out,
        seen,
        subs,
        delivered,
        RelayHealth::default(),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn serve_with_health(
    url: &str,
    stream: Ws,
    cmds: &mut mpsc::UnboundedReceiver<Command>,
    out: &mpsc::UnboundedSender<RelayEvent>,
    seen: &Arc<Mutex<SeenSet>>,
    subs: &Arc<Mutex<Subscriptions>>,
    delivered: &mut Delivered,
    health: RelayHealth,
) -> String {
    use tokio_tungstenite::tungstenite::Message;
    let (mut tx, mut rx) = stream.split();

    // Re-issue every standing subscription; a fresh connection without a REQ
    // receives nothing.
    delivered.reconnected();
    let subscriptions = subs.lock().await.clone();
    for (id, standing) in &subscriptions {
        if send_frame(&mut tx, Message::Text(delivered.request(id, standing)))
            .await
            .is_err()
        {
            return "could not send the subscription".into();
        }
    }

    let mut heartbeat = tokio::time::interval(health.ping_every);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    heartbeat.tick().await;
    let mut pong_due = None;
    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                if pong_due.is_none() {
                    if send_frame(&mut tx, Message::Ping(vec![])).await.is_err() { return "heartbeat send failed".into(); }
                    pong_due = Some(tokio::time::Instant::now() + health.pong_wait);
                }
            }
            _ = async {
                if let Some(due) = pong_due { tokio::time::sleep_until(due).await; }
                else { std::future::pending::<()>().await; }
            } => return "relay heartbeat timed out".into(),
            cmd = cmds.recv() => match cmd {
                Some(Command::Publish(e)) => {
                    let msg = serde_json::json!(["EVENT", &*e]).to_string();
                    if send_frame(&mut tx, Message::Text(msg)).await.is_err() {
                        return "send failed".into();
                    }
                }
                Some(Command::Subscribe(id, standing)) => {
                    // Asked for afresh: the filters as given, with no `since` from before.
                    delivered.counts.remove(&id);
                    if send_frame(&mut tx, Message::Text(req_frame(&id, &standing.filters))).await.is_err() {
                        return "send failed".into();
                    }
                }
                Some(Command::FetchOlder { id, filters, base }) => match delivered.older(&id, &filters, &base) {
                    Some(frame) => {
                        if send_frame(&mut tx, Message::Text(frame)).await.is_err() {
                            return "send failed".into();
                        }
                    }
                    None => {
                        let _ = out.send(RelayEvent::EndOfStored { relay: url.to_string(), subscription: id, count: 0 });
                    }
                },
                Some(Command::Unsubscribe { id }) => {
                    let msg = serde_json::json!(["CLOSE", id]).to_string();
                    if send_frame(&mut tx, Message::Text(msg)).await.is_err() {
                        return "send failed".into();
                    }
                }
                None => return "pool shut down".into(),
            },
            msg = rx.next() => {
                pong_due = None;
                match msg {
                Some(Ok(Message::Text(text))) => {
                    if let Some(id) = handle_relay_message(url, &text, out, seen, delivered).await {
                        let msg = serde_json::json!(["CLOSE", id]).to_string();
                        if send_frame(&mut tx, Message::Text(msg)).await.is_err() {
                            return "send failed".into();
                        }
                    }
                }
                Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Binary(_) | Message::Frame(_))) => {}
                Some(Ok(Message::Close(_))) => return "relay closed the connection".into(),
                Some(Err(e)) => return e.to_string(),
                None => return "stream ended".into(),
                }
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
/// Returns a one-shot subscription that has ended and must now be closed.
async fn handle_relay_message(
    url: &str,
    text: &str,
    out: &mpsc::UnboundedSender<RelayEvent>,
    seen: &Arc<Mutex<SeenSet>>,
    delivered: &mut Delivered,
) -> Option<String> {
    let v = serde_json::from_str::<serde_json::Value>(text).ok()?;
    let kind = v.get(0).and_then(|k| k.as_str())?;
    match kind {
        "EVENT" => {
            let subscription = v.get(1).and_then(|s| s.as_str()).unwrap_or_default();
            let raw = v.get(2)?;
            let event = serde_json::from_value::<Event>(raw.clone()).ok()?;
            // Verify once here; unverified events are forgery attempts, and a
            // relay serving them is worth flagging.
            if !event.verify() {
                eprintln!(
                    "[nostr] {url}: dropped an event that does not verify (id {})",
                    event.id
                );
                return None;
            }
            delivered.note(subscription, event.created_at, unix_now());
            if !seen.lock().await.insert_new(&event.id) {
                return None;
            }
            let _ = out.send(RelayEvent::Event {
                relay: url.to_string(),
                subscription: subscription.to_string(),
                event: Box::new(event),
            });
        }
        "EOSE" => {
            let subscription = v.get(1).and_then(|s| s.as_str())?;
            let count = delivered.counts.remove(subscription).unwrap_or(0);
            let _ = out.send(RelayEvent::EndOfStored {
                relay: url.to_string(),
                subscription: subscription.to_string(),
                count,
            });
            if delivered.once.remove(subscription).is_some() {
                return Some(subscription.to_string());
            }
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
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn refresh_replaces_a_live_socket_and_reissues_its_subscriptions() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let (received, mut subscriptions) = mpsc::unbounded_channel();
        let server = tokio::spawn(async move {
            let mut sockets = Vec::new();
            for _ in 0..2 {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                while let Some(Ok(message)) = socket.next().await {
                    if let Ok(text) = message.to_text() {
                        let frame: serde_json::Value = serde_json::from_str(text).unwrap();
                        if frame[0] == "REQ" {
                            received.send(frame).unwrap();
                            break;
                        }
                    }
                }
                sockets.push(socket);
            }
        });
        let (pool, _events) = RelayPool::connect(vec![url]);
        pool.subscribe(
            "dm",
            vec![Filter {
                kinds: Some(vec![1059]),
                ..Default::default()
            }],
        );
        let first = tokio::time::timeout(Duration::from_secs(2), subscriptions.recv())
            .await
            .unwrap()
            .unwrap();
        pool._refresh.send_modify(|revision| *revision += 1);
        let second = tokio::time::timeout(Duration::from_secs(2), subscriptions.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(second[1], "dm");
        drop(pool);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn an_unsubscribed_id_is_closed_and_not_reissued_on_reconnect() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let (frames, mut received) = mpsc::unbounded_channel();
        let server = tokio::spawn(async move {
            let mut sockets = Vec::new();
            for until in ["CLOSE", "REQ"] {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                while let Some(Ok(message)) = socket.next().await {
                    if let Ok(text) = message.to_text() {
                        let frame: serde_json::Value = serde_json::from_str(text).unwrap();
                        let done = frame[0] == until;
                        frames.send(frame).unwrap();
                        if done {
                            break;
                        }
                    }
                }
                sockets.push(socket);
            }
        });
        let (pool, _events) = RelayPool::connect(vec![url]);
        for id in ["a", "b"] {
            pool.subscribe(id, vec![Filter::default()]);
        }
        pool.unsubscribe("a");
        let mut first = Vec::new();
        while let Ok(Some(frame)) =
            tokio::time::timeout(Duration::from_secs(2), received.recv()).await
        {
            let close = frame[0] == "CLOSE";
            first.push(frame);
            if close {
                break;
            }
        }
        assert_eq!(first.last().unwrap(), &serde_json::json!(["CLOSE", "a"]));
        pool._refresh.send_modify(|revision| *revision += 1);
        let reissued = tokio::time::timeout(Duration::from_secs(2), received.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            reissued[1], "b",
            "only the standing subscription comes back"
        );
        drop(pool);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn refresh_interrupts_connection_backoff() {
        let reserved = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = reserved.local_addr().unwrap();
        drop(reserved);
        let (pool, mut events) = RelayPool::connect(vec![format!("ws://{address}")]);
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(12), events.recv())
                .await
                .unwrap(),
            Some(RelayEvent::Disconnected { .. })
        ));
        let listener = tokio::net::TcpListener::bind(address).await.unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            tokio_tungstenite::accept_async(stream).await.unwrap()
        });
        pool._refresh.send_modify(|revision| *revision += 1);
        let event = tokio::time::timeout(Duration::from_millis(1500), events.recv())
            .await
            .unwrap();
        assert!(matches!(event, Some(RelayEvent::Connected(_))));
        drop(pool);
        drop(server.await.unwrap());
    }

    #[tokio::test]
    async fn resume_bursts_are_coalesced_and_the_watcher_stops_with_its_owner() {
        let (wake, receiver) = watch::channel(0);
        let (refresh, mut changes) = watch::channel(0);
        let task = tokio::spawn(refresh_after_wakeups(receiver, refresh));
        wake.send_modify(|revision| *revision += 1);
        tokio::task::yield_now().await;
        for _ in 0..10 {
            wake.send_modify(|revision| *revision += 1);
        }
        tokio::time::timeout(Duration::from_secs(1), changes.changed())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(*changes.borrow_and_update(), 1);
        assert!(
            tokio::time::timeout(Duration::from_millis(80), changes.changed())
                .await
                .is_err()
        );
        drop(wake);
        task.await.unwrap();
    }

    #[tokio::test]
    async fn a_socket_that_stops_answering_heartbeats_is_disconnected() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            tokio_tungstenite::accept_async(stream).await.unwrap()
        });
        let (client, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        let quiet_server = server.await.unwrap();
        let (_commands, mut commands) = mpsc::unbounded_channel();
        let (out, _events) = mpsc::unbounded_channel();
        let reason = tokio::time::timeout(
            Duration::from_secs(2),
            serve_with_health(
                &url,
                client,
                &mut commands,
                &out,
                &Arc::new(Mutex::new(SeenSet::default())),
                &Arc::new(Mutex::new(Subscriptions::new())),
                &mut Delivered::default(),
                RelayHealth {
                    ping_every: Duration::from_millis(20),
                    pong_wait: Duration::from_millis(30),
                },
            ),
        )
        .await
        .unwrap();
        assert_eq!(reason, "relay heartbeat timed out");
        drop(quiet_server);
    }

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
        let mut delivered = Delivered::default();
        for f in frames {
            handle_relay_message("wss://r.test", f, &tx, &seen, &mut delivered).await;
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
        crate::event::sign_with(&secret, 1_700_000_000, 1, vec![], "hello".into())
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
        assert!(matches!(out[0], RelayEvent::Event { .. }));
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
        let out = feed(&[r#"["EOSE"]"#, r#"["EOSE",null]"#, r#"["EOSE","sub"]"#]).await;

        assert!(
            matches!(&out[..], [RelayEvent::EndOfStored { relay, subscription, count: 0 }] if relay == "wss://r.test" && subscription == "sub"),
            "got {out:?}"
        );
    }

    fn standing(resume: Option<i64>) -> Standing {
        Standing {
            filters: vec![Filter {
                kinds: Some(vec![1059]),
                limit: Some(500),
                ..Default::default()
            }],
            resume,
        }
    }

    fn frame_filter(frame: &str) -> serde_json::Value {
        serde_json::from_str::<serde_json::Value>(frame).unwrap()[2].clone()
    }

    /// A reconnect asks only for what is newer than this relay's newest, less the
    /// slack; a first connection, or a subscription that does not resume, asks for all.
    #[test]
    fn a_resuming_subscription_asks_from_the_newest_this_relay_delivered() {
        let mut delivered = Delivered::default();
        assert_eq!(
            frame_filter(&delivered.request("dm", &standing(Some(100))))["since"],
            serde_json::Value::Null
        );
        delivered.note("dm", 1_000, 5_000);
        delivered.note("dm", 3_000, 5_000);
        delivered.note("dm", 2_000, 5_000);
        assert_eq!(
            frame_filter(&delivered.request("dm", &standing(Some(100))))["since"],
            2_900
        );
        assert_eq!(
            frame_filter(&delivered.request("dm", &standing(None)))["since"],
            serde_json::Value::Null
        );
    }

    /// Anyone can stamp a gift wrap to us in the future; that must not move
    /// `since` past messages not yet read.
    #[test]
    fn a_future_stamp_counts_as_now() {
        let mut delivered = Delivered::default();
        delivered.note("dm", 9_999_999, 5_000);
        assert_eq!(
            frame_filter(&delivered.request("dm", &standing(Some(100))))["since"],
            4_900
        );
    }

    /// Older pages go back from this relay's oldest, and each page moves that
    /// mark, so the next one starts where it ended.
    #[test]
    fn an_older_page_goes_back_from_this_relays_oldest() {
        let mut delivered = Delivered::default();
        let filters = standing(None).filters;
        assert!(
            delivered.older("old", &filters, "dm").is_none(),
            "nothing under the base: nothing older"
        );
        delivered.note("dm", 3_000, 5_000);
        delivered.note("dm", 2_000, 5_000);
        assert_eq!(
            frame_filter(&delivered.older("old", &filters, "dm").unwrap())["until"],
            2_000
        );
        delivered.note("old", 1_500, 5_000);
        assert_eq!(
            frame_filter(&delivered.older("old", &filters, "dm").unwrap())["until"],
            1_500
        );
    }

    /// The relay is told to close a one-shot at its end, and the count says how
    /// many it sent, duplicates included: another relay having sent them first
    /// does not make this one empty.
    #[tokio::test]
    async fn a_one_shot_ends_with_its_count_and_is_closed() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let seen = Arc::new(Mutex::new(SeenSet::default()));
        let mut delivered = Delivered::default();
        delivered.note("dm", 1_800_000_000, 1_800_000_000);
        delivered.older("old", &[Filter::default()], "dm").unwrap();
        let event = serde_json::json!(["EVENT", "old", signed_event()]).to_string();
        for frame in [&event, &event] {
            assert_eq!(
                handle_relay_message("wss://r.test", frame, &tx, &seen, &mut delivered).await,
                None
            );
        }
        let closed = handle_relay_message(
            "wss://r.test",
            r#"["EOSE","old"]"#,
            &tx,
            &seen,
            &mut delivered,
        )
        .await;
        assert_eq!(closed.as_deref(), Some("old"));
        let mut out = Vec::new();
        while let Ok(event) = rx.try_recv() {
            out.push(event);
        }
        assert!(
            matches!(&out[..], [RelayEvent::Event { subscription, .. }, RelayEvent::EndOfStored { count: 2, .. }] if subscription == "old"),
            "got {out:?}"
        );
        assert_eq!(
            delivered.span["dm"].0, 1_700_000_000,
            "the page extends the base's span"
        );
        let standing_end = handle_relay_message(
            "wss://r.test",
            r#"["EOSE","dm"]"#,
            &tx,
            &seen,
            &mut delivered,
        )
        .await;
        assert_eq!(standing_end, None, "a standing subscription stays open");
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
