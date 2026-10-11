use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use dashmap::{DashMap, mapref::entry::Entry};
use dioxusfun_protocol::rendezvous::{
    DiscoverEntry, GeoPoint, MAX_NAME_CHARS, ReservedName, join_code,
};
use serde::{Deserialize, Serialize};

use crate::limits::Limits;

pub struct HostEntry {
    pub name: Option<String>,
    pub description: Option<String>,
    pub public: bool,
    pub transport_key: Option<String>,
    pub transport_addrs: Vec<String>,
    pub location: Option<GeoPoint>,
    pub last_seen_ms: AtomicI64,
    /// Woken when the same key registers this name from a newer session.
    pub superseded: tokio::sync::Notify,
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
    /// Wall clock, unlike `last_seen_ms`, because it must survive a restart.
    /// A file without it counts from when it was loaded rather than expiring at once.
    #[serde(default = "now_unix")]
    pub last_live_unix: u64,
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

#[derive(Debug, Clone, Copy)]
pub struct ReservationCaps {
    pub per_owner: usize,
    pub total: usize,
    /// How long a name outlives its last session.
    pub ttl_secs: u64,
}

impl Default for ReservationCaps {
    fn default() -> Self {
        Self {
            per_owner: 3,
            total: 10_000,
            ttl_secs: 24 * 60 * 60,
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
    // Ownership, quotas and live-session changes must not race with persistence.
    registration_lock: std::sync::Mutex<()>,
    caps: ReservationCaps,
    /// Here and not on `AppCtx` because the server's tests build that by
    /// struct literal.
    pub limits: Limits,
}

/// Returns the join code `name` claims; the name itself stays as written,
/// so a host can be "Friday Game Night" and still be typed.
pub fn validate_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME_CHARS {
        return Err(format!(
            "a server name must be 1–{MAX_NAME_CHARS} characters"
        ));
    }
    join_code(name).ok_or_else(|| "a server name needs a letter or digit from a–z or 0–9".into())
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
            registration_lock: std::sync::Mutex::new(()),
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
        self.claim_name_at(slug, owner, now_unix())
    }

    fn claim_name_at(&self, slug: &str, owner: &str, now: u64) -> Result<(), ClaimError> {
        let _registration = self
            .registration_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.claim_name_locked(slug, owner, now)
    }

    /// The owner's own live session is no obstacle: `claim_host` replaces it,
    /// which is how a host back from a dropped link skips the heartbeat timeout.
    fn claim_name_locked(&self, slug: &str, owner: &str, now: u64) -> Result<(), ClaimError> {
        self.expire_locked(now);
        let already_mine = self
            .reservations
            .get(slug)
            .map(|r| r.owner_pubkey.eq_ignore_ascii_case(owner));
        if already_mine == Some(false) {
            return Err(ClaimError::Taken);
        }
        if already_mine == Some(true) {
            if let Some(mut r) = self.reservations.get_mut(slug) {
                r.last_live_unix = now;
            }
            self.persist();
            return Ok(());
        }
        if self.hosts.contains_key(slug) {
            return Err(ClaimError::LiveElsewhere);
        }
        if self.reservations.len() >= self.caps.total {
            return Err(ClaimError::Full);
        }
        let owned = self
            .reservations
            .iter()
            .filter(|r| r.owner_pubkey.eq_ignore_ascii_case(owner))
            .count();
        if owned >= self.caps.per_owner {
            return Err(ClaimError::OwnerLimit);
        }
        self.reservations.insert(
            slug.to_string(),
            Reservation {
                slug: slug.to_string(),
                owner_pubkey: owner.to_ascii_lowercase(),
                last_live_unix: now,
            },
        );
        self.persist();
        Ok(())
    }

    /// A live name never expires, whatever its timestamp says: that is only
    /// refreshed when a session starts or ends, and by `sweep`.
    fn expire_locked(&self, now: u64) -> bool {
        let before = self.reservations.len();
        let ttl = self.caps.ttl_secs;
        self.reservations.retain(|slug, r| {
            self.hosts.contains_key(slug) || now.saturating_sub(r.last_live_unix) < ttl
        });
        let expired = self.reservations.len() != before;
        if expired {
            self.persist();
        }
        expired
    }

    /// Run periodically. Refreshing live names keeps a restart of this process,
    /// which forgets every session, from expiring a name its host is about to reclaim.
    pub fn sweep(&self, now: u64) {
        let _registration = self
            .registration_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut refreshed = false;
        for mut r in self.reservations.iter_mut() {
            if self.hosts.contains_key(r.key()) {
                r.last_live_unix = now;
                refreshed = true;
            }
        }
        if !self.expire_locked(now) && refreshed {
            self.persist();
        }
    }

    pub fn names_of(&self, owner: &str, now: u64) -> Vec<ReservedName> {
        let ttl = self.caps.ttl_secs;
        let mut names: Vec<ReservedName> = self
            .reservations
            .iter()
            .filter(|r| r.owner_pubkey.eq_ignore_ascii_case(owner))
            .filter_map(|r| {
                if self.hosts.contains_key(r.key()) {
                    return Some(ReservedName {
                        code: r.slug.clone(),
                        expires_in_secs: None,
                    });
                }
                let left = ttl.checked_sub(now.saturating_sub(r.last_live_unix))?;
                (left > 0).then(|| ReservedName {
                    code: r.slug.clone(),
                    expires_in_secs: Some(left),
                })
            })
            .collect();
        names.sort_by(|a, b| a.code.cmp(&b.code));
        names
    }

    pub fn release_name(&self, slug: &str, owner: &str) -> Result<(), ReleaseError> {
        let _registration = self
            .registration_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match self.reservations.get(slug) {
            Some(r) if r.owner_pubkey.eq_ignore_ascii_case(owner) => {}
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

    pub fn claim_host(
        &self,
        shortcode: &str,
        owner: Option<&str>,
        entry: HostEntry,
    ) -> Result<Arc<HostEntry>, ClaimError> {
        let _registration = self
            .registration_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(owner) = owner {
            self.claim_name_locked(shortcode, owner, now_unix())?;
        } else if self.reservations.contains_key(shortcode) {
            return Err(ClaimError::Taken);
        }
        entry.touch();
        let entry = Arc::new(entry);
        match self.hosts.entry(shortcode.to_string()) {
            Entry::Vacant(slot) => {
                slot.insert(entry.clone());
                Ok(entry)
            }
            // Only an owner gets here: `claim_name_locked` refused everyone else.
            Entry::Occupied(mut slot) if owner.is_some() => {
                let old = slot.insert(entry.clone());
                old.superseded.notify_one();
                Ok(entry)
            }
            Entry::Occupied(_) => Err(ClaimError::LiveElsewhere),
        }
    }

    /// Ends one session. A superseded session ends after its successor took
    /// the name, so it removes the listing only if the listing is still its own.
    pub fn release(&self, shortcode: &str, entry: &Arc<HostEntry>, grant: Option<&str>) {
        let _registration = self
            .registration_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(grant) = grant {
            self.voice_grants.remove(grant);
        }
        if self
            .hosts
            .remove_if(shortcode, |_, live| Arc::ptr_eq(live, entry))
            .is_some()
            && let Some(mut r) = self.reservations.get_mut(shortcode)
        {
            r.last_live_unix = now_unix();
            drop(r);
            self.persist();
        }
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
        location: host.location,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host_entry() -> HostEntry {
        HostEntry {
            name: Some("test".to_string()),
            description: None,
            public: false,
            transport_key: None,
            transport_addrs: Vec::new(),
            location: None,
            last_seen_ms: AtomicI64::new(0),
            superseded: Default::default(),
        }
    }

    #[test]
    fn simultaneous_registrations_keep_one_owner_and_one_session() {
        let registry = Registry::new();
        let barrier = std::sync::Barrier::new(16);
        let outcomes = std::thread::scope(|scope| {
            let jobs: Vec<_> = (0..16)
                .map(|index| {
                    let registry = &registry;
                    let barrier = &barrier;
                    scope.spawn(move || {
                        let owner = format!("{index:064x}");
                        barrier.wait();
                        let result = registry.claim_host("same-name", Some(&owner), host_entry());
                        (owner, result)
                    })
                })
                .collect();
            jobs.into_iter()
                .map(|job| job.join().unwrap())
                .collect::<Vec<_>>()
        });
        let winners: Vec<_> = outcomes
            .iter()
            .filter(|(_, result)| result.is_ok())
            .collect();
        assert_eq!(winners.len(), 1);
        let (owner, result) = winners[0];
        assert_eq!(
            registry.reservation_owner("same-name").as_deref(),
            Some(owner.as_str())
        );
        assert!(Arc::ptr_eq(
            registry.hosts.get("same-name").unwrap().value(),
            result.as_ref().unwrap()
        ));
        assert_eq!(registry.hosts.len(), 1);
        for (_, result) in outcomes.iter().filter(|(_, result)| result.is_err()) {
            assert!(matches!(result, Err(ClaimError::Taken)));
        }
    }

    #[test]
    fn concurrent_claims_cannot_exceed_the_owner_quota() {
        let registry = Registry::new().with_caps(ReservationCaps {
            per_owner: 3,
            total: 100,
            ..Default::default()
        });
        let barrier = std::sync::Barrier::new(16);
        let successes = std::thread::scope(|scope| {
            let jobs: Vec<_> = (0..16)
                .map(|index| {
                    let registry = &registry;
                    let barrier = &barrier;
                    scope.spawn(move || {
                        barrier.wait();
                        registry
                            .claim_host(&format!("name-{index}"), Some("owner"), host_entry())
                            .is_ok()
                    })
                })
                .collect();
            jobs.into_iter()
                .map(|job| job.join().unwrap())
                .filter(|success| *success)
                .count()
        });
        assert_eq!(successes, 3);
        assert_eq!(registry.reservations.len(), 3);
    }

    #[test]
    fn stored_uppercase_keys_reclaim_and_release_as_the_same_identity() {
        let path = tmp();
        let uppercase = "AB".repeat(32);
        let lowercase = uppercase.to_ascii_lowercase();
        std::fs::write(
            &path,
            serde_json::to_vec(&vec![Reservation {
                slug: "legacy".to_string(),
                owner_pubkey: uppercase,
                last_live_unix: now_unix(),
            }])
            .unwrap(),
        )
        .unwrap();
        let registry = Registry::load(path.clone()).with_caps(ReservationCaps {
            per_owner: 1,
            total: 100,
            ..Default::default()
        });
        assert!(registry.claim_name("legacy", &lowercase).is_ok());
        assert_eq!(
            registry.claim_name("second", &lowercase),
            Err(ClaimError::OwnerLimit)
        );
        assert!(registry.release_name("legacy", &lowercase).is_ok());
        assert!(registry.claim_name("second", &lowercase).is_ok());
        std::fs::remove_file(path).unwrap();
    }

    fn tmp() -> PathBuf {
        std::env::temp_dir().join(format!("rzv-res-{}.json", uuid::Uuid::new_v4()))
    }

    #[test]
    fn validate_name_rules() {
        assert_eq!(validate_name("Acme").unwrap(), "acme");
        assert_eq!(validate_name("bitcoin-com").unwrap(), "bitcoin-com");
        assert_eq!(validate_name("Has Space").unwrap(), "has-space");
        assert_eq!(validate_name("bad/slash").unwrap(), "bad-slash");
        assert!(validate_name("").is_err());
        assert!(validate_name("?!").is_err());
        assert!(validate_name(&"a".repeat(MAX_NAME_CHARS + 1)).is_err());
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
            ..Default::default()
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

    #[test]
    fn a_name_expires_a_day_after_its_last_session_and_not_while_live() {
        let reg = Registry::new();
        let day = ReservationCaps::default().ttl_secs;
        reg.claim_name_at("idle", "owner_a", 1_000).unwrap();
        reg.claim_name_at("busy", "owner_a", 1_000).unwrap();
        reg.hosts.insert("busy".into(), Arc::new(host_entry()));

        reg.sweep(1_000 + day - 1);
        assert!(reg.reservation_owner("idle").is_some());
        reg.sweep(1_000 + day + 1);
        assert_eq!(reg.reservation_owner("idle"), None);
        assert!(
            reg.reservation_owner("busy").is_some(),
            "a live name never expires"
        );
        assert_eq!(
            reg.names_of("owner_a", 1_000 + day + 1),
            vec![ReservedName {
                code: "busy".into(),
                expires_in_secs: None
            }]
        );
    }

    #[test]
    fn an_expired_name_frees_its_owners_slot_and_can_be_claimed_by_anyone() {
        let reg = Registry::new().with_caps(ReservationCaps {
            per_owner: 1,
            ..Default::default()
        });
        let day = ReservationCaps::default().ttl_secs;
        reg.claim_name_at("old", "owner_a", 0).unwrap();
        assert_eq!(
            reg.claim_name_at("new", "owner_a", day - 1),
            Err(ClaimError::OwnerLimit)
        );
        assert!(reg.claim_name_at("new", "owner_a", day).is_ok());
        assert!(reg.claim_name_at("old", "owner_b", day).is_ok());
    }

    #[test]
    fn a_superseded_session_leaves_its_successor_listed() {
        let reg = Registry::new();
        let first = reg
            .claim_host("mine", Some("owner_a"), host_entry())
            .unwrap();
        let second = reg
            .claim_host("mine", Some("owner_a"), host_entry())
            .unwrap();
        reg.release("mine", &first, None);
        assert!(Arc::ptr_eq(reg.hosts.get("mine").unwrap().value(), &second));
        reg.release("mine", &second, None);
        assert!(!reg.hosts.contains_key("mine"));
    }
}
