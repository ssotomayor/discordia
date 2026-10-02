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
}

#[derive(serde::Deserialize)]
pub(super) struct Event {
    pub generation: u64,
    pub status: Status,
}

#[derive(serde::Deserialize)]
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

#[derive(Default)]
struct Lifecycle {
    target: Option<Target>,
    generation: u64,
    attempt: u8,
    pending: bool,
    blocked: bool,
}

impl Lifecycle {
    fn connect(&mut self) -> Option<Action> {
        let target = self.target.clone()?;
        self.generation = GENERATION.fetch_add(1, Ordering::Relaxed);
        self.pending = false;
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
        self.attempt = 0;
        self.pending = false;
        self.blocked = false;
        self.connect().or(Some(Action::Disconnect))
    }

    fn event(&mut self, event: Event) -> Option<Duration> {
        if self.target.is_none() || event.generation != self.generation || self.blocked {
            return None;
        }
        match event.status {
            Status::Connected if !self.pending => self.attempt = 0,
            Status::Retry if !self.pending => {
                self.pending = true;
                let delay = Duration::from_millis((1500_u64 << self.attempt.min(4)).min(15000));
                self.attempt = self.attempt.saturating_add(1);
                return Some(delay);
            }
            Status::Blocked => {
                self.blocked = true;
                self.pending = false;
            }
            _ => {}
        }
        None
    }

    fn retry(&mut self) -> Option<Action> {
        if !self.pending || self.blocked {
            return None;
        }
        self.connect()
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
                if lifecycle.blocked { deadline = None; }
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
        }
    }

    fn generation(action: Option<Action>) -> u64 {
        match action.unwrap() {
            Action::Connect { generation, .. } => generation,
            Action::Disconnect => panic!("expected connect"),
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
