use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

static GENERATION: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Target {
    pub url: String,
    pub token: String,
    pub voice_epoch: u64,
    pub ice_servers: Vec<crate::protocol::IceServer>,
}

/// What the page reports for one attempt; serde rejects any other status at the boundary.
#[derive(serde::Deserialize)]
pub(super) struct Event {
    pub generation: u64,
    pub status: Status,
}

#[derive(Clone, Copy, Debug, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Status {
    Connected,
    Retry,
    Blocked,
}

pub(super) enum Action {
    Connect { target: Target, generation: u64 },
    Disconnect,
}

/// Where the current target's attempts stand: one value, so a blocked room cannot also await a retry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// An attempt is out, or connected; `attempt` counts the failures before it.
    Trying { attempt: u8 },
    /// The last attempt asked for a retry, which the deadline will start.
    Waiting { attempt: u8 },
    /// Encryption failed: retrying cannot help until the target changes.
    Blocked,
}

struct Lifecycle {
    target: Option<Target>,
    generation: u64,
    phase: Phase,
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self {
            target: None,
            generation: 0,
            phase: Phase::Trying { attempt: 0 },
        }
    }
}

impl Lifecycle {
    fn connect(&mut self, attempt: u8) -> Option<Action> {
        let target = self.target.clone()?;
        self.generation = GENERATION.fetch_add(1, Ordering::Relaxed);
        self.phase = Phase::Trying { attempt };
        Some(Action::Connect {
            target,
            generation: self.generation,
        })
    }

    fn change(&mut self, target: Option<Target>) -> Option<Action> {
        if self.target == target {
            return None;
        }
        self.target = target;
        self.phase = Phase::Trying { attempt: 0 };
        self.connect(0).or(Some(Action::Disconnect))
    }

    fn event(&mut self, event: Event) -> Option<Duration> {
        if self.target.is_none() || event.generation != self.generation {
            return None;
        }
        let (phase, delay) = step(self.phase, event.status);
        self.phase = phase;
        delay
    }

    fn retry(&mut self) -> Option<Action> {
        match self.phase {
            Phase::Waiting { attempt } => self.connect(attempt),
            Phase::Trying { .. } | Phase::Blocked => None,
        }
    }

    fn blocked(&self) -> bool {
        self.phase == Phase::Blocked
    }
}

/// One report for the current attempt: the next phase, and the backoff when a retry is due.
fn step(phase: Phase, status: Status) -> (Phase, Option<Duration>) {
    match (phase, status) {
        (Phase::Blocked, _) | (_, Status::Blocked) => (Phase::Blocked, None),
        (Phase::Trying { .. }, Status::Connected) => (Phase::Trying { attempt: 0 }, None),
        (Phase::Trying { attempt }, Status::Retry) => {
            let delay = Duration::from_millis((1500_u64 << attempt.min(4)).min(15000));
            (
                Phase::Waiting {
                    attempt: attempt.saturating_add(1),
                },
                Some(delay),
            )
        }
        (Phase::Waiting { .. }, Status::Connected | Status::Retry) => (phase, None),
    }
}

pub(super) async fn run(
    mut targets: watch::Receiver<Option<Target>>,
    mut events: mpsc::UnboundedReceiver<Event>,
    mut dispatch: impl FnMut(Action),
) {
    let mut lifecycle = Lifecycle::default();
    let mut deadline = None;
    if let Some(action) = lifecycle.change(targets.borrow_and_update().clone()) {
        dispatch(action);
    }
    loop {
        tokio::select! {
            biased;
            changed = targets.changed() => {
                if changed.is_err() { break; }
                let target = targets.borrow_and_update().clone();
                if let Some(action) = lifecycle.change(target) {
                    deadline = None;
                    dispatch(action);
                }
            }
            event = events.recv() => {
                let Some(event) = event else { break; };
                if let Some(delay) = lifecycle.event(event) {
                    deadline = Some(Instant::now() + delay);
                }
                if lifecycle.blocked() { deadline = None; }
            }
            _ = async {
                match deadline {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            } => {
                deadline = None;
                if let Some(action) = lifecycle.retry() { dispatch(action); }
            }
        }
    }
    dispatch(Action::Disconnect);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(token: &str, epoch: u64) -> Target {
        Target {
            url: "wss://test.invalid".into(),
            token: token.into(),
            voice_epoch: epoch,
            ice_servers: Vec::new(),
        }
    }

    fn generation(action: Option<Action>) -> u64 {
        match action.unwrap() {
            Action::Connect { generation, .. } => generation,
            Action::Disconnect => panic!("expected connect"),
        }
    }

    #[test]
    fn each_report_moves_the_phase_where_the_table_says() {
        use Phase::*;
        let ms = |n| Some(Duration::from_millis(n));
        let cases = [
            (
                Trying { attempt: 0 },
                Status::Connected,
                Trying { attempt: 0 },
                None,
            ),
            (
                Trying { attempt: 3 },
                Status::Connected,
                Trying { attempt: 0 },
                None,
            ),
            (
                Trying { attempt: 0 },
                Status::Retry,
                Waiting { attempt: 1 },
                ms(1500),
            ),
            (
                Trying { attempt: 4 },
                Status::Retry,
                Waiting { attempt: 5 },
                ms(15000),
            ),
            (
                Trying { attempt: 255 },
                Status::Retry,
                Waiting { attempt: 255 },
                ms(15000),
            ),
            (Trying { attempt: 2 }, Status::Blocked, Blocked, None),
            (
                Waiting { attempt: 2 },
                Status::Retry,
                Waiting { attempt: 2 },
                None,
            ),
            (
                Waiting { attempt: 2 },
                Status::Connected,
                Waiting { attempt: 2 },
                None,
            ),
            (Waiting { attempt: 2 }, Status::Blocked, Blocked, None),
            (Blocked, Status::Retry, Blocked, None),
            (Blocked, Status::Connected, Blocked, None),
        ];
        for (from, status, to, delay) in cases {
            assert_eq!(step(from, status), (to, delay), "{from:?} + {status:?}");
        }
    }

    #[test]
    fn the_page_reports_only_the_three_statuses() {
        for (json, ok) in [
            (r#"{"generation":4,"status":"connected"}"#, true),
            (r#"{"generation":4,"status":"retry"}"#, true),
            (r#"{"generation":4,"status":"blocked"}"#, true),
            (r#"{"generation":4,"status":"Connected"}"#, false),
            (r#"{"generation":4,"status":"pending"}"#, false),
        ] {
            assert_eq!(serde_json::from_str::<Event>(json).is_ok(), ok, "{json}");
        }
    }

    #[test]
    fn retry_backoff_is_bounded_deduplicated_and_resets_after_success() {
        let mut lifecycle = Lifecycle::default();
        let mut current = generation(lifecycle.change(Some(target("a", 1))));
        for millis in [1500, 3000, 6000, 12000, 15000, 15000] {
            assert_eq!(
                lifecycle.event(Event {
                    generation: current,
                    status: Status::Retry
                }),
                Some(Duration::from_millis(millis))
            );
            assert!(
                lifecycle
                    .event(Event {
                        generation: current,
                        status: Status::Retry
                    })
                    .is_none()
            );
            let next = generation(lifecycle.retry());
            assert_ne!(next, current);
            current = next;
        }
        lifecycle.event(Event {
            generation: current,
            status: Status::Connected,
        });
        assert_eq!(
            lifecycle.event(Event {
                generation: current,
                status: Status::Retry
            }),
            Some(Duration::from_millis(1500))
        );
    }

    #[test]
    fn leaving_switching_and_encryption_failure_cancel_retries() {
        let mut lifecycle = Lifecycle::default();
        let first = generation(lifecycle.change(Some(target("a", 1))));
        lifecycle.event(Event {
            generation: first,
            status: Status::Retry,
        });
        let second = generation(lifecycle.change(Some(target("b", 2))));
        assert!(lifecycle.retry().is_none());
        assert!(
            lifecycle
                .event(Event {
                    generation: first,
                    status: Status::Retry
                })
                .is_none()
        );
        lifecycle.event(Event {
            generation: second,
            status: Status::Retry,
        });
        lifecycle.event(Event {
            generation: second,
            status: Status::Blocked,
        });
        assert!(lifecycle.retry().is_none());
        assert!(
            lifecycle
                .event(Event {
                    generation: second,
                    status: Status::Retry
                })
                .is_none()
        );
        assert!(matches!(lifecycle.change(None), Some(Action::Disconnect)));
        assert!(lifecycle.retry().is_none());
        assert!(
            lifecycle
                .event(Event {
                    generation: second,
                    status: Status::Connected
                })
                .is_none()
        );
        let third = generation(lifecycle.change(Some(target("b", 3))));
        assert_ne!(second, third);
        assert!(lifecycle.change(Some(target("b", 3))).is_none());
        assert_eq!(
            lifecycle.event(Event {
                generation: third,
                status: Status::Retry
            }),
            Some(Duration::from_millis(1500))
        );
    }

    #[tokio::test]
    async fn actor_retries_current_target_and_stops_when_event_bridge_closes() {
        let (targets, receiver) = watch::channel(Some(target("a", 1)));
        let (events, event_receiver) = mpsc::unbounded_channel();
        let (actions, mut action_receiver) = mpsc::unbounded_channel();
        let task = tokio::spawn(run(receiver, event_receiver, move |action| {
            actions.send(action).unwrap();
        }));
        let first = generation(action_receiver.recv().await);
        events
            .send(Event {
                generation: first,
                status: Status::Retry,
            })
            .unwrap();
        match tokio::time::timeout(Duration::from_secs(3), action_receiver.recv())
            .await
            .unwrap()
            .unwrap()
        {
            Action::Connect {
                target: next,
                generation: second,
            } => {
                assert_eq!(next.token, "a");
                assert_eq!(next.voice_epoch, 1);
                assert_ne!(first, second);
            }
            Action::Disconnect => panic!("expected retry"),
        }
        drop(events);
        assert!(matches!(
            action_receiver.recv().await,
            Some(Action::Disconnect)
        ));
        task.await.unwrap();
        drop(targets);
    }

    #[tokio::test]
    async fn actor_cancels_pending_retry_and_disconnects_when_owner_closes() {
        let (targets, receiver) = watch::channel(Some(target("a", 1)));
        let (events, event_receiver) = mpsc::unbounded_channel();
        let (actions, mut action_receiver) = mpsc::unbounded_channel();
        let task = tokio::spawn(run(receiver, event_receiver, move |action| {
            actions.send(action).unwrap();
        }));
        let first = generation(action_receiver.recv().await);
        events
            .send(Event {
                generation: first,
                status: Status::Retry,
            })
            .unwrap();
        tokio::task::yield_now().await;
        targets.send_replace(None);
        assert!(matches!(
            action_receiver.recv().await,
            Some(Action::Disconnect)
        ));
        assert!(
            tokio::time::timeout(Duration::from_millis(1600), action_receiver.recv())
                .await
                .is_err()
        );
        drop(targets);
        assert!(matches!(
            action_receiver.recv().await,
            Some(Action::Disconnect)
        ));
        task.await.unwrap();
    }
}
