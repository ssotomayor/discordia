use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use discordia_nostr::event::{Event, sign_with, xonly_hex};
use discordia_nostr::relay::Filter;
use discordia_protocol::push::{DmWrap, PushData, RegisterDevice};
use discordia_push_relay::limits::Limits;
use discordia_push_relay::sender::RecordingSender;
use discordia_push_relay::store::{Caps, Store};
use discordia_push_relay::watcher::{Stats, Watcher, WatcherConfig};
use discordia_push_relay::{Config, Relay, now_unix};
use futures_util::{SinkExt, StreamExt};
use secp256k1::SecretKey;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_tungstenite::tungstenite::Message;

const DAY: i64 = 24 * 60 * 60;
const DEBOUNCE: Duration = Duration::from_millis(150);

enum Ctl {
    Live(Event),
    Drop(oneshot::Sender<()>),
    Release,
}

#[derive(Default)]
struct Shared {
    stored: Vec<Event>,
    conn: Option<mpsc::UnboundedSender<Ctl>>,
    held: bool,
    frames: Vec<Value>,
}

/// A Nostr relay in-process: replays what it stores then `EOSE`, forwards live
/// events to matching subscriptions, and can hold its answers or drop the socket.
struct FakeRelay {
    url: String,
    shared: Arc<Mutex<Shared>>,
    accepts: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for FakeRelay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn matches(filters: &[Filter], e: &Event) -> bool {
    filters.iter().any(|f| {
        f.kinds.as_ref().is_none_or(|k| k.contains(&e.kind))
            && f.since.is_none_or(|s| e.created_at >= s)
            && f.p.as_ref().is_none_or(|ps| {
                e.tags.iter().any(|t| {
                    t.first().map(String::as_str) == Some("p")
                        && t.get(1).is_some_and(|p| ps.contains(p))
                })
            })
    })
}

type Ws = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

async fn answer(
    tx: &mut futures_util::stream::SplitSink<Ws, Message>,
    shared: &Mutex<Shared>,
    id: &str,
    filters: &[Filter],
) {
    let stored = shared.lock().unwrap().stored.clone();
    for e in stored.iter().filter(|e| matches(filters, e)) {
        let _ = tx
            .send(Message::Text(json!(["EVENT", id, e]).to_string()))
            .await;
    }
    let _ = tx
        .send(Message::Text(json!(["EOSE", id]).to_string()))
        .await;
}

async fn serve(ws: Ws, shared: Arc<Mutex<Shared>>) {
    let (ctl_tx, mut ctl_rx) = mpsc::unbounded_channel();
    shared.lock().unwrap().conn = Some(ctl_tx);
    let (mut tx, mut rx) = ws.split();
    let mut subs: HashMap<String, Vec<Filter>> = HashMap::new();
    let mut pending: Vec<(String, Vec<Filter>)> = Vec::new();
    loop {
        tokio::select! {
            msg = rx.next() => {
                let Some(Ok(msg)) = msg else { break };
                let Message::Text(text) = msg else { continue };
                let Ok(frame) = serde_json::from_str::<Value>(&text) else { continue };
                shared.lock().unwrap().frames.push(frame.clone());
                let id = frame[1].as_str().unwrap_or_default().to_string();
                match frame[0].as_str() {
                    Some("REQ") => {
                        let filters: Vec<Filter> = frame
                            .as_array()
                            .map(|a| a[2..].iter().filter_map(|f| serde_json::from_value(f.clone()).ok()).collect())
                            .unwrap_or_default();
                        subs.remove(&id);
                        let held = shared.lock().unwrap().held;
                        if held {
                            pending.push((id, filters));
                        } else {
                            answer(&mut tx, &shared, &id, &filters).await;
                            subs.insert(id, filters);
                        }
                    }
                    Some("CLOSE") => {
                        subs.remove(&id);
                        pending.retain(|(p, _)| *p != id);
                    }
                    _ => {}
                }
            }
            ctl = ctl_rx.recv() => match ctl {
                Some(Ctl::Live(e)) => {
                    for (id, filters) in &subs {
                        if matches(filters, &e) {
                            let _ = tx.send(Message::Text(json!(["EVENT", id, e]).to_string())).await;
                        }
                    }
                }
                Some(Ctl::Release) => {
                    for (id, filters) in std::mem::take(&mut pending) {
                        answer(&mut tx, &shared, &id, &filters).await;
                        subs.insert(id, filters);
                    }
                }
                Some(Ctl::Drop(ack)) => {
                    shared.lock().unwrap().conn = None;
                    drop((tx, rx));
                    let _ = ack.send(());
                    return;
                }
                None => break,
            },
        }
    }
    shared.lock().unwrap().conn = None;
}

impl FakeRelay {
    async fn start(held: bool) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let shared = Arc::new(Mutex::new(Shared {
            held,
            ..Shared::default()
        }));
        let accepts = Arc::new(AtomicUsize::new(0));
        let task = tokio::spawn({
            let (shared, accepts) = (shared.clone(), accepts.clone());
            async move {
                loop {
                    let Ok((stream, _)) = listener.accept().await else {
                        return;
                    };
                    accepts.fetch_add(1, Ordering::SeqCst);
                    if let Ok(ws) = tokio_tungstenite::accept_async(stream).await {
                        serve(ws, shared.clone()).await;
                    }
                }
            }
        });
        FakeRelay {
            url,
            shared,
            accepts,
            task,
        }
    }

    /// Kept for replay only, as if it arrived while nobody was subscribed.
    fn store(&self, e: &Event) {
        self.shared.lock().unwrap().stored.push(e.clone());
    }

    fn publish(&self, e: &Event) {
        let mut shared = self.shared.lock().unwrap();
        shared.stored.push(e.clone());
        if let Some(conn) = &shared.conn {
            let _ = conn.send(Ctl::Live(e.clone()));
        }
    }

    async fn drop_connection(&self) {
        let (ack, done) = oneshot::channel();
        let conn = self.shared.lock().unwrap().conn.clone();
        conn.expect("connected").send(Ctl::Drop(ack)).ok();
        done.await.unwrap();
    }

    fn release(&self) {
        let mut shared = self.shared.lock().unwrap();
        shared.held = false;
        if let Some(conn) = &shared.conn {
            let _ = conn.send(Ctl::Release);
        }
    }

    /// Subscription ids in the order first asked for. The pool may send one `REQ`
    /// twice when it was asked before the socket opened; the filter is the same.
    fn sub_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = Vec::new();
        for f in self.frames("REQ") {
            let id = f[1].as_str().unwrap_or_default().to_string();
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        ids
    }

    fn frames(&self, kind: &str) -> Vec<Value> {
        self.shared
            .lock()
            .unwrap()
            .frames
            .iter()
            .filter(|f| f[0] == kind)
            .cloned()
            .collect()
    }

    async fn wait_for_req(&self) {
        for _ in 0..500 {
            if !self.frames("REQ").is_empty() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("{} was never subscribed", self.url);
    }
}

fn relay() -> (Arc<Relay>, Arc<RecordingSender>) {
    let rec = Arc::new(RecordingSender::default());
    let relay = Relay::new(
        Store::in_memory(Caps::default()).unwrap(),
        rec.clone(),
        Config {
            validate_tokens: false,
            ..Config::default()
        },
        Limits::default(),
    );
    (Arc::new(relay), rec)
}

fn start(relay: &Arc<Relay>, urls: &[&FakeRelay]) -> (Watcher, watch::Receiver<Stats>) {
    let mut config = WatcherConfig::new(urls.iter().map(|r| r.url.clone()).collect());
    config.debounce = DEBOUNCE;
    let watcher = Watcher::spawn(relay.clone(), config);
    let stats = watcher.stats();
    (watcher, stats)
}

fn person(seed: u8) -> String {
    xonly_hex(&SecretKey::from_slice(&[seed; 32]).unwrap())
}

fn register(relay: &Relay, pubkey: &str, token: &str, relays: &[&str]) {
    relay
        .store
        .upsert_device(
            pubkey,
            &RegisterDevice {
                token: token.into(),
                package: "com.discordia.app".into(),
                dm: true,
                relays: relays.iter().map(|r| r.to_string()).collect(),
                replaces: None,
            },
            now_unix(),
        )
        .unwrap();
}

/// A gift wrap to `to`, backdated by `age` seconds, from a fresh key.
fn wrap(to: &[&str], age: i64, extra: &[[&str; 2]]) -> Event {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let ephemeral = SecretKey::new(&mut secp256k1::rand::thread_rng());
    let mut tags: Vec<Vec<String>> = to.iter().map(|p| vec!["p".into(), p.to_string()]).collect();
    tags.extend(
        extra
            .iter()
            .map(|[k, v]| vec![k.to_string(), v.to_string()]),
    );
    sign_with(
        &ephemeral,
        now_unix() as i64 - age,
        1059,
        tags,
        format!("ciphertext {n}"),
    )
}

async fn until(
    stats: &mut watch::Receiver<Stats>,
    secs: u64,
    what: &str,
    f: impl FnMut(&Stats) -> bool,
) -> Stats {
    let reached = tokio::time::timeout(Duration::from_secs(secs), stats.wait_for(f))
        .await
        .ok()
        .and_then(|r| r.ok().map(|s| s.clone()));
    reached.unwrap_or_else(|| panic!("timed out waiting for {what}: {:?}", *stats.borrow()))
}

/// Every push has finished and at least `n` were made.
async fn settled(stats: &mut watch::Receiver<Stats>, n: u64) -> Stats {
    until(stats, 5, &format!("{n} pushes"), |s| {
        s.pushes_started >= n && s.pushes_done == s.pushes_started
    })
    .await
}

/// (recipient, event id) per push, in order.
fn pushed(rec: &RecordingSender) -> Vec<(String, String)> {
    rec.delivered()
        .into_iter()
        .filter_map(|r| match r.data {
            PushData::Dm {
                p,
                wrap: DmWrap::Inline { e },
            } => {
                let e: Event = serde_json::from_str(&e).unwrap();
                assert!(e.verify(), "the phone gets a wrap it can verify");
                Some((p, e.id))
            }
            PushData::Dm {
                p,
                wrap: DmWrap::ById { id, .. },
            } => Some((p, id)),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn backlog_is_not_pushed_and_a_wrap_after_baseline_is_pushed_once() {
    let a = FakeRelay::start(false).await;
    let (relay, rec) = relay();
    let k1 = person(1);
    register(&relay, &k1, "t1", &[]);
    a.store(&wrap(&[&k1], DAY + 3600, &[]));
    a.store(&wrap(&[&k1], 60, &[]));
    let (_w, mut st) = start(&relay, &[&a]);
    let s = until(&mut st, 5, "baseline", |s| s.baselined == 1).await;
    assert_eq!(s.wraps, 2, "the backlog arrived before EOSE");

    let req = &a.frames("REQ")[0];
    let since = req[2]["since"].as_i64().unwrap();
    let expected = now_unix() as i64 - 2 * DAY - 60;
    assert!((expected - 5..=expected).contains(&since), "since {since}");
    assert_eq!(req[2]["kinds"], json!([1059]));
    assert_eq!(req[2]["#p"], json!([k1]));

    let fresh = wrap(&[&k1], DAY + 7200, &[]);
    a.publish(&fresh);
    settled(&mut st, 1).await;
    assert_eq!(pushed(&rec), [(k1, fresh.id)]);
}

#[tokio::test]
async fn a_key_registered_while_running_skips_its_backlog_and_gets_what_follows() {
    let a = FakeRelay::start(false).await;
    let (relay, rec) = relay();
    let (k1, k2) = (person(1), person(2));
    register(&relay, &k1, "t1", &[]);
    a.store(&wrap(&[&k1], 600, &[]));
    let (_w, mut st) = start(&relay, &[&a]);
    until(&mut st, 5, "k1 baseline", |s| s.baselined == 1).await;

    a.store(&wrap(&[&k2], 3 * 3600, &[]));
    a.store(&wrap(&[&k2], 5, &[]));
    register(&relay, &k2, "t2", &[]);
    let s = until(&mut st, 5, "k2 baseline", |s| s.baselined == 2).await;
    assert_eq!(s.wraps, 3, "k2's backlog was replayed");
    assert!(rec.delivered().is_empty(), "{:?}", pushed(&rec));

    let fresh = wrap(&[&k2], 3600, &[]);
    a.publish(&fresh);
    settled(&mut st, 1).await;
    assert_eq!(pushed(&rec), [(k2, fresh.id)]);
}

#[tokio::test]
async fn a_wrap_stored_while_a_relay_was_away_is_pushed_once_when_it_returns() {
    let a = FakeRelay::start(false).await;
    let (relay, rec) = relay();
    let k1 = person(1);
    register(&relay, &k1, "t1", &[]);
    a.store(&wrap(&[&k1], 600, &[]));
    let (_w, mut st) = start(&relay, &[&a]);
    until(&mut st, 5, "baseline", |s| s.baselined == 1).await;

    a.drop_connection().await;
    let gap = wrap(&[&k1], DAY, &[]);
    a.store(&gap);
    until(&mut st, 15, "the gap wrap after the redial", |s| {
        s.pushes_started == 1 && s.pushes_done == 1
    })
    .await;
    assert_eq!(a.accepts.load(Ordering::SeqCst), 2);
    assert_eq!(pushed(&rec), [(k1, gap.id)]);
}

#[tokio::test]
async fn the_same_wrap_from_two_relays_is_pushed_once() {
    let a = FakeRelay::start(false).await;
    let b = FakeRelay::start(false).await;
    let (relay, rec) = relay();
    let k1 = person(1);
    register(&relay, &k1, "t1", &[]);
    let (_w, mut st) = start(&relay, &[&a, &b]);
    until(&mut st, 5, "both relays baselined", |s| s.baselined == 2).await;

    let fresh = wrap(&[&k1], 60, &[]);
    a.publish(&fresh);
    b.publish(&fresh);
    until(&mut st, 5, "both copies", |s| {
        s.wraps == 2 && s.pushes_started >= 1 && s.pushes_done == s.pushes_started
    })
    .await;
    assert_eq!(pushed(&rec), [(k1, fresh.id)]);
}

#[tokio::test]
async fn a_relay_that_answers_late_does_not_push_its_backlog() {
    let a = FakeRelay::start(false).await;
    let b = FakeRelay::start(true).await;
    let (relay, rec) = relay();
    let k1 = person(1);
    register(&relay, &k1, "t1", &[]);
    let common = wrap(&[&k1], 2 * 3600, &[]);
    a.store(&common);
    b.store(&common);
    b.store(&wrap(&[&k1], 3 * 3600, &[]));
    let (_w, mut st) = start(&relay, &[&a, &b]);
    until(&mut st, 5, "a baselined", |s| s.baselined == 1).await;
    b.wait_for_req().await;

    let live = wrap(&[&k1], 60, &[]);
    a.publish(&live);
    b.store(&live);
    settled(&mut st, 1).await;
    assert_eq!(pushed(&rec), [(k1.clone(), live.id.clone())]);

    b.release();
    let s = until(&mut st, 5, "b baselined", |s| s.baselined == 2).await;
    assert_eq!(s.wraps, 5, "b replayed all three");
    assert_eq!(pushed(&rec).len(), 1, "nothing b replayed was pushed");

    let later = wrap(&[&k1], 60, &[]);
    b.publish(&later);
    settled(&mut st, 2).await;
    assert_eq!(pushed(&rec), [(k1.clone(), live.id), (k1, later.id)]);
}

#[tokio::test]
async fn a_wrap_naming_several_registered_keys_wakes_each() {
    let a = FakeRelay::start(false).await;
    let (relay, rec) = relay();
    let (k1, k2, stranger) = (person(1), person(2), person(3));
    register(&relay, &k1, "t1", &[]);
    register(&relay, &k2, "t2", &[]);
    let (_w, mut st) = start(&relay, &[&a]);
    until(&mut st, 5, "baseline", |s| s.baselined == 2).await;

    let fresh = wrap(&[&k1, &stranger, &k2, &k1], 60, &[]);
    a.publish(&fresh);
    settled(&mut st, 2).await;
    let mut got = pushed(&rec);
    got.sort();
    let mut want = vec![(k1, fresh.id.clone()), (k2, fresh.id)];
    want.sort();
    assert_eq!(got, want);
}

#[tokio::test]
async fn a_call_signal_with_an_expiration_is_not_pushed() {
    let a = FakeRelay::start(false).await;
    let (relay, rec) = relay();
    let k1 = person(1);
    register(&relay, &k1, "t1", &[]);
    let (_w, mut st) = start(&relay, &[&a]);
    until(&mut st, 5, "baseline", |s| s.baselined == 1).await;

    let expires = (now_unix() + 60).to_string();
    a.publish(&wrap(&[&k1], 0, &[["expiration", &expires]]));
    let marker = wrap(&[&k1], 60, &[]);
    a.publish(&marker);
    let s = settled(&mut st, 1).await;
    assert_eq!(s.wraps, 2);
    assert_eq!(pushed(&rec), [(k1, marker.id)]);
}

#[tokio::test]
async fn a_wrap_whose_signature_fails_never_reaches_push() {
    let a = FakeRelay::start(false).await;
    let (relay, rec) = relay();
    let k1 = person(1);
    register(&relay, &k1, "t1", &[]);
    let (_w, mut st) = start(&relay, &[&a]);
    until(&mut st, 5, "baseline", |s| s.baselined == 1).await;

    let mut forged = wrap(&[&k1], 60, &[]);
    forged.content = "rewritten in flight".into();
    a.publish(&forged);
    let marker = wrap(&[&k1], 60, &[]);
    a.publish(&marker);
    let s = settled(&mut st, 1).await;
    assert_eq!(s.wraps, 1, "the forgery was dropped before the watcher");
    assert_eq!(pushed(&rec), [(k1, marker.id)]);
}

#[tokio::test]
async fn a_burst_of_registrations_resubscribes_once() {
    let a = FakeRelay::start(false).await;
    let (relay, _rec) = relay();
    register(&relay, &person(1), "t1", &[]);
    let (_w, mut st) = start(&relay, &[&a]);
    let before = until(&mut st, 5, "baseline", |s| s.baselined == 1).await;
    assert_eq!(a.sub_ids().len(), 1);

    for seed in 2..=6 {
        register(&relay, &person(seed), &format!("t{seed}"), &[]);
    }
    until(&mut st, 5, "the burst baselined", |s| s.baselined == 6).await;
    tokio::time::sleep(DEBOUNCE * 3).await;

    assert_eq!(st.borrow().resubscribes, before.resubscribes + 1);
    let ids = a.sub_ids();
    assert_eq!(
        ids.len(),
        2,
        "one subscription for the whole burst: {ids:?}"
    );
    let reqs = a.frames("REQ");
    let new = reqs.iter().find(|f| f[1] == ids[1].as_str()).unwrap();
    let mut keys: Vec<String> = serde_json::from_value(new[2]["#p"].clone()).unwrap();
    keys.sort();
    let mut want: Vec<String> = (1..=6).map(person).collect();
    want.sort();
    assert_eq!(keys, want);
    let closes = a.frames("CLOSE");
    assert_eq!(closes.len(), 1);
    assert_eq!(
        closes[0][1],
        ids[0].as_str(),
        "the old generation was closed"
    );
}

#[tokio::test]
async fn a_refresh_put_does_not_resubscribe() {
    let a = FakeRelay::start(false).await;
    let (relay, _rec) = relay();
    let k1 = person(1);
    register(&relay, &k1, "t1", &[]);
    let (_w, mut st) = start(&relay, &[&a]);
    let before = until(&mut st, 5, "baseline", |s| s.baselined == 1).await;
    register(&relay, &k1, "t1", &[]);
    tokio::time::sleep(DEBOUNCE * 3).await;
    assert_eq!(st.borrow().resubscribes, before.resubscribes);
    assert_eq!(a.sub_ids().len(), 1);
}

#[tokio::test]
async fn a_relay_outside_the_allow_list_is_never_dialled() {
    let a = FakeRelay::start(false).await;
    let outside = FakeRelay::start(false).await;
    let (relay, _rec) = relay();
    let (only_outside, anywhere, both) = (person(1), person(2), person(3));
    register(&relay, &only_outside, "t1", &[&outside.url]);
    register(&relay, &anywhere, "t2", &[]);
    register(&relay, &both, "t3", &[&outside.url, &format!("{}/", a.url)]);
    let (_w, mut st) = start(&relay, &[&a]);
    let s = until(&mut st, 5, "baseline", |s| s.baselined == 2).await;
    assert_eq!(s.relays, 1);
    let mut keys: Vec<String> =
        serde_json::from_value(a.frames("REQ")[0][2]["#p"].clone()).unwrap();
    keys.sort();
    let mut want = vec![anywhere, both];
    want.sort();
    assert_eq!(
        keys, want,
        "a key whose relays are all outside is not watched"
    );
    tokio::time::sleep(DEBOUNCE).await;
    assert_eq!(outside.accepts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_relay_left_with_no_keys_is_hung_up() {
    let a = FakeRelay::start(false).await;
    let (relay, _rec) = relay();
    let k1 = person(1);
    register(&relay, &k1, "t1", &[]);
    let (_w, mut st) = start(&relay, &[&a]);
    until(&mut st, 5, "baseline", |s| s.baselined == 1).await;
    relay.store.delete_device(&k1, "t1").unwrap();
    let s = until(&mut st, 5, "hang-up", |s| s.relays == 0).await;
    assert_eq!((s.watched, s.baselined), (0, 0));
}
