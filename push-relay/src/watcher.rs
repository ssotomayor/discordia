//! Gift wraps to registered keys, turned into pushes. Every subscription replays two
//! days of backdated wraps, so a (relay, key) pair pushes only after its first EOSE.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use discordia_nostr::event::Event;
use discordia_nostr::relay::{Filter, RelayEvent, RelayPool};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;
use tokio::time::Instant;

use crate::sender::short;
use crate::{DmEvent, DmPush, Relay, now_unix};

pub const KIND_GIFT_WRAP: u16 = 1059;
/// `nip59::MAX_BACKDATE` in the client.
const MAX_BACKDATE: i64 = 2 * 24 * 60 * 60;
const SINCE_SLACK: i64 = 60;
/// Wraps are only ever backdated; this is room for a sender's fast clock.
const FUTURE_SLACK: i64 = 60 * 60;
/// A live DM reaches every relay within seconds; a relay's first copy this long after
/// another's is a shifted replay, a re-publish or relay sync, and a push would show nothing.
pub const LATE_COPY_SECS: i64 = 10 * 60;

pub struct WatcherConfig {
    /// The only relays it dials. A device's own list narrows this, never widens it.
    pub relays: Vec<String>,
    /// A burst of registrations inside this window costs one resubscribe.
    pub debounce: Duration,
    /// Resubscribes everywhere, so a reconnect replays two days and not a week.
    pub refresh: Duration,
    /// strfry answers a stored query with at most 500 events per filter and holds
    /// at most 20 subscriptions per connection; smaller filters spend more of both.
    pub keys_per_filter: usize,
    pub max_subscriptions: usize,
    /// Wrap ids remembered; past it the oldest go and nothing at or before them is pushed.
    pub seen_cap: usize,
    pub max_in_flight: usize,
}

impl WatcherConfig {
    pub fn new(relays: Vec<String>) -> Self {
        Self {
            relays,
            debounce: Duration::from_secs(3),
            refresh: Duration::from_secs(6 * 60 * 60),
            keys_per_filter: 100,
            max_subscriptions: 8,
            seen_cap: 100_000,
            max_in_flight: 32,
        }
    }
}

/// Counts for the log and for tests to wait on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stats {
    /// Relays dialled now: only those with a key to watch.
    pub relays: usize,
    pub watched: usize,
    /// (relay, key) pairs past their first replay.
    pub baselined: usize,
    pub resubscribes: u64,
    /// Verified gift wraps received, duplicates included.
    pub wraps: u64,
    pub pushes_started: u64,
    pub pushes_done: u64,
}

pub struct Watcher {
    task: Option<tokio::task::JoinHandle<()>>,
    stats: watch::Receiver<Stats>,
}

impl Watcher {
    pub fn spawn(relay: Arc<Relay>, config: WatcherConfig) -> Watcher {
        let (stats_tx, stats) = watch::channel(Stats::default());
        let task = tokio::spawn(run(relay, config, stats_tx));
        Watcher {
            task: Some(task),
            stats,
        }
    }

    pub fn stats(&self) -> watch::Receiver<Stats> {
        self.stats.clone()
    }

    /// Drops every relay connection and any push still in flight.
    pub async fn stop(mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

/// Relay URLs as the allow-list and a device would both spell them.
fn norm(url: &str) -> String {
    url.trim().trim_end_matches('/').to_ascii_lowercase()
}

/// The keys to watch on each allow-listed relay, by its position in `allow`.
pub fn relay_keys(allow: &[String], watch_list: &[(String, Vec<String>)]) -> Vec<BTreeSet<String>> {
    let allowed: Vec<String> = allow.iter().map(|u| norm(u)).collect();
    let mut out = vec![BTreeSet::new(); allow.len()];
    for (pubkey, named) in watch_list {
        let named: HashSet<String> = named.iter().map(|u| norm(u)).collect();
        for (i, url) in allowed.iter().enumerate() {
            if named.is_empty() || named.contains(url) {
                out[i].insert(pubkey.clone());
            }
        }
    }
    out
}

/// Bit positions in `Seen`: one per allow-listed relay, one per `p` tag of a wrap.
pub const MAX_RELAYS: usize = 64;
const MAX_RECIPIENTS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Entry {
    created_at: i64,
    relays: u64,
    /// Per recipient by `p` tag position: one wrap may name keys baselined on different relays.
    pushed: u64,
    /// Our clock when any relay first delivered it.
    first_at: i64,
}

/// What was known of a wrap before this delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Prior {
    pub on_this_relay: bool,
    pub pushed: u64,
    pub first_at: i64,
}

/// Where one key stands on one relay; a key missing from `RelaySlot::watch` is
/// unwatched there. A redial or a forced resubscribe keeps it: `Seen` knows that
/// relay's replay, and what it gained while away is news.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Watch {
    Replaying,
    Baselined,
}

/// Everything `check` reads about one recipient of one delivered wrap.
#[derive(Debug, Clone, Copy)]
pub struct Facts {
    /// `None`: outside the window, or at or under the floor evictions raised.
    pub prior: Option<Prior>,
    pub call_signal: bool,
    pub now: i64,
    /// This relay's state for the recipient.
    pub watch: Option<Watch>,
    /// The recipient's bit in `Prior::pushed`.
    pub recipient: u64,
}

/// Which rule said no, in the order `check` asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    Forgotten,
    CallSignal,
    /// This relay sent it before: its own replay.
    Repeated,
    /// Another relay first sent it `LATE_COPY_SECS` or more ago.
    LateCopy,
    AlreadyPushed,
    Unwatched,
    Replaying,
}

/// Whether one delivery of a wrap is pushed to one of the keys it names.
pub fn check(f: &Facts) -> Result<(), Skip> {
    let Some(prior) = f.prior else {
        return Err(Skip::Forgotten);
    };
    // Only call signals carry an expiration, and a ring needs a design of its own.
    if f.call_signal {
        return Err(Skip::CallSignal);
    }
    // Only this relay's own repeat is its replay; another relay's first copy is news while moments old.
    if prior.on_this_relay {
        return Err(Skip::Repeated);
    }
    if f.now - prior.first_at >= LATE_COPY_SECS {
        return Err(Skip::LateCopy);
    }
    if prior.pushed & f.recipient != 0 {
        return Err(Skip::AlreadyPushed);
    }
    match f.watch {
        None => Err(Skip::Unwatched),
        Some(Watch::Replaying) => Err(Skip::Replaying),
        Some(Watch::Baselined) => Ok(()),
    }
}

/// Which relays delivered each wrap and to whom it was pushed. Bounded by age, since
/// nothing older than the window is pushed, and by count, raising a floor under evictions.
pub struct Seen {
    ids: HashMap<[u8; 32], Entry>,
    by_time: BTreeSet<(i64, [u8; 32])>,
    floor: i64,
    cap: usize,
}

impl Seen {
    pub fn new(cap: usize) -> Self {
        Self {
            ids: HashMap::new(),
            by_time: BTreeSet::new(),
            floor: i64::MIN,
            cap: cap.max(1),
        }
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// Notes that relay `relay` delivered `id`. `None` means it may never be pushed:
    /// outside the window, or at or under the floor that evictions raised.
    pub fn arrive(
        &mut self,
        id: [u8; 32],
        created_at: i64,
        relay: usize,
        now: i64,
    ) -> Option<Prior> {
        let oldest = now - MAX_BACKDATE - SINCE_SLACK;
        while let Some(&(t, old)) = self.by_time.first() {
            if t >= oldest {
                break;
            }
            self.by_time.pop_first();
            self.ids.remove(&old);
        }
        let bit = 1u64.checked_shl(relay as u32).unwrap_or(0);
        if let Some(entry) = self.ids.get_mut(&id) {
            let prior = Prior {
                on_this_relay: entry.relays & bit != 0,
                pushed: entry.pushed,
                first_at: entry.first_at,
            };
            entry.relays |= bit;
            return Some(prior);
        }
        if created_at < oldest || created_at > now + FUTURE_SLACK || created_at <= self.floor {
            return None;
        }
        self.ids.insert(
            id,
            Entry {
                created_at,
                relays: bit,
                pushed: 0,
                first_at: now,
            },
        );
        self.by_time.insert((created_at, id));
        while self.ids.len() > self.cap {
            let Some((t, old)) = self.by_time.pop_first() else {
                break;
            };
            self.ids.remove(&old);
            self.floor = self.floor.max(t);
        }
        if !self.ids.contains_key(&id) {
            return None;
        }
        Some(Prior {
            on_this_relay: false,
            pushed: 0,
            first_at: now,
        })
    }

    pub fn pushed(&mut self, id: &[u8; 32], recipients: u64) {
        if let Some(entry) = self.ids.get_mut(id) {
            entry.pushed |= recipients;
        }
    }
}

struct RelaySlot {
    url: String,
    pool: Option<RelayPool>,
    /// Which pool a forwarded event came from, so a dropped pool's stragglers are ignored.
    epoch: u64,
    /// The current generation's subscription ids and the keys each covers.
    subs: HashMap<String, Vec<String>>,
    watch: BTreeMap<String, Watch>,
}

type Forwarded = (usize, u64, RelayEvent);

struct State {
    relay: Arc<Relay>,
    config: WatcherConfig,
    slots: Vec<RelaySlot>,
    seen: Seen,
    generation: u64,
    epochs: u64,
    tx: mpsc::UnboundedSender<Forwarded>,
    stats: watch::Sender<Stats>,
}

async fn run(relay: Arc<Relay>, config: WatcherConfig, stats: watch::Sender<Stats>) {
    let mut allow: Vec<String> = Vec::new();
    for url in &config.relays {
        if !allow.iter().any(|a| norm(a) == norm(url)) {
            allow.push(url.clone());
        }
    }
    if allow.len() > MAX_RELAYS {
        tracing::warn!(
            configured = allow.len(),
            used = MAX_RELAYS,
            "DM watcher ignores relays past the first {MAX_RELAYS}"
        );
        allow.truncate(MAX_RELAYS);
    }
    let (tx, mut rx) = mpsc::unbounded_channel::<Forwarded>();
    let mut changes = relay.store.changes();
    let mut state = State {
        slots: allow
            .into_iter()
            .map(|url| RelaySlot {
                url,
                pool: None,
                epoch: 0,
                subs: HashMap::new(),
                watch: BTreeMap::new(),
            })
            .collect(),
        seen: Seen::new(config.seen_cap),
        generation: 0,
        epochs: 0,
        relay,
        config,
        tx,
        stats,
    };
    changes.borrow_and_update();
    state.resync(false);
    let mut refresh =
        tokio::time::interval_at(Instant::now() + state.config.refresh, state.config.refresh);
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut debounce: Option<Instant> = None;
    let mut pushes: JoinSet<()> = JoinSet::new();
    loop {
        tokio::select! {
            changed = changes.changed() => {
                if changed.is_err() {
                    return;
                }
                changes.borrow_and_update();
                debounce.get_or_insert(Instant::now() + state.config.debounce);
            }
            _ = async {
                match debounce {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            } => {
                debounce = None;
                state.resync(false);
            }
            _ = refresh.tick() => {
                let s = state.stats.borrow().clone();
                tracing::info!(wraps = s.wraps, pushes = s.pushes_started, watched = s.watched, "DM watcher refreshing");
                state.resync(true);
            }
            Some((slot, epoch, event)) = rx.recv() => {
                for (pubkey, wrap) in state.on_event(slot, epoch, event) {
                    while pushes.len() >= state.config.max_in_flight {
                        if pushes.join_next().await.is_some() {
                            state.stats.send_modify(|s| s.pushes_done += 1);
                        }
                    }
                    state.stats.send_modify(|s| s.pushes_started += 1);
                    let relay = state.relay.clone();
                    pushes.spawn(push(relay, pubkey, wrap));
                }
            }
            Some(_) = pushes.join_next(), if !pushes.is_empty() => {
                state.stats.send_modify(|s| s.pushes_done += 1);
            }
        }
    }
}

struct Wrap {
    id: String,
    json: String,
    relay: String,
}

async fn push(relay: Arc<Relay>, pubkey: String, wrap: Wrap) {
    let event = DmEvent {
        id: &wrap.id,
        json: &wrap.json,
        relay: &wrap.relay,
    };
    match relay.push_dm(&pubkey, &event).await {
        Ok(DmPush::Sent { devices, delivered }) => tracing::debug!(
            id = short(&wrap.id),
            pubkey = short(&pubkey),
            devices,
            delivered,
            "DM pushed"
        ),
        Ok(other) => tracing::debug!(
            id = short(&wrap.id),
            pubkey = short(&pubkey),
            ?other,
            "DM not pushed"
        ),
        Err(e) => tracing::error!(error = %e, "DM push failed"),
    }
}

impl State {
    fn resync(&mut self, force: bool) {
        let list = match self.relay.store.dm_watch_list(now_unix()) {
            Ok(list) => list,
            Err(e) => {
                tracing::error!(?e, "DM watcher could not read its watch list");
                return;
            }
        };
        let urls: Vec<String> = self.slots.iter().map(|s| s.url.clone()).collect();
        let wanted = relay_keys(&urls, &list);
        let since = now_unix() as i64 - MAX_BACKDATE - SINCE_SLACK;
        let mut changed = 0;
        for (i, keys) in wanted.into_iter().enumerate() {
            if !force && self.slots[i].watch.keys().eq(keys.iter()) {
                continue;
            }
            if keys.is_empty() && self.slots[i].pool.is_none() {
                continue;
            }
            changed += 1;
            self.generation += 1;
            self.apply(i, keys, since);
        }
        if changed > 0 {
            self.stats.send_modify(|s| s.resubscribes += 1);
        }
        self.publish_counts(list.len());
        if changed > 0 {
            tracing::info!(
                keys = list.len(),
                relays = changed,
                "DM watcher resubscribed"
            );
        }
    }

    fn apply(&mut self, i: usize, keys: BTreeSet<String>, since: i64) {
        let generation = self.generation;
        if keys.is_empty() {
            let slot = &mut self.slots[i];
            slot.pool = None;
            slot.subs.clear();
            slot.watch.clear();
            return;
        }
        if self.slots[i].pool.is_none() {
            self.epochs += 1;
            let epoch = self.epochs;
            let (pool, mut events) = RelayPool::connect(vec![self.slots[i].url.clone()]);
            let tx = self.tx.clone();
            tokio::spawn(async move {
                while let Some(event) = events.recv().await {
                    if tx.send((i, epoch, event)).is_err() {
                        return;
                    }
                }
            });
            self.slots[i].pool = Some(pool);
            self.slots[i].epoch = epoch;
        }
        let per_filter = self.config.keys_per_filter.max(1);
        let max_subs = self.config.max_subscriptions.max(1);
        let slot = &mut self.slots[i];
        let Some(pool) = &slot.pool else { return };
        // Closed first: a relay at its subscription cap would refuse the new ones,
        // and the new replay covers whatever lands in between.
        for id in slot.subs.keys() {
            pool.unsubscribe(id);
        }
        slot.subs.clear();
        let all: Vec<&String> = keys.iter().collect();
        let filters: Vec<&[&String]> = all.chunks(per_filter).collect();
        let per_sub = filters.len().div_ceil(max_subs);
        for (n, group) in filters.chunks(per_sub).enumerate() {
            let id = format!("dxp{generation}-{n}");
            let covered: Vec<String> = group
                .iter()
                .flat_map(|f| f.iter().map(|k| (*k).clone()))
                .collect();
            let filters = group
                .iter()
                .map(|chunk| Filter {
                    kinds: Some(vec![KIND_GIFT_WRAP]),
                    p: Some(chunk.iter().map(|k| (*k).clone()).collect()),
                    since: Some(since),
                    ..Default::default()
                })
                .collect();
            pool.subscribe(&id, filters);
            slot.subs.insert(id, covered);
        }
        let held = std::mem::take(&mut slot.watch);
        slot.watch = keys
            .into_iter()
            .map(|k| {
                let state = held.get(&k).copied().unwrap_or(Watch::Replaying);
                (k, state)
            })
            .collect();
    }

    fn publish_counts(&self, watched: usize) {
        let relays = self.slots.iter().filter(|s| s.pool.is_some()).count();
        let baselined = self
            .slots
            .iter()
            .flat_map(|s| s.watch.values())
            .filter(|w| **w == Watch::Baselined)
            .count();
        self.stats.send_modify(|s| {
            s.relays = relays;
            s.watched = watched;
            s.baselined = baselined;
        });
    }

    /// The (recipient, wrap) pairs to push for one forwarded relay event.
    fn on_event(&mut self, i: usize, epoch: u64, event: RelayEvent) -> Vec<(String, Wrap)> {
        let Some(slot) = self.slots.get_mut(i) else {
            return Vec::new();
        };
        if slot.epoch != epoch || slot.pool.is_none() {
            return Vec::new();
        }
        match event {
            RelayEvent::EndOfStored { subscription, .. } => {
                if let Some(keys) = slot.subs.get(&subscription) {
                    let mut fresh = 0;
                    for key in keys {
                        if let Some(state @ Watch::Replaying) = slot.watch.get_mut(key) {
                            *state = Watch::Baselined;
                            fresh += 1;
                        }
                    }
                    if fresh > 0 {
                        tracing::debug!(
                            relay = %slot.url,
                            keys = fresh,
                            "DM watcher baselined"
                        );
                        let watched = self.stats.borrow().watched;
                        self.publish_counts(watched);
                    }
                }
                Vec::new()
            }
            RelayEvent::Event { event, .. } => self.on_wrap(i, &event),
            RelayEvent::Connected(url) => {
                tracing::debug!(relay = %url, "DM watcher connected");
                Vec::new()
            }
            RelayEvent::Disconnected { relay, why } => {
                tracing::debug!(%relay, %why, "DM watcher disconnected");
                Vec::new()
            }
            RelayEvent::Published { .. } => Vec::new(),
        }
    }

    fn on_wrap(&mut self, i: usize, event: &Event) -> Vec<(String, Wrap)> {
        if event.kind != KIND_GIFT_WRAP {
            return Vec::new();
        }
        self.stats.send_modify(|s| s.wraps += 1);
        let Some(id) = hex::decode(&event.id)
            .ok()
            .and_then(|b| <[u8; 32]>::try_from(b).ok())
        else {
            return Vec::new();
        };
        let now = now_unix() as i64;
        let prior = self.seen.arrive(id, event.created_at, i, now);
        let call_signal = event.tag("expiration").is_some();
        let slot = &self.slots[i];
        let mut named: Vec<&str> = Vec::new();
        let mut pushing = 0u64;
        let mut recipients: Vec<&str> = Vec::new();
        let p_tags = event
            .tags
            .iter()
            .filter(|t| t.first().map(String::as_str) == Some("p"))
            .filter_map(|t| t.get(1).map(String::as_str))
            .take(MAX_RECIPIENTS);
        for (n, p) in p_tags.enumerate() {
            if named.contains(&p) {
                continue;
            }
            named.push(p);
            let facts = Facts {
                prior,
                call_signal,
                now,
                watch: slot.watch.get(p).copied(),
                recipient: 1u64 << n,
            };
            match check(&facts) {
                Ok(()) => {
                    pushing |= facts.recipient;
                    recipients.push(p);
                }
                Err(skip) => tracing::trace!(id = short(&event.id), ?skip, "DM wrap not pushed"),
            }
        }
        if recipients.is_empty() {
            return Vec::new();
        }
        let Ok(json) = serde_json::to_string(event) else {
            return Vec::new();
        };
        let relay = slot.url.clone();
        self.seen.pushed(&id, pushing);
        recipients
            .into_iter()
            .map(|p| {
                (
                    p.to_string(),
                    Wrap {
                        id: event.id.clone(),
                        json: json.clone(),
                        relay: relay.clone(),
                    },
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn urls(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_device_narrows_the_allow_list_and_never_widens_it() {
        let allow = urls(&["wss://a.example", "wss://B.example/"]);
        let list = vec![
            ("k1".to_string(), vec![]),
            ("k2".to_string(), urls(&["wss://b.example"])),
            ("k3".to_string(), urls(&["wss://elsewhere.example"])),
            (
                "k4".to_string(),
                urls(&["wss://A.example/", "wss://c.example"]),
            ),
        ];
        let keys = relay_keys(&allow, &list);
        assert_eq!(
            keys.len(),
            2,
            "one set per allow-listed relay, nothing else"
        );
        assert_eq!(
            keys[0].iter().map(String::as_str).collect::<Vec<_>>(),
            ["k1", "k4"]
        );
        assert_eq!(
            keys[1].iter().map(String::as_str).collect::<Vec<_>>(),
            ["k1", "k2"]
        );
    }

    fn id(n: u32) -> [u8; 32] {
        let mut id = [0u8; 32];
        id[..4].copy_from_slice(&n.to_be_bytes());
        id
    }

    const NOW: i64 = 1_800_000_000;

    const A: usize = 0;
    const B: usize = 1;

    fn fresh() -> Option<Prior> {
        fresh_at(NOW)
    }

    fn fresh_at(now: i64) -> Option<Prior> {
        Some(Prior {
            on_this_relay: false,
            pushed: 0,
            first_at: now,
        })
    }

    /// Everything but the delivery's own history says push.
    fn facts(prior: Option<Prior>, now: i64) -> Facts {
        Facts {
            prior,
            call_signal: false,
            now,
            watch: Some(Watch::Baselined),
            recipient: 0b10,
        }
    }

    fn news(prior: Option<Prior>, now: i64) -> bool {
        check(&facts(prior, now)).is_ok()
    }

    #[test]
    fn each_rule_says_no_on_its_own_and_in_order() {
        type Change = fn(&mut Facts);
        let rows: [(&str, Change, Skip); 7] = [
            ("1 forgotten", |f| f.prior = None, Skip::Forgotten),
            (
                "2 a call signal",
                |f| f.call_signal = true,
                Skip::CallSignal,
            ),
            (
                "3 this relay's own replay",
                |f| f.prior.as_mut().unwrap().on_this_relay = true,
                Skip::Repeated,
            ),
            (
                "4 a late copy",
                |f| f.now = NOW + LATE_COPY_SECS,
                Skip::LateCopy,
            ),
            (
                "5 pushed to this key",
                |f| f.prior.as_mut().unwrap().pushed = 0b11,
                Skip::AlreadyPushed,
            ),
            ("6 not watched here", |f| f.watch = None, Skip::Unwatched),
            (
                "7 still replaying here",
                |f| f.watch = Some(Watch::Replaying),
                Skip::Replaying,
            ),
        ];
        let pushes = facts(fresh(), NOW);
        assert_eq!(check(&pushes), Ok(()));
        let mut other = pushes;
        other.prior.as_mut().unwrap().pushed = 0b01;
        assert_eq!(check(&other), Ok(()), "pushed to another key only");
        let mut moments = pushes;
        moments.now = NOW + LATE_COPY_SECS - 1;
        assert_eq!(check(&moments), Ok(()));
        for (rule, change, skip) in rows {
            let mut f = pushes;
            change(&mut f);
            assert_eq!(check(&f), Err(skip), "rule {rule}");
        }
        let mut everything = pushes;
        everything.call_signal = true;
        everything.watch = None;
        everything.prior.as_mut().unwrap().on_this_relay = true;
        assert_eq!(
            check(&everything),
            Err(Skip::CallSignal),
            "the first rule wins"
        );
    }

    #[test]
    fn a_relay_repeating_itself_is_known_and_another_relay_is_not() {
        let mut seen = Seen::new(10);
        assert_eq!(seen.arrive(id(1), NOW - 100, B, NOW), fresh());
        let again = seen.arrive(id(1), NOW - 100, B, NOW).unwrap();
        assert!(again.on_this_relay);
        let elsewhere = seen.arrive(id(1), NOW - 100, A, NOW).unwrap();
        assert!(!elsewhere.on_this_relay && elsewhere.pushed == 0);
        seen.pushed(&id(1), 0b1);
        let third = seen.arrive(id(1), NOW - 100, 63, NOW).unwrap();
        assert_eq!((third.on_this_relay, third.pushed), (false, 0b1));
    }

    #[test]
    fn nothing_outside_the_window_is_news_or_kept() {
        let mut seen = Seen::new(10);
        assert_eq!(
            seen.arrive(id(1), NOW - MAX_BACKDATE - SINCE_SLACK - 1, A, NOW),
            None
        );
        assert_eq!(seen.arrive(id(2), NOW + FUTURE_SLACK + 1, A, NOW), None);
        assert!(seen.is_empty());
        assert_eq!(seen.arrive(id(3), NOW - MAX_BACKDATE, A, NOW), fresh());
        assert_eq!(
            seen.arrive(id(4), NOW, A, NOW + 3 * MAX_BACKDATE),
            None,
            "a stale since replaying the past pushes nothing"
        );
        assert!(seen.len() <= 1, "aged out: {}", seen.len());
    }

    #[test]
    fn past_the_cap_an_evicted_id_is_never_news_from_any_relay() {
        let mut seen = Seen::new(3);
        for n in 0..10 {
            assert!(seen.arrive(id(n), NOW - 1000 + n as i64, A, NOW).is_some());
        }
        assert_eq!(seen.len(), 3);
        for n in 0..7 {
            for relay in [A, B] {
                assert_eq!(
                    seen.arrive(id(n), NOW - 1000 + n as i64, relay, NOW),
                    None,
                    "{n} came back from relay {relay}"
                );
            }
        }
        assert_eq!(
            seen.arrive(id(99), NOW - 10, B, NOW),
            fresh(),
            "newer than the floor"
        );
    }

    #[test]
    fn another_relays_first_copy_is_news_only_soon_after_the_first_delivery() {
        let mut seen = Seen::new(10);
        assert!(news(seen.arrive(id(1), NOW - 100, B, NOW), NOW));
        let seconds = NOW + 5;
        assert!(
            news(seen.arrive(id(1), NOW - 100, A, seconds), seconds),
            "a copy moments behind is the lost-push case and still news"
        );

        seen.arrive(id(2), NOW - 100, B, NOW);
        let nine = NOW + 9 * 60;
        assert!(news(seen.arrive(id(2), NOW - 100, A, nine), nine));

        seen.arrive(id(3), NOW - 100, B, NOW);
        let eleven = NOW + 11 * 60;
        let late = seen.arrive(id(3), NOW - 100, A, eleven);
        assert!(!news(late, eleven));
        assert_eq!(
            late.map(|l| (l.pushed, l.first_at)),
            Some((0, NOW)),
            "not pushed, and the first delivery's time is kept"
        );
        let third = NOW + 12 * 60;
        assert!(!news(seen.arrive(id(3), NOW - 100, 2, third), third));
        assert_eq!(seen.arrive(id(4), NOW - 100, A, eleven), fresh_at(eleven));
    }
}
