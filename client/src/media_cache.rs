use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, Weak, mpsc};

use parking_lot::Mutex;
use tokio::sync::{Notify, mpsc::UnboundedSender};

use crate::protocol::ClientMessage;

const BYTE_BUDGET: usize = 128 * 1024 * 1024;
const ENTRY_BUDGET: usize = 2048;

#[derive(Clone)]
struct Entry {
    data: Arc<String>,
    touched: Arc<AtomicU64>,
}

#[derive(Default)]
struct Work {
    demanded: HashSet<String>,
    queued: HashSet<String>,
    loaded: Vec<(String, String)>,
}

#[derive(Default)]
pub(crate) struct Updates {
    pub ready: Notify,
    work: Mutex<Work>,
}

#[derive(Clone)]
pub struct MediaCache {
    entries: HashMap<String, Entry>,
    bytes: usize,
    byte_budget: usize,
    clock: Arc<AtomicU64>,
    updates: Arc<Updates>,
}

impl Default for MediaCache {
    fn default() -> Self {
        Self::with_budget(BYTE_BUDGET)
    }
}

impl MediaCache {
    fn with_budget(byte_budget: usize) -> Self {
        Self {
            entries: HashMap::new(),
            bytes: 0,
            byte_budget,
            clock: Arc::new(AtomicU64::new(0)),
            updates: Arc::default(),
        }
    }

    pub fn get(&self, address: &str) -> Option<&String> {
        if let Some(entry) = self.entries.get(address) {
            entry.touched.store(
                self.clock.fetch_add(1, Ordering::Relaxed),
                Ordering::Relaxed,
            );
            return Some(entry.data.as_ref());
        }
        let mut work = self.updates.work.lock();
        if work.demanded.insert(address.to_owned()) {
            work.queued.insert(address.to_owned());
            self.updates.ready.notify_one();
        }
        None
    }

    pub fn contains_key(&self, address: &str) -> bool {
        self.entries.contains_key(address)
    }

    pub fn insert(&mut self, address: String, data: String) {
        if let Some(previous) = self.entries.remove(&address) {
            self.bytes -= previous.data.len();
        }
        let data = if data.len() <= self.byte_budget {
            data
        } else {
            String::new()
        };
        while self.bytes + data.len() > self.byte_budget || self.entries.len() >= ENTRY_BUDGET {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.touched.load(Ordering::Relaxed))
                .map(|(key, _)| key.clone());
            let Some(oldest) = oldest else { break };
            if let Some(entry) = self.entries.remove(&oldest) {
                self.bytes -= entry.data.len();
            }
        }
        {
            let mut work = self.updates.work.lock();
            work.demanded.remove(&address);
            work.queued.remove(&address);
        }
        self.bytes += data.len();
        self.entries.insert(
            address,
            Entry {
                data: Arc::new(data),
                touched: Arc::new(AtomicU64::new(self.clock.fetch_add(1, Ordering::Relaxed))),
            },
        );
    }

    pub(crate) fn updates(&self) -> Arc<Updates> {
        self.updates.clone()
    }

    pub(crate) fn has_work(&self) -> bool {
        let work = self.updates.work.lock();
        !work.queued.is_empty() || !work.loaded.is_empty()
    }

    pub(crate) fn take_work(&self) -> (Vec<(String, String)>, Vec<String>) {
        let mut work = self.updates.work.lock();
        (
            std::mem::take(&mut work.loaded),
            work.queued.drain().collect(),
        )
    }
}

enum DiskJob {
    Load(Vec<String>, UnboundedSender<ClientMessage>, Weak<Updates>),
    Store(String, String),
}

fn disk_worker() -> Option<&'static mpsc::SyncSender<DiskJob>> {
    static WORKER: OnceLock<Option<mpsc::SyncSender<DiskJob>>> = OnceLock::new();
    WORKER
        .get_or_init(|| {
            let (tx, rx) = mpsc::sync_channel(8);
            match std::thread::Builder::new()
                .name("media-cache".into())
                .spawn(move || {
                    while let Ok(job) = rx.recv() {
                        match job {
                            DiskJob::Store(address, data) => {
                                crate::emoji::store_cached(&address, &data)
                            }
                            DiskJob::Load(addresses, tx, updates) => {
                                let Some(updates) = updates.upgrade() else {
                                    continue;
                                };
                                let mut missing = Vec::new();
                                let mut loaded = Vec::new();
                                for address in addresses {
                                    match crate::emoji::load_cached(&address) {
                                        Some(data) => loaded.push((address, data)),
                                        None => missing.push(address),
                                    }
                                }
                                if !loaded.is_empty() {
                                    updates.work.lock().loaded.extend(loaded);
                                    updates.ready.notify_one();
                                }
                                if !missing.is_empty()
                                    && tx
                                        .send(ClientMessage::FetchEmoji { images: missing })
                                        .is_err()
                                {
                                    tracing::debug!("media cache session closed");
                                }
                            }
                        }
                    }
                }) {
                Ok(_) => Some(tx),
                Err(error) => {
                    tracing::warn!(%error, "media disk cache unavailable");
                    None
                }
            }
        })
        .as_ref()
}

pub(crate) fn load(
    addresses: Vec<String>,
    tx: &UnboundedSender<ClientMessage>,
    cache: &MediaCache,
) {
    let job = DiskJob::Load(addresses, tx.clone(), Arc::downgrade(&cache.updates));
    let result = match disk_worker() {
        Some(worker) => worker.try_send(job),
        None => Err(mpsc::TrySendError::Disconnected(job)),
    };
    if let Err(error) = result {
        let job = match error {
            mpsc::TrySendError::Full(job) | mpsc::TrySendError::Disconnected(job) => job,
        };
        if let DiskJob::Load(images, tx, _) = job
            && tx.send(ClientMessage::FetchEmoji { images }).is_err()
        {
            tracing::debug!("media request session closed");
        }
    }
}

pub(crate) fn store(address: &str, data: &str) {
    if let Some(worker) = disk_worker()
        && worker
            .try_send(DiskJob::Store(address.into(), data.into()))
            .is_err()
    {
        tracing::debug!("media disk cache queue full; keeping memory copy");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_evicts_the_least_recently_viewed_image() {
        let mut cache = MediaCache::with_budget(8);
        cache.insert("a".into(), "aaaa".into());
        cache.insert("b".into(), "bbbb".into());
        assert_eq!(cache.get("a").map(String::as_str), Some("aaaa"));
        cache.insert("c".into(), "cccc".into());
        assert!(!cache.contains_key("b"));
        assert!(cache.contains_key("a"));
        assert_eq!(cache.bytes, 8);
    }

    #[test]
    fn missing_images_are_requested_once_until_answered() {
        let mut cache = MediaCache::with_budget(8);
        assert!(cache.get("a").is_none());
        assert_eq!(cache.take_work().1, ["a"]);
        assert!(cache.get("a").is_none());
        assert!(!cache.has_work());
        cache.insert("a".into(), "data".into());
        assert!(cache.get("a").is_some());
        assert!(!cache.has_work());
    }

    #[test]
    fn replacement_and_oversized_data_preserve_the_budget() {
        let mut cache = MediaCache::with_budget(8);
        cache.insert("a".into(), "12345678".into());
        cache.insert("a".into(), "12".into());
        assert_eq!(cache.bytes, 2);
        cache.insert("b".into(), "too large to cache".into());
        assert_eq!(cache.bytes, 2);
        assert_eq!(cache.get("b").map(String::as_str), Some(""));
    }
}
