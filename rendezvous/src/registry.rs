//! Active host registrations + pending proxy pairings.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::WebSocket;
use dashmap::DashMap;
use tokio::sync::{Mutex, oneshot};

use crate::protocol::{DiscoverEntry, RendezvousToHost};

/// Each registered host has a sender for control messages (so we can notify
/// them of incoming friends), plus the public-browse metadata they opted
/// into at register time.
pub struct HostEntry {
    pub name: Option<String>,
    pub description: Option<String>,
    pub public: bool,
    pub control_tx: tokio::sync::mpsc::UnboundedSender<RendezvousToHost>,
}

/// A friend is waiting; once the host opens the matching `/proxy/{session_id}`
/// connection, we hand it through `host_proxy_tx` to the friend handler.
pub struct PendingPairing {
    pub host_proxy_tx: oneshot::Sender<WebSocket>,
}

pub struct Registry {
    /// shortcode -> registered host
    pub hosts: DashMap<String, Arc<HostEntry>>,
    /// session_id -> friend's pending pairing slot (waiting for the host's
    /// proxy WS to arrive)
    pub pending: DashMap<String, Mutex<Option<PendingPairing>>>,
}

impl Registry {
    pub fn new() -> Self {
        Self {
            hosts: DashMap::new(),
            pending: DashMap::new(),
        }
    }

    /// Try to claim a shortcode; returns false if it's already taken.
    pub fn try_claim(&self, shortcode: &str, entry: HostEntry) -> bool {
        if self.hosts.contains_key(shortcode) {
            return false;
        }
        self.hosts.insert(shortcode.to_string(), Arc::new(entry));
        true
    }

    pub fn release(&self, shortcode: &str) {
        self.hosts.remove(shortcode);
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

    /// Snapshot of all hosts that opted into public listing. Sorted by name
    /// (case-insensitive) with un-named hosts at the end.
    pub fn discover(&self) -> Vec<DiscoverEntry> {
        let mut entries: Vec<DiscoverEntry> = self
            .hosts
            .iter()
            .filter(|h| h.value().public)
            .map(|h| DiscoverEntry {
                shortcode: h.key().clone(),
                name: h.value().name.clone(),
                description: h.value().description.clone(),
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
