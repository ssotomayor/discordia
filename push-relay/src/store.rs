use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use discordia_protocol::push::{HANDLE_BYTES, RegisterDevice};
use rand::RngCore;
use rusqlite::{Connection, OptionalExtension, params};

#[derive(Debug, Clone, Copy)]
pub struct Caps {
    pub devices_per_key: usize,
    pub handles_per_device: usize,
    /// New registrations are refused past it; nobody's device is evicted for a stranger's.
    pub total_devices: usize,
    pub ttl_secs: u64,
}

impl Default for Caps {
    fn default() -> Self {
        Self {
            devices_per_key: 4,
            handles_per_device: 64,
            total_devices: 100_000,
            ttl_secs: 30 * 24 * 60 * 60,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum StoreError {
    NoDevice,
    HandleCap,
    Full,
    Storage(String),
}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        StoreError::Storage(e.to_string())
    }
}

/// One FCM registration: what a send needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub device: i64,
    pub token: String,
}

const SCHEMA: &str = "
    PRAGMA foreign_keys = ON;
    CREATE TABLE IF NOT EXISTS devices (
        id INTEGER PRIMARY KEY,
        pubkey TEXT NOT NULL,
        token TEXT NOT NULL,
        package TEXT NOT NULL,
        dm INTEGER NOT NULL,
        relays TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL,
        expires_at INTEGER NOT NULL,
        UNIQUE (pubkey, token)
    );
    CREATE INDEX IF NOT EXISTS devices_by_expiry ON devices (expires_at);
    CREATE INDEX IF NOT EXISTS devices_by_token ON devices (token);
    CREATE TABLE IF NOT EXISTS handles (
        handle TEXT PRIMARY KEY,
        device INTEGER NOT NULL REFERENCES devices (id) ON DELETE CASCADE,
        created_at INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS handles_by_device ON handles (device);
";

pub struct Store {
    db: Mutex<Connection>,
    caps: Caps,
    changed: tokio::sync::watch::Sender<u64>,
}

pub fn new_handle() -> String {
    let mut bytes = [0u8; HANDLE_BYTES];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// What `new_handle` makes, so a path segment is checked before any query.
pub fn is_handle(s: &str) -> bool {
    s.len() == 43
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

impl Store {
    pub fn in_memory(caps: Caps) -> Result<Self, String> {
        let db = Connection::open_in_memory().map_err(|e| e.to_string())?;
        db.execute_batch(SCHEMA).map_err(|e| e.to_string())?;
        Ok(Self {
            db: Mutex::new(db),
            caps,
            changed: tokio::sync::watch::Sender::new(0),
        })
    }

    pub fn open(path: &Path, caps: Caps) -> Result<Self, String> {
        let fail = |e: rusqlite::Error| format!("push store {}: {e}", path.display());
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        }
        let db = Connection::open(path).map_err(fail)?;
        // `ping` and an operator's `sqlite3` open the same file.
        db.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(fail)?;
        db.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))
            .map_err(fail)?;
        db.execute_batch(SCHEMA).map_err(fail)?;
        Ok(Self {
            db: Mutex::new(db),
            caps,
            changed: tokio::sync::watch::Sender::new(0),
        })
    }

    pub fn caps(&self) -> Caps {
        self.caps
    }

    /// Bumped by every write that can change `dm_watch_list`, refreshes included:
    /// the watcher diffs the list, so a spurious bump costs one query.
    pub fn changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.changed.subscribe()
    }

    fn bump(&self) {
        self.changed.send_modify(|n| *n = n.wrapping_add(1));
    }

    fn db(&self) -> MutexGuard<'_, Connection> {
        // A panic mid-statement leaves SQLite consistent; the guard is all that is poisoned.
        self.db.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// An expired row the sweep has not reached is no device: putting it again is new.
    pub fn device_exists(&self, pubkey: &str, token: &str, now: u64) -> Result<bool, StoreError> {
        Ok(self
            .db()
            .prepare_cached(
                "SELECT 1 FROM devices WHERE pubkey = ?1 AND token = ?2 AND expires_at > ?3",
            )?
            .exists(params![pubkey, token, now as i64])?)
    }

    /// Upserts by (pubkey, token) and returns the new expiry.
    pub fn upsert_device(
        &self,
        pubkey: &str,
        req: &RegisterDevice,
        now: u64,
    ) -> Result<u64, StoreError> {
        let expires = now + self.caps.ttl_secs;
        let relays =
            serde_json::to_string(&req.relays).map_err(|e| StoreError::Storage(e.to_string()))?;
        let mut db = self.db();
        let tx = db.transaction()?;
        let exists = tx
            .prepare_cached("SELECT 1 FROM devices WHERE pubkey = ?1 AND token = ?2")?
            .exists(params![pubkey, req.token])?;
        // Only the install and this relay know a token, so presenting one held by
        // another key is an account switch on that phone, not a theft.
        let moved: i64 = tx
            .prepare_cached("SELECT COUNT(*) FROM devices WHERE token = ?1 AND pubkey != ?2")?
            .query_row(params![req.token, pubkey], |r| r.get(0))?;
        let replaced: Option<i64> = match &req.replaces {
            Some(old) if *old != req.token => tx
                .prepare_cached("SELECT id FROM devices WHERE pubkey = ?1 AND token = ?2")?
                .query_row(params![pubkey, old], |r| r.get(0))
                .optional()?,
            _ => None,
        };
        if !exists && replaced.is_none() && moved == 0 {
            let total: i64 = tx.query_row("SELECT COUNT(*) FROM devices", [], |r| r.get(0))?;
            let own: i64 = tx.query_row(
                "SELECT COUNT(*) FROM devices WHERE pubkey = ?1",
                [pubkey],
                |r| r.get(0),
            )?;
            if total as usize >= self.caps.total_devices
                && (own as usize) < self.caps.devices_per_key
            {
                return Err(StoreError::Full);
            }
        }
        if moved > 0 {
            tx.prepare_cached("DELETE FROM devices WHERE token = ?1 AND pubkey != ?2")?
                .execute(params![req.token, pubkey])?;
        }
        tx.prepare_cached(
            "INSERT INTO devices (pubkey, token, package, dm, relays, created_at, updated_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?7)
             ON CONFLICT (pubkey, token) DO UPDATE SET
                package = excluded.package, dm = excluded.dm, relays = excluded.relays,
                updated_at = excluded.updated_at, expires_at = excluded.expires_at",
        )?
        .execute(params![
            pubkey,
            req.token,
            req.package,
            req.dm,
            relays,
            now as i64,
            expires as i64
        ])?;
        let id: i64 = tx
            .prepare_cached("SELECT id FROM devices WHERE pubkey = ?1 AND token = ?2")?
            .query_row(params![pubkey, req.token], |r| r.get(0))?;
        if let Some(old) = replaced {
            tx.prepare_cached("UPDATE handles SET device = ?1 WHERE device = ?2")?
                .execute(params![id, old])?;
            tx.prepare_cached("DELETE FROM devices WHERE id = ?1")?
                .execute([old])?;
        }
        tx.prepare_cached(
            "DELETE FROM devices WHERE pubkey = ?1 AND id NOT IN (
                SELECT id FROM devices WHERE pubkey = ?1
                ORDER BY updated_at DESC, id DESC LIMIT ?2)",
        )?
        .execute(params![pubkey, self.caps.devices_per_key as i64])?;
        tx.commit()?;
        drop(db);
        self.bump();
        Ok(expires)
    }

    pub fn delete_device(&self, pubkey: &str, token: &str) -> Result<bool, StoreError> {
        let gone = self
            .db()
            .prepare_cached("DELETE FROM devices WHERE pubkey = ?1 AND token = ?2")?
            .execute(params![pubkey, token])?
            > 0;
        if gone {
            self.bump();
        }
        Ok(gone)
    }

    pub fn delete_device_id(&self, id: i64) -> Result<bool, StoreError> {
        let gone = self
            .db()
            .prepare_cached("DELETE FROM devices WHERE id = ?1")?
            .execute([id])?
            > 0;
        if gone {
            self.bump();
        }
        Ok(gone)
    }

    pub fn create_handle(&self, pubkey: &str, token: &str, now: u64) -> Result<String, StoreError> {
        let mut db = self.db();
        let tx = db.transaction()?;
        let device: i64 = tx
            .prepare_cached(
                "SELECT id FROM devices WHERE pubkey = ?1 AND token = ?2 AND expires_at > ?3",
            )?
            .query_row(params![pubkey, token, now as i64], |r| r.get(0))
            .optional()?
            .ok_or(StoreError::NoDevice)?;
        let held: i64 = tx
            .prepare_cached("SELECT COUNT(*) FROM handles WHERE device = ?1")?
            .query_row([device], |r| r.get(0))?;
        if held as usize >= self.caps.handles_per_device {
            return Err(StoreError::HandleCap);
        }
        let handle = new_handle();
        tx.prepare_cached("INSERT INTO handles (handle, device, created_at) VALUES (?1, ?2, ?3)")?
            .execute(params![handle, device, now as i64])?;
        tx.commit()?;
        Ok(handle)
    }

    /// Only the key whose device holds it may delete a handle.
    pub fn delete_handle(&self, pubkey: &str, handle: &str) -> Result<bool, StoreError> {
        Ok(self
            .db()
            .prepare_cached(
                "DELETE FROM handles WHERE handle = ?1
                 AND device IN (SELECT id FROM devices WHERE pubkey = ?2)",
            )?
            .execute(params![handle, pubkey])?
            > 0)
    }

    pub fn handle_target(&self, handle: &str, now: u64) -> Result<Option<Target>, StoreError> {
        Ok(self
            .db()
            .prepare_cached(
                "SELECT d.id, d.token FROM handles h JOIN devices d ON d.id = h.device
                 WHERE h.handle = ?1 AND d.expires_at > ?2",
            )?
            .query_row(params![handle, now as i64], |r| {
                Ok(Target {
                    device: r.get(0)?,
                    token: r.get(1)?,
                })
            })
            .optional()?)
    }

    pub fn devices_of(
        &self,
        pubkey: &str,
        dm_only: bool,
        now: u64,
    ) -> Result<Vec<Target>, StoreError> {
        let db = self.db();
        let mut stmt = db.prepare_cached(
            "SELECT id, token FROM devices
             WHERE pubkey = ?1 AND expires_at > ?2 AND (dm = 1 OR ?3 = 0)
             ORDER BY id",
        )?;
        let rows = stmt.query_map(params![pubkey, now as i64, dm_only], |r| {
            Ok(Target {
                device: r.get(0)?,
                token: r.get(1)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Every unexpired key with DMs on and the relays its devices named; empty when
    /// any of them named none, which means every relay the watcher may dial.
    pub fn dm_watch_list(&self, now: u64) -> Result<Vec<(String, Vec<String>)>, StoreError> {
        let db = self.db();
        let mut stmt = db.prepare_cached(
            "SELECT pubkey, relays FROM devices WHERE dm = 1 AND expires_at > ?1 ORDER BY pubkey",
        )?;
        let rows = stmt.query_map([now as i64], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        let mut out: Vec<(String, Vec<String>)> = Vec::new();
        for row in rows {
            let (pubkey, relays) = row?;
            let relays: Vec<String> = serde_json::from_str(&relays).unwrap_or_default();
            match out.last_mut() {
                Some((last, merged)) if *last == pubkey => {
                    if merged.is_empty() || relays.is_empty() {
                        merged.clear();
                        continue;
                    }
                    for r in relays {
                        if !merged.contains(&r) {
                            merged.push(r);
                        }
                    }
                }
                _ => out.push((pubkey, relays)),
            }
        }
        Ok(out)
    }

    pub fn sweep(&self, now: u64) -> Result<usize, StoreError> {
        let n = self
            .db()
            .prepare_cached("DELETE FROM devices WHERE expires_at <= ?1")?
            .execute([now as i64])?;
        if n > 0 {
            self.bump();
        }
        Ok(n)
    }

    pub fn device_count(&self) -> Result<usize, StoreError> {
        let n: i64 = self
            .db()
            .query_row("SELECT COUNT(*) FROM devices", [], |r| r.get(0))?;
        Ok(n as usize)
    }

    pub fn handle_count(&self, pubkey: &str, token: &str) -> Result<usize, StoreError> {
        let n: i64 = self.db().query_row(
            "SELECT COUNT(*) FROM handles h JOIN devices d ON d.id = h.device
             WHERE d.pubkey = ?1 AND d.token = ?2",
            params![pubkey, token],
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 24 * 60 * 60;
    const T0: u64 = 1_700_000_000;

    fn key(n: u8) -> String {
        format!("{n:02x}").repeat(32)
    }

    fn reg(token: &str) -> RegisterDevice {
        RegisterDevice {
            token: token.into(),
            package: "com.discordia.app".into(),
            dm: false,
            relays: vec![],
            replaces: None,
        }
    }

    fn store() -> Store {
        Store::in_memory(Caps::default()).unwrap()
    }

    #[test]
    fn a_second_put_refreshes_the_same_row() {
        let s = store();
        assert_eq!(
            s.upsert_device(&key(1), &reg("a"), T0).unwrap(),
            T0 + 30 * DAY
        );
        let h = s.create_handle(&key(1), "a", T0).unwrap();
        assert_eq!(
            s.upsert_device(&key(1), &reg("a"), T0 + DAY).unwrap(),
            T0 + 31 * DAY
        );
        assert_eq!(s.device_count().unwrap(), 1);
        assert!(s.handle_target(&h, T0 + DAY).unwrap().is_some());
    }

    #[test]
    fn a_token_put_under_another_key_moves_there_without_its_handles() {
        let s = store();
        s.upsert_device(&key(1), &reg("a"), T0).unwrap();
        s.upsert_device(&key(1), &reg("keep"), T0).unwrap();
        let h = s.create_handle(&key(1), "a", T0).unwrap();
        s.upsert_device(&key(2), &reg("a"), T0 + 1).unwrap();
        assert_eq!(s.device_count().unwrap(), 2);
        assert!(s.handle_target(&h, T0 + 1).unwrap().is_none());
        assert!(!s.delete_device(&key(1), "a").unwrap());
        let mine: Vec<_> = s
            .devices_of(&key(1), false, T0 + 1)
            .unwrap()
            .into_iter()
            .map(|t| t.token)
            .collect();
        assert_eq!(mine, ["keep"]);
        assert_eq!(s.devices_of(&key(2), false, T0 + 1).unwrap().len(), 1);
    }

    #[test]
    fn a_full_relay_still_lets_a_token_change_keys() {
        let s = Store::in_memory(Caps {
            total_devices: 2,
            ..Caps::default()
        })
        .unwrap();
        s.upsert_device(&key(1), &reg("a"), T0).unwrap();
        s.upsert_device(&key(2), &reg("b"), T0).unwrap();
        assert!(s.upsert_device(&key(3), &reg("a"), T0).is_ok());
        assert_eq!(s.device_count().unwrap(), 2);
    }

    #[test]
    fn every_write_that_can_move_the_watch_list_signals() {
        let s = store();
        let mut rx = s.changes();
        s.upsert_device(&key(1), &reg("a"), T0).unwrap();
        assert!(rx.has_changed().unwrap());
        rx.mark_unchanged();
        assert!(!s.delete_device(&key(1), "nope").unwrap());
        assert!(
            !rx.has_changed().unwrap(),
            "nothing deleted, nothing to say"
        );
        s.delete_device(&key(1), "a").unwrap();
        assert!(rx.has_changed().unwrap());
        rx.mark_unchanged();
        s.upsert_device(&key(1), &reg("b"), T0).unwrap();
        rx.mark_unchanged();
        s.sweep(T0 + 31 * DAY).unwrap();
        assert!(rx.has_changed().unwrap());
        rx.mark_unchanged();
        s.upsert_device(&key(1), &reg("c"), T0).unwrap();
        let id = s.devices_of(&key(1), false, T0).unwrap()[0].device;
        rx.mark_unchanged();
        s.delete_device_id(id).unwrap();
        assert!(rx.has_changed().unwrap());
    }

    #[test]
    fn a_rotated_token_carries_its_handles() {
        let s = store();
        s.upsert_device(&key(1), &reg("old"), T0).unwrap();
        let h1 = s.create_handle(&key(1), "old", T0).unwrap();
        let h2 = s.create_handle(&key(1), "old", T0).unwrap();
        let mut rotated = reg("new");
        rotated.replaces = Some("old".into());
        s.upsert_device(&key(1), &rotated, T0 + 1).unwrap();
        assert_eq!(s.device_count().unwrap(), 1);
        for h in [h1, h2] {
            assert_eq!(s.handle_target(&h, T0 + 1).unwrap().unwrap().token, "new");
        }
    }

    #[test]
    fn replaces_cannot_reach_another_keys_device() {
        let s = store();
        s.upsert_device(&key(1), &reg("victim"), T0).unwrap();
        let h = s.create_handle(&key(1), "victim", T0).unwrap();
        let mut theft = reg("mine");
        theft.replaces = Some("victim".into());
        s.upsert_device(&key(2), &theft, T0).unwrap();
        assert_eq!(s.handle_target(&h, T0).unwrap().unwrap().token, "victim");
        assert_eq!(s.device_count().unwrap(), 2);
    }

    #[test]
    fn a_fifth_device_evicts_the_least_recently_registered() {
        let s = store();
        for (i, t) in ["a", "b", "c", "d"].into_iter().enumerate() {
            s.upsert_device(&key(1), &reg(t), T0 + i as u64).unwrap();
        }
        s.upsert_device(&key(1), &reg("a"), T0 + 10).unwrap();
        s.upsert_device(&key(1), &reg("e"), T0 + 11).unwrap();
        let tokens: Vec<String> = s
            .devices_of(&key(1), false, T0 + 11)
            .unwrap()
            .into_iter()
            .map(|t| t.token)
            .collect();
        assert_eq!(tokens, ["a", "c", "d", "e"]);
    }

    #[test]
    fn a_device_holds_at_most_sixty_four_handles() {
        let s = store();
        s.upsert_device(&key(1), &reg("a"), T0).unwrap();
        for _ in 0..64 {
            s.create_handle(&key(1), "a", T0).unwrap();
        }
        assert_eq!(
            s.create_handle(&key(1), "a", T0),
            Err(StoreError::HandleCap)
        );
        assert_eq!(s.handle_count(&key(1), "a").unwrap(), 64);
        assert_eq!(s.create_handle(&key(2), "a", T0), Err(StoreError::NoDevice));
    }

    #[test]
    fn a_full_relay_refuses_newcomers_and_keeps_everyone() {
        let s = Store::in_memory(Caps {
            total_devices: 2,
            ..Caps::default()
        })
        .unwrap();
        s.upsert_device(&key(1), &reg("a"), T0).unwrap();
        s.upsert_device(&key(2), &reg("b"), T0).unwrap();
        assert_eq!(
            s.upsert_device(&key(3), &reg("c"), T0),
            Err(StoreError::Full)
        );
        assert_eq!(s.device_count().unwrap(), 2);
        assert!(
            s.upsert_device(&key(1), &reg("a"), T0 + 1).is_ok(),
            "a refresh is not new"
        );
        let mut rotated = reg("a2");
        rotated.replaces = Some("a".into());
        assert!(
            s.upsert_device(&key(1), &rotated, T0 + 2).is_ok(),
            "a rotation does not grow"
        );
        assert_eq!(
            s.upsert_device(&key(1), &reg("z"), T0 + 3),
            Err(StoreError::Full)
        );
    }

    #[test]
    fn thirty_days_without_a_put_expires_a_device_and_its_handles() {
        let s = store();
        s.upsert_device(&key(1), &reg("a"), T0).unwrap();
        let h = s.create_handle(&key(1), "a", T0).unwrap();
        let end = T0 + 30 * DAY;
        assert!(s.handle_target(&h, end - 1).unwrap().is_some());
        assert!(
            s.handle_target(&h, end).unwrap().is_none(),
            "expired before any sweep"
        );
        assert_eq!(s.sweep(end - 1).unwrap(), 0);
        assert_eq!(s.sweep(end).unwrap(), 1);
        assert_eq!(s.device_count().unwrap(), 0);
        let orphans: i64 = s
            .db()
            .query_row("SELECT COUNT(*) FROM handles", [], |r| r.get(0))
            .unwrap();
        assert_eq!(orphans, 0);
    }

    #[test]
    fn a_handle_is_deleted_only_by_its_owner() {
        let s = store();
        s.upsert_device(&key(1), &reg("a"), T0).unwrap();
        let h = s.create_handle(&key(1), "a", T0).unwrap();
        assert!(is_handle(&h));
        assert!(!s.delete_handle(&key(2), &h).unwrap());
        assert!(s.delete_handle(&key(1), &h).unwrap());
        assert!(s.handle_target(&h, T0).unwrap().is_none());
    }

    #[test]
    fn the_watch_list_is_dm_devices_with_relays_merged() {
        let s = store();
        let mut a = reg("a");
        a.dm = true;
        a.relays = vec!["wss://one".into()];
        let mut b = reg("b");
        b.dm = true;
        b.relays = vec!["wss://one".into(), "wss://two".into()];
        s.upsert_device(&key(1), &a, T0).unwrap();
        s.upsert_device(&key(1), &b, T0).unwrap();
        s.upsert_device(&key(2), &reg("c"), T0).unwrap();
        assert_eq!(
            s.dm_watch_list(T0).unwrap(),
            vec![(
                key(1),
                vec!["wss://one".to_string(), "wss://two".to_string()]
            )]
        );
        assert_eq!(s.devices_of(&key(1), true, T0).unwrap().len(), 2);
        assert!(s.devices_of(&key(2), true, T0).unwrap().is_empty());

        let mut anywhere = reg("d");
        anywhere.dm = true;
        s.upsert_device(&key(1), &anywhere, T0).unwrap();
        assert_eq!(
            s.dm_watch_list(T0).unwrap(),
            vec![(key(1), vec![])],
            "a device that names no relay widens its key to all of them"
        );
    }
}
