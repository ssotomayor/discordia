//! Mention pushes: who a message should wake, and posting its ciphertext to the
//! one relay the operator configured. Nothing a client sends picks the host.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use dashmap::DashMap;
use tokio::sync::mpsc;

use crate::livekit::BoxFuture;
use crate::protocol::push::{
    DEFAULT_PUSH_RELAY, HANDLE_BYTES, MAX_PUSH_BODY_BYTES, MentionPayload, PushReason,
};
use crate::protocol::{Id, Message};
use crate::state::{AppState, durable};
use crate::store::PushRow;

pub const MAX_REGISTRATIONS_PER_KEY: usize = 4;
pub const REGISTRATION_TTL: Duration = Duration::from_secs(30 * 24 * 3600);
pub const PUSHES_PER_MINUTE: usize = 20;
const RATE_WINDOW: Duration = Duration::from_secs(60);
/// Jobs waiting for the worker. Past it a mention is not pushed: the socket
/// that committed the message never waits on the relay.
pub const QUEUE_CAP: usize = 256;
const IN_FLIGHT: usize = 8;
const SEND_TIMEOUT: Duration = Duration::from_secs(5);
const RETRY_AFTER: Duration = Duration::from_secs(1);

pub const HANDLE_LEN: usize = (HANDLE_BYTES * 8).div_ceil(6);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Accepted,
    Gone,
    Retry,
    Rejected,
}

pub trait PushRelay: Send + Sync {
    fn push<'a>(&'a self, handle: &'a str, body: String) -> BoxFuture<'a, Outcome>;
}

pub struct HttpRelay {
    base: String,
    http: reqwest::Client,
}

impl HttpRelay {
    pub fn new(base: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            // A redirect would let the relay's answer choose another host.
            http: reqwest::Client::builder()
                .timeout(SEND_TIMEOUT)
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap_or_default(),
        }
    }
}

impl PushRelay for HttpRelay {
    fn push<'a>(&'a self, handle: &'a str, body: String) -> BoxFuture<'a, Outcome> {
        Box::pin(async move {
            if !valid_handle(handle) {
                return Outcome::Rejected;
            }
            let sent = self
                .http
                .post(format!("{}/v1/push/{handle}", self.base))
                .header(reqwest::header::CONTENT_TYPE, "text/plain")
                .body(body)
                .send()
                .await;
            match sent {
                Ok(res) => outcome_of(res.status().as_u16()),
                Err(_) => Outcome::Retry,
            }
        })
    }
}

pub fn outcome_of(status: u16) -> Outcome {
    match status {
        200..=299 => Outcome::Accepted,
        404 | 410 => Outcome::Gone,
        429 | 500..=599 => Outcome::Retry,
        _ => Outcome::Rejected,
    }
}

/// Off by default, so a server built in a test never dials the project relay;
/// `serve` and the self-host ask for `from_setting`.
#[derive(Clone, Default)]
pub struct PushConfig {
    url: Option<String>,
    relay: Option<Arc<dyn PushRelay>>,
}

impl std::fmt::Debug for PushConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PushConfig")
            .field("url", &self.url)
            .finish()
    }
}

impl PushConfig {
    pub fn off() -> Self {
        Self::default()
    }

    /// Unset is the project's relay; empty or `off` is none.
    pub fn from_setting(raw: Option<&str>) -> Result<Self, String> {
        match raw.map(str::trim) {
            None => Self::http(DEFAULT_PUSH_RELAY),
            Some(s) if s.is_empty() || s.eq_ignore_ascii_case("off") => Ok(Self::off()),
            Some(s) => Self::http(s),
        }
    }

    pub fn http(url: &str) -> Result<Self, String> {
        let base = relay_base(url)?;
        Ok(Self {
            relay: Some(Arc::new(HttpRelay::new(&base))),
            url: Some(base),
        })
    }

    /// `url` is only what clients are told; `relay` is what is called.
    pub fn with_relay(url: &str, relay: Arc<dyn PushRelay>) -> Result<Self, String> {
        Ok(Self {
            url: Some(relay_base(url)?),
            relay: Some(relay),
        })
    }

    pub fn url(&self) -> Option<&str> {
        self.url.as_deref()
    }
}

/// Plain `http` only to loopback, where a test's or a co-located relay listens.
/// Kept as written, less a trailing `/`: a client compares it with its own.
pub fn relay_base(raw: &str) -> Result<String, String> {
    let base = raw.trim().trim_end_matches('/');
    let refused = || {
        format!(
            "push relay {base:?}: needs an https:// URL without a query, or http:// on loopback"
        )
    };
    let url = reqwest::Url::parse(base).map_err(|_| refused())?;
    let host = url.host_str().unwrap_or_default();
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    let scheme_ok = url.scheme() == "https" || (url.scheme() == "http" && loopback);
    if !scheme_ok
        || host.is_empty()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(refused());
    }
    Ok(base.to_string())
}

/// The handle becomes a path segment, so only the relay's own alphabet passes.
pub fn valid_handle(handle: &str) -> bool {
    handle.len() == HANDLE_LEN
        && handle
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        && URL_SAFE_NO_PAD
            .decode(handle)
            .is_ok_and(|bytes| bytes.len() == HANDLE_BYTES)
}

pub fn decode_key(key: &str) -> Option<[u8; 32]> {
    let key = key.trim();
    if key.len() > 64 {
        return None;
    }
    [STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD]
        .iter()
        .find_map(|engine| engine.decode(key).ok())
        .and_then(|bytes| bytes.try_into().ok())
}

/// The first eight characters: enough to match log lines, too few to push with.
fn short(handle: &str) -> &str {
    handle.get(..8).unwrap_or(handle)
}

/// Everything `decide` reads about one recipient, gathered beforehand so no
/// lock is held while it runs.
#[derive(Debug, Clone)]
pub struct Facts<'a> {
    pub recipient: &'a str,
    pub author: &'a str,
    /// The recipient's name in this guild, `None` when not a member.
    pub name: Option<&'a str>,
    pub bot: bool,
    pub registered: bool,
    pub sees_channel: bool,
    pub content: &'a str,
    pub dnd: bool,
    pub attended: bool,
    pub recent: usize,
}

/// Which rule said no, in the order `check` asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    Author,
    NotMember,
    Bot,
    Unregistered,
    Hidden,
    NotMentioned,
    DoNotDisturb,
    Attended,
    RateLimited,
}

pub fn check(f: &Facts) -> Result<PushReason, Skip> {
    if f.recipient == f.author {
        return Err(Skip::Author);
    }
    let Some(name) = f.name else {
        return Err(Skip::NotMember);
    };
    if f.bot {
        return Err(Skip::Bot);
    }
    if !f.registered {
        return Err(Skip::Unregistered);
    }
    // Text channels cannot be hidden today; this keeps a future ACL from leaking through push.
    if !f.sees_channel {
        return Err(Skip::Hidden);
    }
    if name.is_empty() || !crate::protocol::mentions_name(f.content, name) {
        return Err(Skip::NotMentioned);
    }
    if f.dnd {
        return Err(Skip::DoNotDisturb);
    }
    if f.attended {
        return Err(Skip::Attended);
    }
    if f.recent >= PUSHES_PER_MINUTE {
        return Err(Skip::RateLimited);
    }
    Ok(PushReason::Mention)
}

pub fn decide(f: &Facts) -> Option<PushReason> {
    check(f).ok()
}

/// NIP-44 under the handle's key, used directly as the conversation key. Names
/// are cut until the body fits rather than the push being lost.
pub fn seal(payload: &MentionPayload, key: &[u8; 32]) -> Option<String> {
    for cap in [usize::MAX, 128, 64, 32, 16, 8, 0] {
        let mut p = payload.clone();
        for name in [&mut p.author_name, &mut p.guild_name, &mut p.channel_name] {
            if let Some((at, _)) = name.char_indices().nth(cap) {
                name.truncate(at);
            }
        }
        let Ok(json) = serde_json::to_string(&p) else {
            return None;
        };
        if let Ok(body) = discordia_nostr::nip44::encrypt(key, &json)
            && body.len() <= MAX_PUSH_BODY_BYTES
        {
            return Some(body);
        }
    }
    None
}

#[derive(Clone)]
struct Registration {
    handle: String,
    key: [u8; 32],
    updated_at: i64,
}

impl Registration {
    fn from_row(row: &PushRow) -> Option<Self> {
        Some(Self {
            handle: row.handle.clone(),
            key: decode_key(&row.key)?,
            updated_at: row.updated_at,
        })
    }
}

struct Job {
    handle: String,
    key: [u8; 32],
    payload: MentionPayload,
}

struct Active {
    url: String,
    queue: mpsc::Sender<Job>,
}

/// The server's push state: memory mirrors of `push_registrations`, written
/// only after SQLite commits, and the queue to the worker.
#[derive(Default)]
pub struct Push {
    active: std::sync::OnceLock<Active>,
    registrations: DashMap<String, Vec<Registration>>,
    recent: std::sync::Mutex<HashMap<String, VecDeque<Instant>>>,
    dropped: AtomicU64,
}

impl Push {
    pub fn url(&self) -> Option<String> {
        self.active.get().map(|a| a.url.clone())
    }

    fn recent_lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, VecDeque<Instant>>> {
        self.recent.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn recent_count(&self, pubkey: &str, now: Instant) -> usize {
        let mut recent = self.recent_lock();
        let Some(sent) = recent.get_mut(pubkey) else {
            return 0;
        };
        while sent
            .front()
            .is_some_and(|at| now.duration_since(*at) >= RATE_WINDOW)
        {
            sent.pop_front();
        }
        sent.len()
    }

    /// Checked again under the lock: two messages at once must not both take the last slot.
    fn take(&self, pubkey: &str, now: Instant) -> bool {
        let mut recent = self.recent_lock();
        let sent = recent.entry(pubkey.to_string()).or_default();
        while sent
            .front()
            .is_some_and(|at| now.duration_since(*at) >= RATE_WINDOW)
        {
            sent.pop_front();
        }
        if sent.len() >= PUSHES_PER_MINUTE {
            return false;
        }
        sent.push_back(now);
        true
    }

    fn sweep_recent(&self, now: Instant) {
        self.recent_lock().retain(|_, sent| {
            sent.back()
                .is_some_and(|at| now.duration_since(*at) < RATE_WINDOW)
        });
    }
}

/// Once, before the first connection.
pub async fn start(state: &Arc<AppState>, config: PushConfig) -> Result<(), sqlx::Error> {
    state.prune_push_registrations().await;
    let (Some(url), Some(relay)) = (config.url, config.relay) else {
        tracing::info!("mention push off");
        return Ok(());
    };
    for row in state.store.push_registrations(None).await? {
        if let Some(registration) = Registration::from_row(&row) {
            state
                .push
                .registrations
                .entry(row.pubkey)
                .or_default()
                .push(registration);
        }
    }
    let (queue, jobs) = mpsc::channel(QUEUE_CAP);
    tracing::info!(relay = %url, "mention push through");
    if state.push.active.set(Active { url, queue }).is_ok() {
        tokio::spawn(worker(state.clone(), relay, jobs));
    }
    Ok(())
}

async fn worker(state: Arc<AppState>, relay: Arc<dyn PushRelay>, mut jobs: mpsc::Receiver<Job>) {
    let slots = Arc::new(tokio::sync::Semaphore::new(IN_FLIGHT));
    while let Some(job) = jobs.recv().await {
        let Ok(slot) = slots.clone().acquire_owned().await else {
            break;
        };
        let (state, relay) = (state.clone(), relay.clone());
        tokio::spawn(async move {
            let _slot = slot;
            send(&state, relay.as_ref(), job).await;
        });
    }
}

async fn send(state: &AppState, relay: &dyn PushRelay, job: Job) {
    let Some(body) = seal(&job.payload, &job.key) else {
        tracing::warn!(
            handle = short(&job.handle),
            "mention push could not be sealed"
        );
        return;
    };
    let mut outcome = attempt(relay, &job.handle, body.clone()).await;
    if outcome == Outcome::Retry {
        tokio::time::sleep(RETRY_AFTER).await;
        outcome = attempt(relay, &job.handle, body).await;
    }
    match outcome {
        Outcome::Accepted => {}
        Outcome::Gone => state.forget_push_handle(&job.handle).await,
        other => {
            tracing::debug!(handle = short(&job.handle), ?other, "mention push dropped")
        }
    }
}

async fn attempt(relay: &dyn PushRelay, handle: &str, body: String) -> Outcome {
    tokio::time::timeout(SEND_TIMEOUT, relay.push(handle, body))
        .await
        .unwrap_or(Outcome::Retry)
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

impl AppState {
    pub async fn register_push(&self, pubkey: &str, handle: &str, key: &str) -> Result<(), String> {
        if self.push.active.get().is_none() {
            return Err("this server does not send push notifications".into());
        }
        if !valid_handle(handle) {
            return Err("that is not a push relay handle".into());
        }
        let key = decode_key(key).ok_or("a push key is 32 bytes of base64")?;
        let row = PushRow {
            handle: handle.to_string(),
            pubkey: pubkey.to_string(),
            key: STANDARD.encode(key),
            updated_at: now_ms(),
        };
        let _write = self.durable_writes.lock().await;
        let moved_from = durable(
            self.store
                .upsert_push_registration(&row, MAX_REGISTRATIONS_PER_KEY)
                .await,
            "push registration",
        )?;
        for owner in std::iter::once(pubkey).chain(moved_from.as_deref()) {
            self.reload_push_registrations(owner).await?;
        }
        Ok(())
    }

    pub async fn unregister_push(&self, pubkey: &str, handle: &str) -> Result<(), String> {
        if !valid_handle(handle) {
            return Err("that is not a push relay handle".into());
        }
        let _write = self.durable_writes.lock().await;
        durable(
            self.store
                .delete_push_registration(handle, Some(pubkey))
                .await,
            "push unregistration",
        )?;
        self.reload_push_registrations(pubkey).await
    }

    /// The relay answered 404 or 410, so nobody can be woken through it again.
    async fn forget_push_handle(&self, handle: &str) {
        let _write = self.durable_writes.lock().await;
        if let Err(error) = self.store.delete_push_registration(handle, None).await {
            tracing::error!(%error, handle = short(handle), "could not drop a gone push handle");
            return;
        }
        let owners: Vec<String> = self
            .push
            .registrations
            .iter()
            .filter(|e| e.value().iter().any(|r| r.handle == handle))
            .map(|e| e.key().clone())
            .collect();
        for owner in owners {
            let _ = self.reload_push_registrations(&owner).await;
        }
        tracing::info!(
            handle = short(handle),
            "push handle gone; registration dropped"
        );
    }

    /// Callers hold `durable_writes`, and have committed.
    async fn reload_push_registrations(&self, pubkey: &str) -> Result<(), String> {
        let rows = durable(
            self.store.push_registrations(Some(pubkey)).await,
            "push registrations",
        )?;
        let held: Vec<Registration> = rows.iter().filter_map(Registration::from_row).collect();
        if held.is_empty() {
            self.push.registrations.remove(pubkey);
        } else {
            self.push.registrations.insert(pubkey.to_string(), held);
        }
        Ok(())
    }

    pub async fn prune_push_registrations(&self) -> u64 {
        let cutoff = now_ms() - REGISTRATION_TTL.as_millis() as i64;
        let _write = self.durable_writes.lock().await;
        let pruned = match self.store.prune_push_registrations(cutoff).await {
            Ok(pruned) => pruned,
            Err(error) => {
                tracing::error!(%error, "push registration prune skipped");
                return 0;
            }
        };
        self.push.registrations.retain(|_, held| {
            held.retain(|r| r.updated_at >= cutoff);
            !held.is_empty()
        });
        self.push.sweep_recent(Instant::now());
        pruned
    }

    /// After a committed message's `deliver`. Only enqueues: the relay is the
    /// worker's to wait on, never a socket loop's.
    pub fn notify_offline_mentions(&self, guild_id: Id, message: &Message) {
        let Some(active) = self.push.active.get() else {
            return;
        };
        if !message.content.contains('@') {
            return;
        }
        let Some(channel) = self.channels.get(&message.channel_id).map(|c| c.clone()) else {
            return;
        };
        let guild_name = self
            .guilds
            .get(&guild_id)
            .map(|g| g.name.clone())
            .unwrap_or_default();
        let registered: Vec<(String, Vec<Registration>)> = self
            .push
            .registrations
            .iter()
            .map(|e| (e.key().clone(), e.value().clone()))
            .collect();
        let now = Instant::now();
        for (pubkey, held) in registered {
            let member = self.members.get(&guild_id).and_then(|members| {
                members
                    .get(&pubkey)
                    .map(|m| (m.user.username.clone(), m.bot))
            });
            let Some((name, member_bot)) = member else {
                continue;
            };
            let dnd = self
                .profiles
                .get(&pubkey)
                .is_some_and(|p| p.status.as_deref() == Some("dnd"));
            let facts = Facts {
                recipient: &pubkey,
                author: &message.author.pubkey,
                name: Some(name.as_str()),
                bot: member_bot || self.bot_install(guild_id, &pubkey).is_some(),
                registered: !held.is_empty(),
                sees_channel: self.can_see_channel(&pubkey, &channel),
                content: &message.content,
                dnd,
                attended: self.attended(&pubkey),
                recent: self.push.recent_count(&pubkey, now),
            };
            if decide(&facts).is_none() || !self.push.take(&pubkey, now) {
                continue;
            }
            let payload = MentionPayload {
                v: 1,
                k: PushReason::Mention,
                guild: guild_id,
                channel: channel.id,
                message: message.id,
                author: message.author.pubkey.clone(),
                author_name: message.author.username.clone(),
                guild_name: guild_name.clone(),
                channel_name: channel.name.clone(),
                at: message.created_at.timestamp(),
            };
            for registration in held {
                let job = Job {
                    handle: registration.handle,
                    key: registration.key,
                    payload: payload.clone(),
                };
                if active.queue.try_send(job).is_err() {
                    let dropped = self.push.dropped.fetch_add(1, Ordering::Relaxed) + 1;
                    if dropped.is_power_of_two() {
                        tracing::warn!(dropped, "push queue full; mention pushes dropped");
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: &str = "aa";
    const YOU: &str = "bb";

    fn pushes() -> Facts<'static> {
        Facts {
            recipient: ME,
            author: YOU,
            name: Some("ana"),
            bot: false,
            registered: true,
            sees_channel: true,
            content: "hey @ana, look",
            dnd: false,
            attended: false,
            recent: 0,
        }
    }

    #[test]
    fn each_rule_says_no_on_its_own_and_in_order() {
        type Change = fn(&mut Facts<'static>);
        let rows: [(&str, Change, Skip); 10] = [
            ("1 the author", |f| f.author = ME, Skip::Author),
            ("1 not a member", |f| f.name = None, Skip::NotMember),
            ("1 a bot", |f| f.bot = true, Skip::Bot),
            (
                "2 no registration",
                |f| f.registered = false,
                Skip::Unregistered,
            ),
            (
                "3 cannot see the channel",
                |f| f.sees_channel = false,
                Skip::Hidden,
            ),
            (
                "4 not mentioned",
                |f| f.content = "hey ana",
                Skip::NotMentioned,
            ),
            (
                "4 inside an address",
                |f| f.content = "mail x@ana",
                Skip::NotMentioned,
            ),
            ("5 do not disturb", |f| f.dnd = true, Skip::DoNotDisturb),
            (
                "6 someone is looking",
                |f| f.attended = true,
                Skip::Attended,
            ),
            (
                "7 over the rate",
                |f| f.recent = PUSHES_PER_MINUTE,
                Skip::RateLimited,
            ),
        ];
        assert_eq!(check(&pushes()), Ok(PushReason::Mention));
        assert_eq!(decide(&pushes()), Some(PushReason::Mention));
        for (rule, change, skip) in rows {
            let mut facts = pushes();
            change(&mut facts);
            assert_eq!(check(&facts), Err(skip), "rule {rule}");
            assert_eq!(decide(&facts), None, "rule {rule}");
        }
        let mut everything = pushes();
        everything.bot = true;
        everything.attended = true;
        everything.recent = PUSHES_PER_MINUTE;
        assert_eq!(check(&everything), Err(Skip::Bot), "the first rule wins");
        let mut nameless = pushes();
        nameless.name = Some("");
        nameless.content = "@ alone";
        assert_eq!(check(&nameless), Err(Skip::NotMentioned));
    }

    #[test]
    fn twenty_a_minute_per_recipient() {
        let push = Push::default();
        let start = Instant::now();
        for _ in 0..PUSHES_PER_MINUTE {
            assert!(push.take(ME, start));
        }
        assert!(!push.take(ME, start));
        assert_eq!(push.recent_count(ME, start), PUSHES_PER_MINUTE);
        assert!(push.take(YOU, start), "each recipient has their own");
        let later = start + RATE_WINDOW;
        assert_eq!(push.recent_count(ME, later), 0);
        assert!(push.take(ME, later));
        push.sweep_recent(later + RATE_WINDOW);
        assert!(push.recent_lock().is_empty());
    }

    fn payload(name: &str) -> MentionPayload {
        MentionPayload {
            v: 1,
            k: PushReason::Mention,
            guild: Id::from_u128(1),
            channel: Id::from_u128(2),
            message: Id::from_u128(3),
            author: "ab".repeat(32),
            author_name: name.into(),
            guild_name: name.into(),
            channel_name: name.into(),
            at: 1_700_000_000,
        }
    }

    fn open(body: &str, key: &[u8; 32]) -> MentionPayload {
        serde_json::from_str(&discordia_nostr::nip44::decrypt(key, body).unwrap()).unwrap()
    }

    #[test]
    fn a_payload_that_fits_is_sealed_whole() {
        let key = [7u8; 32];
        let body = seal(&payload("ana"), &key).unwrap();
        assert!(body.len() <= MAX_PUSH_BODY_BYTES);
        assert_eq!(open(&body, &key), payload("ana"));
    }

    #[test]
    fn long_names_are_cut_rather_than_the_push_dropped() {
        let key = [9u8; 32];
        for long in [
            "é".repeat(5_000),
            "\u{1}".repeat(2_000),
            "x".repeat(100_000),
        ] {
            let original = payload(&long);
            let body = seal(&original, &key).expect("still pushed");
            assert!(body.len() <= MAX_PUSH_BODY_BYTES, "{} bytes", body.len());
            let opened = open(&body, &key);
            for (cut, whole) in [
                (&opened.author_name, &original.author_name),
                (&opened.guild_name, &original.guild_name),
                (&opened.channel_name, &original.channel_name),
            ] {
                assert!(whole.starts_with(cut.as_str()) && cut.len() < whole.len());
            }
            assert_eq!(
                (opened.guild, opened.channel, opened.message, opened.at),
                (
                    original.guild,
                    original.channel,
                    original.message,
                    original.at
                )
            );
            assert_eq!(opened.author, original.author);
        }
    }

    #[test]
    fn only_a_relay_handle_is_a_handle() {
        let good = URL_SAFE_NO_PAD.encode([0xfbu8; HANDLE_BYTES]);
        assert_eq!(good.len(), HANDLE_LEN);
        assert!(valid_handle(&good));
        let mut refused = vec![
            String::new(),
            good[1..].to_string(),
            format!("{good}A"),
            STANDARD.encode([0xfbu8; HANDLE_BYTES]),
        ];
        for bad in ["/", "?", "#", "%", "..", ".", "+", "=", " "] {
            refused.push(format!("{bad}{}", &good[bad.len()..]));
            refused.push(format!("{}{bad}", &good[..HANDLE_LEN - bad.len()]));
        }
        for bad in refused {
            assert!(!valid_handle(&bad), "{bad:?}");
        }
    }

    #[test]
    fn a_key_is_exactly_32_bytes_of_base64() {
        let raw = [3u8; 32];
        for spelt in [
            STANDARD.encode(raw),
            STANDARD_NO_PAD.encode(raw),
            URL_SAFE.encode(raw),
            URL_SAFE_NO_PAD.encode(raw),
        ] {
            assert_eq!(decode_key(&spelt), Some(raw));
        }
        for bad in [
            STANDARD.encode([3u8; 31]),
            STANDARD.encode([3u8; 33]),
            "not base64!".into(),
            String::new(),
            "A".repeat(10_000),
        ] {
            assert_eq!(decode_key(&bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_relay_is_https_or_loopback_and_off_is_off() {
        for good in [
            "https://push.example",
            "https://push.example/",
            "https://push.example:8443/base",
            "http://127.0.0.1:7720",
            "http://localhost:7720",
            "http://[::1]:7720",
        ] {
            assert!(relay_base(good).is_ok(), "{good}");
        }
        assert_eq!(
            relay_base("https://push.example/").unwrap(),
            "https://push.example"
        );
        for bad in [
            "http://push.example",
            "http://10.0.0.1",
            "ftp://push.example",
            "https://user:pw@push.example",
            "https://push.example/?x=1",
            "https://push.example/#x",
            "push.example",
        ] {
            assert!(relay_base(bad).is_err(), "{bad}");
        }
        assert_eq!(
            PushConfig::from_setting(None).unwrap().url(),
            Some(DEFAULT_PUSH_RELAY)
        );
        for off in ["", "  ", "off", "OFF"] {
            assert_eq!(PushConfig::from_setting(Some(off)).unwrap().url(), None);
        }
        assert!(PushConfig::from_setting(Some("http://push.example")).is_err());
        assert_eq!(PushConfig::default().url(), None);
    }

    #[test]
    fn relay_answers_map_to_outcomes() {
        for (status, outcome) in [
            (202, Outcome::Accepted),
            (404, Outcome::Gone),
            (410, Outcome::Gone),
            (429, Outcome::Retry),
            (503, Outcome::Retry),
            (301, Outcome::Rejected),
            (400, Outcome::Rejected),
            (413, Outcome::Rejected),
        ] {
            assert_eq!(outcome_of(status), outcome, "{status}");
        }
    }
}
