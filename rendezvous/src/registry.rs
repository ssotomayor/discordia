use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use dashmap::{DashMap, mapref::entry::Entry};
use discordia_protocol::rendezvous::{
    DiscoverEntry, GeoPoint, MAX_NAME_CHARS, ReservedName, join_code,
};
use rusqlite::{Connection, OptionalExtension, params};

use crate::limits::Limits;

pub struct HostEntry {
    pub name: Option<String>,
    pub description: Option<String>,
    pub public: bool,
    pub transport_key: Option<String>,
    pub transport_addrs: Vec<String>,
    pub location: Option<GeoPoint>,
    /// Who registered it, as `origin` spells them: what the per-address caps count.
    pub address: Option<String>,
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

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// Keys are free, so a cap per key bounds a key and not a requester: names
/// and live hosts are also counted per address, which is not free.
#[derive(Debug, Clone, Copy)]
pub struct ReservationCaps {
    pub per_owner: usize,
    pub per_address: usize,
    pub total: usize,
    /// How long a name outlives its last session.
    pub ttl_secs: u64,
    pub hosts_per_address: usize,
    pub hosts_total: usize,
}

impl Default for ReservationCaps {
    fn default() -> Self {
        Self {
            per_owner: 3,
            per_address: 16,
            total: 10_000,
            ttl_secs: 24 * 60 * 60,
            hosts_per_address: 16,
            hosts_total: 10_000,
        }
    }
}

/// An IPv6 host is handed a /64, so one address in it is not one requester.
pub fn origin(ip: std::net::IpAddr) -> String {
    use std::net::IpAddr;
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => v4.to_string(),
            None => {
                let mut bits = v6.octets();
                bits[8..].fill(0);
                format!("{}/64", std::net::Ipv6Addr::from(bits))
            }
        },
        v4 => v4.to_string(),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ReleaseError {
    NotYours,
    LiveNow,
    Storage,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ClaimError {
    Taken,
    LiveElsewhere,
    OwnerLimit,
    AddressLimit,
    Full,
    Storage,
}

pub struct Registry {
    voice_grants: DashMap<String, String>,
    pub hosts: DashMap<String, Arc<HostEntry>>,
    /// The only copy of the reservations, so an operator's `sqlite3` edit
    /// takes effect without a restart.
    reservations: Mutex<Connection>,
    // Ownership, quotas and live-session changes must not race with each other.
    registration_lock: Mutex<()>,
    caps: ReservationCaps,
    /// Here and not on `AppCtx` because the server's tests build that by
    /// struct literal.
    pub limits: Limits,
}

const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS reservations (
        slug TEXT PRIMARY KEY,
        owner_pubkey TEXT NOT NULL COLLATE NOCASE,
        last_live_unix INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS reservations_by_owner ON reservations (owner_pubkey);
";

/// Rows from before addresses were recorded count against none.
fn migrate(db: &Connection) -> rusqlite::Result<()> {
    let has_address = db
        .prepare("SELECT 1 FROM pragma_table_info('reservations') WHERE name = 'address'")?
        .exists([])?;
    if !has_address {
        db.execute_batch("ALTER TABLE reservations ADD COLUMN address TEXT")?;
    }
    db.execute_batch("CREATE INDEX IF NOT EXISTS reservations_by_address ON reservations (address)")
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

fn storage<E>(error: rusqlite::Error, fallback: E) -> E {
    tracing::error!(%error, "reservations store failed");
    fallback
}

fn holder(db: &Connection, slug: &str) -> rusqlite::Result<Option<String>> {
    db.prepare_cached("SELECT owner_pubkey FROM reservations WHERE slug = ?1")?
        .query_row([slug], |row| row.get(0))
        .optional()
}

fn set_live(db: &Connection, slug: &str, now: u64) -> rusqlite::Result<usize> {
    db.prepare_cached("UPDATE reservations SET last_live_unix = ?2 WHERE slug = ?1")?
        .execute(params![slug, now as i64])
}

impl Registry {
    /// In memory: for tests and anything that need not survive a restart.
    pub fn new() -> Self {
        let db = Connection::open_in_memory().expect("in-memory sqlite");
        db.execute_batch(SCHEMA).expect("reservations schema");
        migrate(&db).expect("reservations schema");
        Self::with_db(db)
    }

    pub fn open(path: &Path) -> Result<Self, String> {
        let fail = |e: rusqlite::Error| format!("reservations store {}: {e}", path.display());
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        }
        let db = Connection::open(path).map_err(fail)?;
        // An operator's `sqlite3` may hold the write lock for a moment.
        db.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(fail)?;
        db.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))
            .map_err(fail)?;
        db.execute_batch(SCHEMA).map_err(fail)?;
        migrate(&db).map_err(fail)?;
        let count: i64 = db
            .query_row("SELECT COUNT(*) FROM reservations", [], |row| row.get(0))
            .map_err(fail)?;
        tracing::info!(count, path = %path.display(), "reservations loaded");
        Ok(Self::with_db(db))
    }

    fn with_db(db: Connection) -> Self {
        Self {
            hosts: DashMap::new(),
            voice_grants: DashMap::new(),
            reservations: Mutex::new(db),
            registration_lock: Mutex::new(()),
            caps: ReservationCaps::default(),
            limits: Limits::default(),
        }
    }

    pub fn with_caps(mut self, caps: ReservationCaps) -> Self {
        self.caps = caps;
        self
    }

    fn db(&self) -> MutexGuard<'_, Connection> {
        self.reservations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn registration(&self) -> MutexGuard<'_, ()> {
        self.registration_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn claim_name(&self, slug: &str, owner: &str) -> Result<(), ClaimError> {
        self.claim_name_at(slug, owner, None, now_unix())
    }

    fn claim_name_at(
        &self,
        slug: &str,
        owner: &str,
        address: Option<&str>,
        now: u64,
    ) -> Result<(), ClaimError> {
        let _registration = self.registration();
        self.claim_name_locked(slug, owner, address, now)
    }

    /// The owner's own live session is no obstacle: `claim_host` replaces it,
    /// which is how a host back from a dropped link skips the heartbeat timeout.
    fn claim_name_locked(
        &self,
        slug: &str,
        owner: &str,
        address: Option<&str>,
        now: u64,
    ) -> Result<(), ClaimError> {
        let db = self.db();
        self.claim_in(&db, slug, owner, address, now)
            .unwrap_or_else(|e| Err(storage(e, ClaimError::Storage)))
    }

    /// A name its owner already holds is kept whatever the caps now say, so a
    /// host keeps its name across restarts and address changes.
    fn claim_in(
        &self,
        db: &Connection,
        slug: &str,
        owner: &str,
        address: Option<&str>,
        now: u64,
    ) -> rusqlite::Result<Result<(), ClaimError>> {
        self.expire(db, now)?;
        match holder(db, slug)? {
            Some(held) if !held.eq_ignore_ascii_case(owner) => return Ok(Err(ClaimError::Taken)),
            Some(_) => {
                set_live(db, slug, now)?;
                if let Some(address) = address {
                    db.prepare_cached("UPDATE reservations SET address = ?2 WHERE slug = ?1")?
                        .execute(params![slug, address])?;
                }
                return Ok(Ok(()));
            }
            None => {}
        }
        if self.hosts.contains_key(slug) {
            return Ok(Err(ClaimError::LiveElsewhere));
        }
        let total: i64 = db.query_row("SELECT COUNT(*) FROM reservations", [], |row| row.get(0))?;
        if total as usize >= self.caps.total {
            return Ok(Err(ClaimError::Full));
        }
        let owned: i64 = db.query_row(
            "SELECT COUNT(*) FROM reservations WHERE owner_pubkey = ?1",
            [owner],
            |row| row.get(0),
        )?;
        if owned as usize >= self.caps.per_owner {
            return Ok(Err(ClaimError::OwnerLimit));
        }
        if let Some(address) = address {
            let from_here: i64 = db.query_row(
                "SELECT COUNT(*) FROM reservations WHERE address = ?1",
                [address],
                |row| row.get(0),
            )?;
            if from_here as usize >= self.caps.per_address {
                return Ok(Err(ClaimError::AddressLimit));
            }
        }
        db.execute(
            "INSERT INTO reservations (slug, owner_pubkey, last_live_unix, address) VALUES (?1, ?2, ?3, ?4)",
            params![slug, owner.to_ascii_lowercase(), now as i64, address],
        )?;
        Ok(Ok(()))
    }

    /// A live name never expires, whatever its timestamp says: that is only
    /// refreshed when a session starts or ends, and by `sweep`.
    fn expire(&self, db: &Connection, now: u64) -> rusqlite::Result<()> {
        let Some(cutoff) = now.checked_sub(self.caps.ttl_secs) else {
            return Ok(());
        };
        let stale: Vec<String> = db
            .prepare_cached("SELECT slug FROM reservations WHERE last_live_unix <= ?1")?
            .query_map([cutoff as i64], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for slug in stale.iter().filter(|slug| !self.hosts.contains_key(*slug)) {
            db.execute("DELETE FROM reservations WHERE slug = ?1", [slug])?;
        }
        Ok(())
    }

    /// Run periodically. Refreshing live names keeps a restart of this process,
    /// which forgets every session, from expiring a name its host is about to reclaim.
    pub fn sweep(&self, now: u64) {
        let _registration = self.registration();
        let live: Vec<String> = self.hosts.iter().map(|h| h.key().clone()).collect();
        let db = self.db();
        let swept = db.unchecked_transaction().and_then(|tx| {
            for slug in &live {
                set_live(&tx, slug, now)?;
            }
            self.expire(&tx, now)?;
            tx.commit()
        });
        if let Err(e) = swept {
            storage(e, ());
        }
    }

    pub fn names_of(&self, owner: &str, now: u64) -> Vec<ReservedName> {
        let ttl = self.caps.ttl_secs;
        let held: rusqlite::Result<Vec<(String, i64)>> = self
            .db()
            .prepare_cached(
                "SELECT slug, last_live_unix FROM reservations WHERE owner_pubkey = ?1 ORDER BY slug",
            )
            .and_then(|mut query| {
                query
                    .query_map([owner], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect()
            });
        let held = held.unwrap_or_else(|e| storage(e, Vec::new()));
        held.into_iter()
            .filter_map(|(slug, last_live)| {
                if self.hosts.contains_key(&slug) {
                    return Some(ReservedName {
                        code: slug,
                        expires_in_secs: None,
                    });
                }
                let left = ttl.checked_sub(now.saturating_sub(last_live as u64))?;
                (left > 0).then_some(ReservedName {
                    code: slug,
                    expires_in_secs: Some(left),
                })
            })
            .collect()
    }

    pub fn release_name(&self, slug: &str, owner: &str) -> Result<(), ReleaseError> {
        let _registration = self.registration();
        let db = self.db();
        match holder(&db, slug) {
            Ok(Some(held)) if held.eq_ignore_ascii_case(owner) => {}
            Ok(_) => return Err(ReleaseError::NotYours),
            Err(e) => return Err(storage(e, ReleaseError::Storage)),
        }
        if self.hosts.contains_key(slug) {
            return Err(ReleaseError::LiveNow);
        }
        db.execute("DELETE FROM reservations WHERE slug = ?1", [slug])
            .map(|_| ())
            .map_err(|e| storage(e, ReleaseError::Storage))
    }

    pub fn reservation_owner(&self, slug: &str) -> Option<String> {
        holder(&self.db(), slug).unwrap_or_else(|e| storage(e, None))
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
        let _registration = self.registration();
        // A session replacing its own listing is not one more host.
        let replacing = owner.is_some() && self.hosts.contains_key(shortcode);
        if !replacing {
            if self.hosts.len() >= self.caps.hosts_total {
                return Err(ClaimError::Full);
            }
            let from_here = |address: &String| {
                self.hosts
                    .iter()
                    .filter(|h| h.value().address.as_ref() == Some(address))
                    .count()
            };
            if entry
                .address
                .as_ref()
                .is_some_and(|a| from_here(a) >= self.caps.hosts_per_address)
            {
                return Err(ClaimError::AddressLimit);
            }
        }
        if let Some(owner) = owner {
            self.claim_name_locked(shortcode, owner, entry.address.as_deref(), now_unix())?;
        } else if holder(&self.db(), shortcode)
            .map_err(|e| storage(e, ClaimError::Storage))?
            .is_some()
        {
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
        let _registration = self.registration();
        if let Some(grant) = grant {
            self.voice_grants.remove(grant);
        }
        if self
            .hosts
            .remove_if(shortcode, |_, live| Arc::ptr_eq(live, entry))
            .is_some()
            && let Err(e) = set_live(&self.db(), shortcode, now_unix())
        {
            storage(e, ());
        }
    }

    /// At most `MAX_DISCOVER`, so one answer stays small whatever is registered.
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
        entries.truncate(MAX_DISCOVER);
        entries
    }

    /// Answers unlisted hosts too, unlike `discover`: holding a code is what
    /// buys a connection, so this hands out no new reachability.
    pub fn lookup(&self, code: &str) -> Option<DiscoverEntry> {
        self.hosts.get(code).map(|h| entry_for(h.key(), h.value()))
    }
}

pub const MAX_DISCOVER: usize = 500;

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
            address: None,
            last_seen_ms: AtomicI64::new(0),
            superseded: Default::default(),
        }
    }

    fn from(address: &str) -> HostEntry {
        HostEntry {
            address: Some(address.to_string()),
            public: true,
            ..host_entry()
        }
    }

    /// Keys are free and addresses are not: a handful of keys from one place
    /// cannot fill the table, and a host keeps the name it already holds.
    #[test]
    fn names_are_capped_per_address_but_an_owner_keeps_its_own() {
        let reg = Registry::new().with_caps(ReservationCaps {
            per_address: 2,
            ..Default::default()
        });
        let here = "203.0.113.4";
        for (slug, owner) in [("one", "key_a"), ("two", "key_b")] {
            let entry = reg.claim_host(slug, Some(owner), from(here)).unwrap();
            reg.release(slug, &entry, None);
        }
        assert_eq!(
            reg.claim_host("three", Some("key_c"), from(here)).err(),
            Some(ClaimError::AddressLimit)
        );
        assert!(
            reg.claim_host("three", Some("key_c"), from("198.51.100.1"))
                .is_ok()
        );
        assert!(
            reg.claim_host("one", Some("key_a"), from(here)).is_ok(),
            "a restarted host reclaims its name whatever the address count"
        );
    }

    #[test]
    fn live_hosts_are_capped_per_address_and_in_all_and_discover_stays_small() {
        let reg = Registry::new().with_caps(ReservationCaps {
            hosts_per_address: 2,
            hosts_total: 4,
            ..Default::default()
        });
        let here = "203.0.113.4";
        reg.claim_host("anon-1", None, from(here)).unwrap();
        reg.claim_host("anon-2", None, from(here)).unwrap();
        assert_eq!(
            reg.claim_host("anon-3", None, from(here)).err(),
            Some(ClaimError::AddressLimit)
        );
        reg.claim_host("named", Some("owner"), from("198.51.100.1"))
            .unwrap();
        assert!(
            reg.claim_host("named", Some("owner"), from("198.51.100.1"))
                .is_ok(),
            "a host re-registering after a dropped link replaces its session"
        );
        reg.claim_host("other", None, from("198.51.100.2")).unwrap();
        assert_eq!(
            reg.claim_host("full", None, from("198.51.100.3")).err(),
            Some(ClaimError::Full)
        );

        let wide = Registry::new().with_caps(ReservationCaps {
            hosts_per_address: usize::MAX,
            hosts_total: usize::MAX,
            ..Default::default()
        });
        for n in 0..MAX_DISCOVER + 20 {
            wide.claim_host(&format!("h{n}"), None, from(here)).unwrap();
        }
        assert_eq!(wide.discover().len(), MAX_DISCOVER);
    }

    #[test]
    fn an_ipv6_host_is_one_address_across_its_prefix() {
        let a = origin("2001:db8::1".parse().unwrap());
        assert_eq!(a, origin("2001:db8::ffff:2".parse().unwrap()));
        assert_ne!(a, origin("2001:db8:0:1::1".parse().unwrap()));
        assert_eq!(origin("::ffff:192.0.2.1".parse().unwrap()), "192.0.2.1");
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
        assert_eq!(registry.names_of("owner", now_unix()).len(), 3);
    }

    #[test]
    fn stored_uppercase_keys_reclaim_and_release_as_the_same_identity() {
        let dir = tmp();
        let path = dir.join("reservations.db");
        let uppercase = "AB".repeat(32);
        let lowercase = uppercase.to_ascii_lowercase();
        let registry = Registry::open(&path).unwrap().with_caps(ReservationCaps {
            per_owner: 1,
            total: 100,
            ..Default::default()
        });
        Connection::open(&path)
            .unwrap()
            .execute(
                "INSERT INTO reservations (slug, owner_pubkey, last_live_unix) VALUES ('legacy', ?1, ?2)",
                params![uppercase, now_unix() as i64],
            )
            .unwrap();
        assert!(registry.claim_name("legacy", &lowercase).is_ok());
        assert_eq!(
            registry.claim_name("second", &lowercase),
            Err(ClaimError::OwnerLimit)
        );
        assert!(registry.release_name("legacy", &lowercase).is_ok());
        assert!(registry.claim_name("second", &lowercase).is_ok());
        drop(registry);
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn tmp() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("rzv-res-{}", uuid::Uuid::new_v4()))
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
    fn reservations_survive_reopen() {
        let dir = tmp();
        let path = dir.join("reservations.db");
        Registry::open(&path)
            .unwrap()
            .claim_name("acme", "owner_a")
            .unwrap();
        let reopened = Registry::open(&path).unwrap();
        assert_eq!(
            reopened.reservation_owner("acme").as_deref(),
            Some("owner_a")
        );
        assert_eq!(
            reopened.claim_name("acme", "someone_else"),
            Err(ClaimError::Taken)
        );
        assert!(reopened.claim_name("acme", "owner_a").is_ok());
        drop(reopened);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_operator_edit_takes_effect_without_a_restart() {
        let dir = tmp();
        let path = dir.join("reservations.db");
        let registry = Registry::open(&path).unwrap();
        registry.claim_name("squatted", "owner_a").unwrap();
        Connection::open(&path)
            .unwrap()
            .execute("DELETE FROM reservations WHERE slug = 'squatted'", [])
            .unwrap();
        assert_eq!(registry.reservation_owner("squatted"), None);
        assert!(registry.claim_name("squatted", "owner_b").is_ok());
        drop(registry);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_name_expires_a_day_after_its_last_session_and_not_while_live() {
        let reg = Registry::new();
        let day = ReservationCaps::default().ttl_secs;
        reg.claim_name_at("idle", "owner_a", None, 1_000).unwrap();
        reg.claim_name_at("busy", "owner_a", None, 1_000).unwrap();
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
        reg.claim_name_at("old", "owner_a", None, 0).unwrap();
        assert_eq!(
            reg.claim_name_at("new", "owner_a", None, day - 1),
            Err(ClaimError::OwnerLimit)
        );
        assert!(reg.claim_name_at("new", "owner_a", None, day).is_ok());
        assert!(reg.claim_name_at("old", "owner_b", None, day).is_ok());
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
