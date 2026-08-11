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

/// A persisted claim on a name. The `slug` (lowercased) is the map key and the
/// join code; `name` preserves the owner's original casing for display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reservation {
    pub slug: String,
    pub name: String,
    pub owner_pubkey: String,
    pub description: Option<String>,
    pub public: bool,
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
    pub fn claim_name(
        &self,
        slug: &str,
        name: &str,
        owner: &str,
        description: Option<String>,
        public: bool,
    ) -> Result<(), ClaimError> {
        if let Some(existing) = self.reservations.get(slug) {
            if existing.owner_pubkey != owner {
                return Err(ClaimError::Taken);
            }
        }
        if self.hosts.contains_key(slug) {
            return Err(ClaimError::LiveElsewhere);
        }
        self.reservations.insert(
            slug.to_string(),
            Reservation {
                slug: slug.to_string(),
                name: name.to_string(),
                owner_pubkey: owner.to_string(),
                description,
                public,
            },
        );
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
            .map(|h| DiscoverEntry {
                shortcode: h.key().clone(),
                name: h.value().name.clone(),
                description: h.value().description.clone(),
                idle_secs: h.value().idle_secs(),
            })
            .collect();
        entries.sort_by(|a, b| {
            let an = a.name.as_deref().unwrap_or("\u{FFFF}").to_lowercase();
            let bn = b.name.as_deref().unwrap_or("\u{FFFF}").to_lowercase();
            an.cmp(&bn).then_with(|| a.shortcode.cmp(&b.shortcode))
        });
        entries
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
        assert!(
            reg.claim_name("acme", "Acme", "owner_a", None, true)
                .is_ok()
        );
        // Same owner may re-claim (reconnect / metadata refresh).
        assert!(
            reg.claim_name("acme", "Acme", "owner_a", None, false)
                .is_ok()
        );
        // A different owner cannot.
        assert_eq!(
            reg.claim_name("acme", "Acme", "owner_b", None, true),
            Err(ClaimError::Taken)
        );
    }

    #[test]
    fn reservations_survive_reload() {
        let path = tmp();
        {
            let reg = Registry::load(path.clone());
            reg.claim_name("acme", "Acme", "owner_a", Some("desc".into()), true)
                .unwrap();
        }
        // Fresh registry from the same file: the reservation and its owner
        // survived, so a squatter is still rejected but the owner reclaims.
        let reg2 = Registry::load(path.clone());
        assert_eq!(reg2.reservation_owner("acme").as_deref(), Some("owner_a"));
        assert_eq!(
            reg2.claim_name("acme", "Acme", "someone_else", None, true),
            Err(ClaimError::Taken)
        );
        assert!(
            reg2.claim_name("acme", "Acme", "owner_a", None, true)
                .is_ok()
        );
        let _ = std::fs::remove_file(path);
    }
}
