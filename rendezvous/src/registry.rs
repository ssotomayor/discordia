use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use axum::extract::ws::WebSocket;
use dashmap::DashMap;
use dioxusfun_protocol::rendezvous::{DiscoverEntry, RendezvousToHost};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, oneshot};

pub struct HostEntry {
    pub name: Option<String>,
    pub description: Option<String>,
    pub public: bool,
    pub endpoint: Option<String>,
    pub transport_key: Option<String>,
    pub transport_addrs: Vec<String>,
    pub control_tx: tokio::sync::mpsc::UnboundedSender<RendezvousToHost>,
    pub last_seen_ms: AtomicI64,
}

impl HostEntry {
    pub fn touch(&self) {
        self.last_seen_ms.store(now_ms(), Ordering::Relaxed);
    }

    pub fn idle_ms(&self) -> i64 {
        (now_ms() - self.last_seen_ms.load(Ordering::Relaxed)).max(0)
    }

    /// Monotonic on purpose: this measures silence, and a wall clock that
    /// jumps backwards would resurrect a dead host.
    pub fn idle_secs(&self) -> u64 {
        (self.idle_ms() / 1000) as u64
    }
}

fn now_ms() -> i64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_millis() as i64
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reservation {
    pub slug: String,
    pub owner_pubkey: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ReleaseError {
    NotYours,
    LiveNow,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ClaimError {
    Taken,
    LiveElsewhere,
}

pub struct PendingPairing {
    pub host_proxy_tx: oneshot::Sender<WebSocket>,
}

pub struct Registry {
    voice_grants: DashMap<String, String>,
    pub hosts: DashMap<String, Arc<HostEntry>>,
    reservations: DashMap<String, Reservation>,
    pub pending: DashMap<String, Mutex<Option<PendingPairing>>>,
    store_path: Option<PathBuf>,
}

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
    pub fn new() -> Self {
        Self {
            hosts: DashMap::new(),
            voice_grants: DashMap::new(),
            reservations: DashMap::new(),
            pending: DashMap::new(),
            store_path: None,
        }
    }

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

    pub fn reservation_owner(&self, slug: &str) -> Option<String> {
        self.reservations.get(slug).map(|r| r.owner_pubkey.clone())
    }

    pub fn issue_voice_grant(&self, shortcode: &str) -> String {
        let grant = uuid::Uuid::new_v4().to_string();
        self.voice_grants
            .insert(grant.clone(), shortcode.to_string());
        grant
    }

    pub fn voice_grant_owner(&self, grant: &str) -> Option<String> {
        self.voice_grants.get(grant).map(|s| s.clone())
    }

    pub fn try_claim(&self, shortcode: &str, entry: HostEntry) -> Option<Arc<HostEntry>> {
        if self.hosts.contains_key(shortcode) {
            return None;
        }
        entry.touch();
        let entry = Arc::new(entry);
        self.hosts.insert(shortcode.to_string(), entry.clone());
        Some(entry)
    }

    pub fn release(&self, shortcode: &str) {
        self.hosts.remove(shortcode);
        self.voice_grants.retain(|_, sc| sc != shortcode);
    }

    pub fn open_pairing(&self, session_id: &str) -> Option<oneshot::Receiver<WebSocket>> {
        let (tx, rx) = oneshot::channel();
        let slot = Mutex::new(Some(PendingPairing { host_proxy_tx: tx }));
        self.pending.insert(session_id.to_string(), slot);
        Some(rx)
    }

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

    pub fn schedule_pairing_timeout(self: Arc<Self>, session_id: String, timeout: Duration) {
        tokio::spawn(async move {
            tokio::time::sleep(timeout).await;
            if self.pending.remove(&session_id).is_some() {
                tracing::warn!(%session_id, "pairing expired waiting for host");
            }
        });
    }

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

    /// Answers unlisted hosts too, unlike `discover`: holding a code already
    /// buys a relayed connection, so this hands out no new reachability.
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
        assert!(reg.claim_name("acme", "owner_a").is_ok());
        assert_eq!(reg.claim_name("acme", "owner_b"), Err(ClaimError::Taken));
    }

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
