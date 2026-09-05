use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use dashmap::DashMap;
use dioxusfun_protocol::rendezvous::DiscoverEntry;
use serde::{Deserialize, Serialize};

use crate::limits::Limits;

pub struct HostEntry {
    pub name: Option<String>,
    pub description: Option<String>,
    pub public: bool,
    pub transport_key: Option<String>,
    pub transport_addrs: Vec<String>,
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

#[derive(Debug, Clone, Copy)]
pub struct ReservationCaps {
    pub per_owner: usize,
    pub total: usize,
}

impl Default for ReservationCaps {
    fn default() -> Self {
        Self {
            per_owner: 3,
            total: 10_000,
        }
    }
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
    OwnerLimit,
    Full,
}

pub struct Registry {
    voice_grants: DashMap<String, String>,
    pub hosts: DashMap<String, Arc<HostEntry>>,
    reservations: DashMap<String, Reservation>,
    store_path: Option<PathBuf>,
    persist_lock: std::sync::Mutex<()>,
    caps: ReservationCaps,
    /// Here and not on `AppCtx` because the server's tests build that by
    /// struct literal.
    pub limits: Limits,
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
        Self::with_store(DashMap::new(), None)
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
        Self::with_store(reservations, Some(path))
    }

    fn with_store(reservations: DashMap<String, Reservation>, store_path: Option<PathBuf>) -> Self {
        Self {
            hosts: DashMap::new(),
            voice_grants: DashMap::new(),
            reservations,
            store_path,
            persist_lock: std::sync::Mutex::new(()),
            caps: ReservationCaps::default(),
            limits: Limits::default(),
        }
    }

    pub fn with_caps(mut self, caps: ReservationCaps) -> Self {
        self.caps = caps;
        self
    }

    fn persist(&self) {
        let Some(path) = self.store_path.as_ref() else {
            return;
        };
        let _one_writer = self
            .persist_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let list: Vec<Reservation> = self
            .reservations
            .iter()
            .map(|r| r.value().clone())
            .collect();
        let json = match serde_json::to_string_pretty(&list) {
            Ok(json) => json,
            Err(e) => {
                tracing::error!(error = %e, "failed to serialize reservations");
                return;
            }
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = path.with_extension("json.tmp");
        let written = std::fs::File::create(&tmp).and_then(|mut f| {
            f.write_all(json.as_bytes())?;
            f.sync_all()?;
            std::fs::rename(&tmp, path)
        });
        if let Err(e) = written {
            tracing::error!(error = %e, path = %path.display(), "failed to persist reservations");
            let _ = std::fs::remove_file(&tmp);
        }
    }

    pub fn claim_name(&self, slug: &str, owner: &str) -> Result<(), ClaimError> {
        let already_mine = self.reservations.get(slug).map(|r| r.owner_pubkey == owner);
        if already_mine == Some(false) {
            return Err(ClaimError::Taken);
        }
        if self.hosts.contains_key(slug) {
            return Err(ClaimError::LiveElsewhere);
        }
        if already_mine == Some(true) {
            return Ok(());
        }
        if self.reservations.len() >= self.caps.total {
            return Err(ClaimError::Full);
        }
        let owned = self
            .reservations
            .iter()
            .filter(|r| r.owner_pubkey == owner)
            .count();
        if owned >= self.caps.per_owner {
            return Err(ClaimError::OwnerLimit);
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

    /// Answers unlisted hosts too, unlike `discover`: holding a code is what
    /// buys a connection, so this hands out no new reachability.
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
    fn an_owner_is_capped_and_a_release_frees_a_slot() {
        let reg = Registry::new();
        for slug in ["one", "two", "three"] {
            assert!(reg.claim_name(slug, "owner_a").is_ok());
        }
        assert_eq!(
            reg.claim_name("four", "owner_a"),
            Err(ClaimError::OwnerLimit)
        );
        assert!(
            reg.claim_name("two", "owner_a").is_ok(),
            "re-claiming a held name is not a new reservation"
        );
        assert!(reg.claim_name("four", "owner_b").is_ok());
        assert!(reg.release_name("one", "owner_a").is_ok());
        assert!(
            reg.claim_name("four", "owner_a").is_err(),
            "owner_b holds it"
        );
        assert!(reg.claim_name("five", "owner_a").is_ok());
    }

    #[test]
    fn the_table_has_a_ceiling() {
        let reg = Registry::new().with_caps(ReservationCaps {
            per_owner: 10,
            total: 5,
        });
        for i in 0..5 {
            assert!(reg.claim_name(&format!("n{i}"), &format!("o{i}")).is_ok());
        }
        assert_eq!(reg.claim_name("n5", "o5"), Err(ClaimError::Full));
        assert!(
            reg.claim_name("n0", "o0").is_ok(),
            "a held name still answers"
        );
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

    #[test]
    fn persist_leaves_no_temp_file_and_a_whole_file() {
        let path = tmp();
        let tmp_path = path.with_extension("json.tmp");
        let reg = Registry::load(path.clone());
        reg.claim_name("acme", "owner_a").unwrap();
        reg.claim_name("beta", "owner_b").unwrap();
        assert!(!tmp_path.exists(), "temp file left behind");
        let text = std::fs::read_to_string(&path).unwrap();
        let list: Vec<Reservation> = serde_json::from_str(&text).unwrap();
        assert_eq!(list.len(), 2);
        let _ = std::fs::remove_file(path);
    }
}
