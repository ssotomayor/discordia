//! Active host registrations, persistent name reservations, and pending proxy
//! pairings.
//!
//! Two layers, deliberately separate:
//! - **Live hosts** (`hosts`): keyed by slug/shortcode → a control channel.
//!   Ephemeral — a host is only here while its `/control` WebSocket is open,
//!   because that's the only way to reach it for a join.
//! - **Reservations** (`reservations`): keyed by slug → owner pubkey +
//!   metadata. **Persistent** (JSON-file backed) so a claimed name survives a
//!   rendezvous restart and stays owned while the host is briefly offline.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use axum::extract::ws::WebSocket;
use dashmap::DashMap;
use dioxusfun_protocol::rendezvous::{DiscoverEntry, RendezvousToHost};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, oneshot};

/// Each registered host has a sender for control messages (so we can notify
/// them of incoming friends), plus the public-browse metadata they opted
/// into at register time.
pub struct HostEntry {
    /// Display name (original case). For anonymous hosts this is the shortcode.
    pub name: Option<String>,
    pub description: Option<String>,
    pub public: bool,
    /// Gateway URL the host says the internet can dial directly, when it
    /// managed to obtain one. We store and hand it out verbatim: whether it
    /// actually works is settled by the joiner trying it, not by us probing it.
    pub endpoint: Option<String>,
    /// The host's QUIC transport key, and where to try reaching it. Only ever
    /// set when the registering key signed for it (see `relay.rs`), so anything
    /// here has been attested.
    pub transport_key: Option<String>,
    pub transport_addrs: Vec<String>,
    pub control_tx: tokio::sync::mpsc::UnboundedSender<RendezvousToHost>,
    /// Unix millis of the last frame we had from this host, refreshed by the
    /// control loop's heartbeat.
    ///
    /// A live WebSocket is *not* evidence that the peer still exists. When a
    /// host dies without closing the connection — laptop sleeps, Wi-Fi drops,
    /// the process is killed, a NAT drops the flow — the socket stays half-open
    /// and the read side simply never yields again. Without a clock the entry
    /// would sit in `hosts` forever, which is exactly why dead hosts kept
    /// appearing in the browse list.
    pub last_seen_ms: AtomicI64,
}

impl HostEntry {
    pub fn touch(&self) {
        self.last_seen_ms.store(now_ms(), Ordering::Relaxed);
    }

    /// Milliseconds since we last heard from this host. The deadline check
    /// works in millis so a sub-second timeout (tests) doesn't truncate to zero
    /// and unregister every host on its first heartbeat.
    pub fn idle_ms(&self) -> i64 {
        (now_ms() - self.last_seen_ms.load(Ordering::Relaxed)).max(0)
    }

    /// Seconds since we last heard from this host, for the browse listing.
    pub fn idle_secs(&self) -> u64 {
        (self.idle_ms() / 1000) as u64
    }
}

/// Milliseconds since the process started.
///
/// Monotonic on purpose: this measures "how long since we heard from the host",
/// and a wall clock that steps (NTP correction, the machine waking from sleep,
/// a manual change) would make a live host look long-dead or a dead one look
/// fresh.
fn now_ms() -> i64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_millis() as i64
}

/// A persisted claim on a name: who owns it, and nothing else.
///
/// **It used to carry `name`, `description` and `public` too, and nothing ever
/// read them.** A reservation answers exactly one question — may this key use
/// this name — and the display fields were answering a question nobody asked:
/// `discover()` lists only *live* hosts, on purpose ("you can't join a host
/// that's away"), and a host that comes back re-supplies all three in its own
/// `Register` frame before anything is shown. Persisting them meant they
/// survived a restart without ever being applied to anything.
///
/// Older `reservations.json` files still load: serde ignores fields the struct
/// no longer names, so the extra keys are dropped on the next write rather than
/// failing the boot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reservation {
    /// Lowercased; the map key and the join code.
    pub slug: String,
    pub owner_pubkey: String,
}

/// Why releasing a name was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum ReleaseError {
    /// No such reservation. Reported the same way as a wrong owner, on
    /// purpose: the two are indistinguishable to anyone who is not the owner,
    /// and telling them apart would turn this into an oracle for which names
    /// are claimed — a thing `/discover` deliberately does not answer for
    /// hosts that are offline.
    NotYours,
    /// A session is registered under that name right now. Releasing while a
    /// host is live would leave the session running under a code anyone could
    /// then claim, so the owner is asked to stop the host first.
    LiveNow,
}

/// Why a name claim was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum ClaimError {
    /// Reserved by a different pubkey.
    Taken,
    /// The owner's own name, but a live session already holds it right now.
    LiveElsewhere,
}

/// A friend is waiting; once the host opens the matching `/proxy/{session_id}`
/// connection, we hand it through `host_proxy_tx` to the friend handler.
pub struct PendingPairing {
    pub host_proxy_tx: oneshot::Sender<WebSocket>,
}

pub struct Registry {
    /// voice-token grant -> shortcode it authorises (see issue_voice_grant)
    voice_grants: DashMap<String, String>,
    /// slug/shortcode -> live registered host
    pub hosts: DashMap<String, Arc<HostEntry>>,
    /// slug -> persistent name reservation (survives restart)
    reservations: DashMap<String, Reservation>,
    /// session_id -> friend's pending pairing slot (waiting for the host's
    /// proxy WS to arrive)
    pub pending: DashMap<String, Mutex<Option<PendingPairing>>>,
    /// Where reservations are persisted. `None` = in-memory only (tests).
    store_path: Option<PathBuf>,
}

/// Canonicalize a claimed name into its slug (lowercased) and validate the
/// charset. Returns the slug or an error message suitable for the host.
pub fn validate_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() || name.len() > 64 {
        return Err("name must be 1–64 characters".into());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return Err("name may only contain letters, digits, '-', '_' and '.'".into());
    }
    Ok(name.to_lowercase())
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

impl Registry {
    /// In-memory registry (no persistence). Used by tests and anonymous-only
    /// deployments.
    pub fn new() -> Self {
        Self {
            hosts: DashMap::new(),
            voice_grants: DashMap::new(),
            reservations: DashMap::new(),
            pending: DashMap::new(),
            store_path: None,
        }
    }

    /// Open a registry backed by `path` (a JSON file), loading any existing
    /// reservations. Missing/unreadable file → start empty.
    pub fn load(path: PathBuf) -> Self {
        let reservations = DashMap::new();
        match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<Vec<Reservation>>(&text) {
                Ok(list) => {
                    for r in list {
                        reservations.insert(r.slug.clone(), r);
                    }
                    tracing::info!(count = reservations.len(), path = %path.display(), "reservations loaded");
                }
                Err(e) => {
                    tracing::warn!(error = %e, path = %path.display(), "reservations file unreadable — starting empty")
                }
            },
            Err(_) => {
                tracing::info!(path = %path.display(), "no reservations file yet — starting empty")
            }
        }
        Self {
            hosts: DashMap::new(),
            voice_grants: DashMap::new(),
            reservations,
            pending: DashMap::new(),
            store_path: Some(path),
        }
    }

    fn persist(&self) {
        let Some(path) = self.store_path.as_ref() else {
            return;
        };
        let list: Vec<Reservation> = self
            .reservations
            .iter()
            .map(|r| r.value().clone())
            .collect();
        match serde_json::to_string_pretty(&list) {
            Ok(json) => {
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                if let Err(e) = std::fs::write(path, json) {
                    tracing::error!(error = %e, path = %path.display(), "failed to persist reservations");
                }
            }
            Err(e) => tracing::error!(error = %e, "failed to serialize reservations"),
        }
    }

    /// Reserve (or refresh) a name for `owner`. Fails if a *different* pubkey
    /// already owns it, or if a live session currently holds it. On success the
    /// reservation is persisted.
    pub fn claim_name(&self, slug: &str, owner: &str) -> Result<(), ClaimError> {
        if let Some(existing) = self.reservations.get(slug)
            && existing.owner_pubkey != owner
        {
            return Err(ClaimError::Taken);
        }
        if self.hosts.contains_key(slug) {
            return Err(ClaimError::LiveElsewhere);
        }
        self.reservations.insert(
            slug.to_string(),
            Reservation {
                slug: slug.to_string(),
                owner_pubkey: owner.to_string(),
            },
        );
        self.persist();
        Ok(())
    }

    /// Give up a reserved name, if `owner` is the key that holds it.
    ///
    /// The inverse of `claim_name`, and the reason it exists: a reservation
    /// persists, so before this there was no way to undo one. A name claimed
    /// by mistake, or held by a key its owner has rotated away from, stayed
    /// claimed for the life of the relay's data directory.
    pub fn release_name(&self, slug: &str, owner: &str) -> Result<(), ReleaseError> {
        match self.reservations.get(slug) {
            Some(r) if r.owner_pubkey == owner => {}
            _ => return Err(ReleaseError::NotYours),
        }
        if self.hosts.contains_key(slug) {
            return Err(ReleaseError::LiveNow);
        }
        self.reservations.remove(slug);
        self.persist();
        Ok(())
    }

    /// The owner pubkey of a reserved name, if any.
    pub fn reservation_owner(&self, slug: &str) -> Option<String> {
        self.reservations.get(slug).map(|r| r.owner_pubkey.clone())
    }

    /// Issue a per-session grant that authorises `POST /voice-token` for this
    /// host only. Returned to the host in `Registered`; revoked on release.
    pub fn issue_voice_grant(&self, shortcode: &str) -> String {
        let grant = uuid::Uuid::new_v4().to_string();
        self.voice_grants
            .insert(grant.clone(), shortcode.to_string());
        grant
    }

    /// The shortcode a voice grant belongs to, if the grant is live.
    pub fn voice_grant_owner(&self, grant: &str) -> Option<String> {
        self.voice_grants.get(grant).map(|s| s.clone())
    }

    /// Try to claim a live slot for a shortcode. Returns the shared entry so the
    /// control loop can keep its heartbeat fresh, or `None` if already live.
    pub fn try_claim(&self, shortcode: &str, entry: HostEntry) -> Option<Arc<HostEntry>> {
        if self.hosts.contains_key(shortcode) {
            return None;
        }
        entry.touch();
        let entry = Arc::new(entry);
        self.hosts.insert(shortcode.to_string(), entry.clone());
        Some(entry)
    }

    /// Drop the LIVE registration for a shortcode. Never touches the persistent
    /// reservation — a named host going offline keeps its name.
    pub fn release(&self, shortcode: &str) {
        self.hosts.remove(shortcode);
        // A grant must not outlive the session that owns it.
        self.voice_grants.retain(|_, sc| sc != shortcode);
    }

    /// Register a new pending pairing slot; friend handler awaits the oneshot.
    /// Returns the receiver side; the proxy handler will fulfill it when the
    /// host's `/proxy/{session_id}` WS arrives. None if session_id collides
    /// (extremely unlikely with UUIDs).
    pub fn open_pairing(&self, session_id: &str) -> Option<oneshot::Receiver<WebSocket>> {
        let (tx, rx) = oneshot::channel();
        let slot = Mutex::new(Some(PendingPairing { host_proxy_tx: tx }));
        self.pending.insert(session_id.to_string(), slot);
        Some(rx)
    }

    /// Called by the proxy handler when the host's outbound WS arrives.
    /// Hands the socket to the waiting friend handler. Returns false if no
    /// friend was waiting or the slot expired.
    pub async fn fulfill_pairing(&self, session_id: &str, socket: WebSocket) -> bool {
        let Some(entry) = self.pending.remove(session_id) else {
            return false;
        };
        let mut guard = entry.1.lock().await;
        let Some(pairing) = guard.take() else {
            return false;
        };
        pairing.host_proxy_tx.send(socket).is_ok()
    }

    /// Expire a pending pairing after a timeout (cleanup if host never came).
    pub fn schedule_pairing_timeout(self: Arc<Self>, session_id: String, timeout: Duration) {
        tokio::spawn(async move {
            tokio::time::sleep(timeout).await;
            if self.pending.remove(&session_id).is_some() {
                tracing::warn!(%session_id, "pairing expired waiting for host");
            }
        });
    }

    /// Snapshot of all LIVE hosts that opted into public listing. Sorted by
    /// name (case-insensitive) with un-named hosts at the end. Offline reserved
    /// names are intentionally excluded — you can't join a host that's away.
    pub fn discover(&self) -> Vec<DiscoverEntry> {
        let mut entries: Vec<DiscoverEntry> = self
            .hosts
            .iter()
            .filter(|h| h.value().public)
            .map(|h| entry_for(h.key(), h.value()))
            .collect();
        entries.sort_by(|a, b| {
            let an = a.name.as_deref().unwrap_or("\u{FFFF}").to_lowercase();
            let bn = b.name.as_deref().unwrap_or("\u{FFFF}").to_lowercase();
            an.cmp(&bn).then_with(|| a.shortcode.cmp(&b.shortcode))
        });
        entries
    }

    /// One live host by its join code, public or not.
    ///
    /// Unlisted on purpose: `discover()` answers "what may I browse", this
    /// answers "I already have a code — how do I reach that host". A joiner
    /// needs the second to try the direct address before the relay, and an
    /// unlisted host is exactly the case where someone was handed a code.
    /// Knowing the code already buys a relayed connection, so this hands out no
    /// reachability that `/join/{code}` did not — only the host's address, which
    /// is why a host that publishes none stays relay-only here too.
    pub fn lookup(&self, code: &str) -> Option<DiscoverEntry> {
        self.hosts.get(code).map(|h| entry_for(h.key(), h.value()))
    }
}

fn entry_for(shortcode: &str, host: &HostEntry) -> DiscoverEntry {
    DiscoverEntry {
        shortcode: shortcode.to_string(),
        name: host.name.clone(),
        description: host.description.clone(),
        idle_secs: host.idle_secs(),
        endpoint: host.endpoint.clone(),
        transport_key: host.transport_key.clone(),
        transport_addrs: host.transport_addrs.clone(),
        // Filled in by the router, which knows the deployment's own URL; the
        // registry only knows about hosts.
        relay_url: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        std::env::temp_dir().join(format!("rzv-res-{}.json", uuid::Uuid::new_v4()))
    }

    #[test]
    fn validate_name_rules() {
        assert_eq!(validate_name("Acme").unwrap(), "acme");
        assert_eq!(validate_name("bitcoin-com").unwrap(), "bitcoin-com");
        assert!(validate_name("").is_err());
        assert!(validate_name("has space").is_err());
        assert!(validate_name("bad/slash").is_err());
    }

    #[test]
    fn claim_is_unique_and_owner_scoped() {
        let reg = Registry::new();
        assert!(reg.claim_name("acme", "owner_a").is_ok());
        // Same owner may re-claim (reconnect / metadata refresh).
        assert!(reg.claim_name("acme", "owner_a").is_ok());
        // A different owner cannot.
        assert_eq!(reg.claim_name("acme", "owner_b"), Err(ClaimError::Taken));
    }

    /// A file written before the display fields were dropped must still load.
    ///
    /// This matters more than it looks: `load` treats an unparseable file as
    /// "start empty" and only logs a warning, so an incompatible format change
    /// would silently un-claim every name on the relay — the owners would find
    /// their names free for anyone the next time they reconnected. Serde
    /// ignores unknown fields, and this is the test that says so.
    #[test]
    fn a_file_from_before_the_fields_were_dropped_still_loads() {
        let path = tmp();
        std::fs::write(
            &path,
            r#"[{"slug":"acme","name":"Acme","owner_pubkey":"owner_a",
                 "description":"a description nobody read","public":true}]"#,
        )
        .unwrap();

        let reg = Registry::load(path.clone());
        assert_eq!(
            reg.reservation_owner("acme").as_deref(),
            Some("owner_a"),
            "the one field that is read must survive the older format"
        );
        // And the claim still behaves: the owner keeps it, a stranger does not.
        assert_eq!(
            reg.claim_name("acme", "someone_else"),
            Err(ClaimError::Taken)
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn reservations_survive_reload() {
        let path = tmp();
        {
            let reg = Registry::load(path.clone());
            reg.claim_name("acme", "owner_a").unwrap();
        }
        // Fresh registry from the same file: the reservation and its owner
        // survived, so a squatter is still rejected but the owner reclaims.
        let reg2 = Registry::load(path.clone());
        assert_eq!(reg2.reservation_owner("acme").as_deref(), Some("owner_a"));
        assert_eq!(
            reg2.claim_name("acme", "someone_else"),
            Err(ClaimError::Taken)
        );
        assert!(reg2.claim_name("acme", "owner_a").is_ok());
        let _ = std::fs::remove_file(path);
    }
}
