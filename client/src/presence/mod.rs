//! What we tell a guild someone is doing, and the two things that can say it.
//!
//! Both producers are local. The server re-checks nothing here and cannot: an
//! activity is a claim about a machine it has no view of, exactly like `bot`
//! and `client_version` in `Identify` (trap 12). Lying costs nobody anything.

pub mod detect;
mod installed;
pub mod ipc;

use std::sync::Arc;

use dioxus::prelude::*;

use crate::protocol::{Activity, ClientMessage};
use crate::state::use_gateway;

/// How often the process table is walked. Long, because the scan is the whole
/// cost of the feature and nobody notices a game showing up ten seconds late.
const SCAN_EVERY: std::time::Duration = std::time::Duration::from_secs(15);

struct ScanWorker {
    _stop: std::sync::mpsc::Sender<()>,
    extra: Arc<parking_lot::Mutex<Vec<(String, String)>>>,
}

fn scan_until_stopped(
    tx: tokio::sync::mpsc::UnboundedSender<(Source, Option<Activity>)>,
    stop: std::sync::mpsc::Receiver<()>,
    mut scan: impl FnMut() -> Option<Activity>,
) {
    loop {
        if stop.try_recv() != Err(std::sync::mpsc::TryRecvError::Empty) {
            break;
        }
        if tx.send((Source::Detected, scan())).is_err() {
            return;
        }
        if stop.recv_timeout(SCAN_EVERY) != Err(std::sync::mpsc::RecvTimeoutError::Timeout) {
            break;
        }
    }
    if tx.send((Source::Detected, None)).is_err() {
        tracing::debug!("presence session already closed");
    }
}

impl ScanWorker {
    fn start(
        tx: tokio::sync::mpsc::UnboundedSender<(Source, Option<Activity>)>,
        extra: Vec<(String, String)>,
    ) -> Self {
        let (stop, stopped) = std::sync::mpsc::channel();
        let extra = Arc::new(parking_lot::Mutex::new(extra));
        let latest = extra.clone();
        std::thread::spawn(move || {
            let mut detector = detect::Detector::new(&latest.lock());
            scan_until_stopped(tx, stopped, || {
                detector.update_extra(&latest.lock());
                detector.scan()
            });
        });
        Self { _stop: stop, extra }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// A game that told us itself, through the local socket.
    Rpc,
    /// A process we recognised.
    Detected,
}

/// A game that describes itself beats one we merely spotted running, whatever
/// order the two arrived in.
#[derive(Default)]
pub struct Merge {
    rpc: Option<Activity>,
    detected: Option<Activity>,
}

impl Merge {
    pub fn apply(&mut self, source: Source, activity: Option<Activity>) {
        match source {
            Source::Rpc => self.rpc = activity,
            Source::Detected => self.detected = activity,
        }
    }

    pub fn current(&self) -> Option<&Activity> {
        self.rpc.as_ref().or(self.detected.as_ref())
    }
}

/// The settings the service actually acts on, lifted out of `ClientSettings` so
/// the task holds a plain value rather than reaching back into a `Signal`.
#[derive(Clone, PartialEq, Eq)]
struct Config {
    share: bool,
    detect: bool,
    discord_socket: bool,
    extra: Vec<(String, String)>,
}

/// Mounted inside a session: an activity has nowhere to go without a gateway.
/// Both halves are off unless asked for — a process list is a fingerprint of
/// what someone has installed, and a bound socket is visible to every program
/// on the machine.
#[component]
pub fn PresenceService() -> Element {
    let gateway = use_gateway();
    let settings = use_context::<Signal<crate::settings::ClientSettings>>();

    let config = Config {
        share: settings.read().share_activity,
        detect: settings.read().detect_games,
        discord_socket: settings.read().discord_rpc_socket,
        extra: settings.read().detect_extra.clone(),
    };

    // `use_future` runs once on mount and never again, so the toggles reach the
    // task through a channel rather than by it re-reading the signal.
    let config_tx = use_hook(|| {
        let (tx, rx) = tokio::sync::watch::channel(config.clone());
        (Arc::new(tx), rx)
    });
    let (tx_handle, config_rx) = config_tx;
    {
        let tx_handle = tx_handle.clone();
        use_effect(move || {
            let next = Config {
                share: settings.read().share_activity,
                detect: settings.read().detect_games,
                discord_socket: settings.read().discord_rpc_socket,
                extra: settings.read().detect_extra.clone(),
            };
            tx_handle.send_if_modified(|held| {
                let changed = *held != next;
                if changed {
                    *held = next;
                }
                changed
            });
        });
    }

    use_future(move || {
        let gateway = gateway.clone();
        let mut config_rx = config_rx.clone();
        async move {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(Source, Option<Activity>)>();
            let mut merge = Merge::default();
            let mut published: Option<Activity> = None;
            let mut scanning: Option<ScanWorker> = None;
            let mut rpc_tasks = tokio::task::JoinSet::new();
            let mut listening = false;

            loop {
                let cfg = config_rx.borrow().clone();

                if cfg.share && cfg.detect && scanning.is_none() {
                    scanning = Some(ScanWorker::start(tx.clone(), cfg.extra.clone()));
                }
                if let Some(worker) = &scanning {
                    let mut extra = worker.extra.lock();
                    if *extra != cfg.extra {
                        extra.clone_from(&cfg.extra);
                    }
                }
                if !(cfg.share && cfg.detect) {
                    scanning = None;
                }

                // Bound once and kept: releasing the socket name mid-session
                // would hand it to whatever asks next, which is the thing the
                // person turning this off is trying to avoid.
                if cfg.share && !listening {
                    listening = true;
                    let (rpc_tx, mut rpc_rx) = tokio::sync::mpsc::unbounded_channel();
                    rpc_tasks.spawn(ipc::listen(ipc::socket_names(cfg.discord_socket), rpc_tx));
                    let tx = tx.clone();
                    rpc_tasks.spawn(async move {
                        while let Some(update) = rpc_rx.recv().await {
                            if tx.send((Source::Rpc, update.activity)).is_err() {
                                return;
                            }
                        }
                    });
                }

                let now = cfg.share.then(|| merge.current().cloned()).flatten();
                if now != published {
                    published = now.clone();
                    gateway.send(ClientMessage::SetActivity { activity: now });
                }

                tokio::select! {
                    got = rx.recv() => match got {
                        Some((source, activity)) => merge.apply(source, activity),
                        None => return,
                    },
                    changed = config_rx.changed() => {
                        if changed.is_err() {
                            return;
                        }
                    }
                }
            }
        }
    });

    rsx! { Fragment {} }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ActivityKind;

    #[test]
    fn ending_a_session_wakes_the_scanner_without_waiting_for_the_next_scan() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (stop, stopped) = std::sync::mpsc::channel();
        let worker = ScanWorker {
            _stop: stop,
            extra: Arc::default(),
        };
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            scan_until_stopped(tx, stopped, || named("Factorio"));
            done_tx.send(()).expect("test completion receiver");
        });
        assert_eq!(rx.blocking_recv().unwrap().1, named("Factorio"));
        drop(worker);
        done_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("scanner must wake when its owner is dropped");
        thread.join().unwrap();
        assert_eq!(rx.blocking_recv(), Some((Source::Detected, None)));
        assert!(rx.blocking_recv().is_none());
    }

    #[test]
    fn a_cancelled_scanner_does_not_start_another_process_scan() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (stop, stopped) = std::sync::mpsc::channel::<()>();
        drop(stop);
        scan_until_stopped(tx, stopped, || panic!("cancelled scan ran"));
        assert_eq!(rx.blocking_recv(), Some((Source::Detected, None)));
    }

    fn named(name: &str) -> Option<Activity> {
        Some(Activity {
            kind: ActivityKind::Playing,
            name: name.to_string(),
            details: None,
            state: None,
            started_ms: None,
        })
    }

    #[test]
    fn a_game_that_speaks_for_itself_outranks_one_we_spotted() {
        let mut m = Merge::default();
        m.apply(Source::Detected, named("Factorio"));
        assert_eq!(m.current().map(|a| a.name.as_str()), Some("Factorio"));

        m.apply(Source::Rpc, named("Seablock"));
        assert_eq!(m.current().map(|a| a.name.as_str()), Some("Seablock"));
    }

    #[test]
    fn losing_the_socket_falls_back_to_the_process_we_can_still_see() {
        let mut m = Merge::default();
        m.apply(Source::Detected, named("Factorio"));
        m.apply(Source::Rpc, named("Seablock"));

        m.apply(Source::Rpc, None);
        assert_eq!(m.current().map(|a| a.name.as_str()), Some("Factorio"));

        m.apply(Source::Detected, None);
        assert!(m.current().is_none());
    }
}
