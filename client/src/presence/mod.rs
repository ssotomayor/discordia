//! What we tell a guild someone is doing, and the two things that can say it.
//!
//! Both producers are local. The server re-checks nothing here and cannot: an
//! activity is a claim about a machine it has no view of, exactly like `bot`
//! and `client_version` in `Identify` (trap 12). Lying costs nobody anything.

pub mod detect;
pub mod ipc;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use dioxus::prelude::*;

use crate::protocol::{Activity, ClientMessage};
use crate::state::use_gateway;

/// How often the process table is walked. Long, because the scan is the whole
/// cost of the feature and nobody notices a game showing up ten seconds late.
const SCAN_EVERY: std::time::Duration = std::time::Duration::from_secs(15);

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
            let mut scanning: Option<Arc<AtomicBool>> = None;
            let mut listening = false;

            loop {
                let cfg = config_rx.borrow().clone();

                if cfg.share && cfg.detect && scanning.is_none() {
                    let run = Arc::new(AtomicBool::new(true));
                    scanning = Some(run.clone());
                    let tx = tx.clone();
                    let extra = cfg.extra.clone();
                    // Its own thread, not a task: the scan is blocking and long
                    // enough to be felt on the runtime that carries the audio.
                    std::thread::spawn(move || {
                        let mut detector = detect::Detector::new(&extra);
                        while run.load(Ordering::Relaxed) {
                            if tx.send((Source::Detected, detector.scan())).is_err() {
                                return;
                            }
                            std::thread::sleep(SCAN_EVERY);
                        }
                        let _ = tx.send((Source::Detected, None));
                    });
                }
                if let Some(run) = &scanning
                    && !(cfg.share && cfg.detect)
                {
                    run.store(false, Ordering::Relaxed);
                    scanning = None;
                }

                // Bound once and kept: releasing the socket name mid-session
                // would hand it to whatever asks next, which is the thing the
                // person turning this off is trying to avoid.
                if cfg.share && !listening {
                    listening = true;
                    let (rpc_tx, mut rpc_rx) = tokio::sync::mpsc::unbounded_channel();
                    tokio::spawn(ipc::listen(ipc::socket_names(cfg.discord_socket), rpc_tx));
                    let tx = tx.clone();
                    tokio::spawn(async move {
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
