//! Whether the server should take this client as watched. It assumes so until
//! told otherwise, so an unfocused desktop must say it, or the phone stays quiet.

use std::time::Duration;

use tokio::time::Instant;

/// Alt-tabbing and moving to the Social window flap focus; this outlasts a flap.
pub const DEBOUNCE: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Poll {
    Send(bool),
    At(Instant),
    Idle,
}

#[derive(Debug, Clone)]
pub struct Gate {
    /// What the server believes; `None` outside Ready.
    told: Option<bool>,
    focused: bool,
    since: Instant,
}

impl Gate {
    pub fn new(focused: bool, now: Instant) -> Self {
        Self {
            told: None,
            focused,
            since: now,
        }
    }

    /// A fresh Ready: the server starts at attended, so only `false` is news,
    /// and it is sent at once since nothing is flapping yet.
    pub fn ready(&mut self) -> Option<bool> {
        self.told = Some(self.focused);
        (!self.focused).then_some(false)
    }

    pub fn lost(&mut self) {
        self.told = None;
    }

    pub fn focus(&mut self, focused: bool, now: Instant) {
        if focused != self.focused {
            self.focused = focused;
            self.since = now;
        }
    }

    pub fn poll(&mut self, now: Instant) -> Poll {
        match self.told {
            Some(told) if told != self.focused => {
                let due = self.since + DEBOUNCE;
                if now >= due {
                    self.told = Some(self.focused);
                    Poll::Send(self.focused)
                } else {
                    Poll::At(due)
                }
            }
            _ => Poll::Idle,
        }
    }
}

/// Longer than `DEBOUNCE`: each change here is a request against the relay's per-address limit.
pub const HOLD_AFTER: Duration = Duration::from_secs(2);
pub const RELEASE_AFTER: Duration = Duration::from_secs(5);
const RENEW: Duration = Duration::from_secs(crate::protocol::push::ATTENTION_RENEW_SECS);
/// An account with no phone asks again this seldom, so most desktops barely call the relay.
pub const NO_DEVICE_RECHECK: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Hold,
    Release,
    At(Instant),
    Idle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Held,
    NoDevice,
    Failed,
}

/// What a desktop in front holds at the push relay so the phone stays quiet for DMs.
/// A DM has no server to tell (`Gate`), so the relay that wakes the phone is told instead.
#[derive(Debug, Clone)]
pub struct Lease {
    focused: bool,
    since: Instant,
    /// Whether the relay may still hold one of ours.
    held: bool,
    /// The next ask while focused: a renewal, a retry or the recheck.
    next: Instant,
}

impl Lease {
    pub fn new(focused: bool, now: Instant) -> Self {
        Self {
            focused,
            since: now,
            held: false,
            next: now,
        }
    }

    pub fn held(&self) -> bool {
        self.held
    }

    pub fn focus(&mut self, focused: bool, now: Instant) {
        if focused != self.focused {
            self.focused = focused;
            self.since = now;
        }
    }

    /// `Hold` repeats until `answered` moves the next ask.
    pub fn poll(&mut self, now: Instant) -> Step {
        if self.focused {
            let due = self.next.max(self.since + HOLD_AFTER);
            return if now >= due {
                Step::Hold
            } else {
                Step::At(due)
            };
        }
        if !self.held {
            return Step::Idle;
        }
        let due = self.since + RELEASE_AFTER;
        if now < due {
            return Step::At(due);
        }
        self.held = false;
        self.next = now;
        Step::Release
    }

    pub fn answered(&mut self, answer: Answer, now: Instant) {
        match answer {
            Answer::Held => {
                self.held = true;
                self.next = now + RENEW;
            }
            Answer::NoDevice => {
                self.held = false;
                self.next = now + NO_DEVICE_RECHECK;
            }
            // Whatever the relay held before still lapses or is released on its own terms.
            Answer::Failed => self.next = now + RENEW,
        }
    }
}

/// Ends, releasing what it holds, when the account's view drops the sender.
pub async fn keep_lease(
    relay: super::registration::RelayClient,
    identity: crate::identity::Identity,
    mut focused: tokio::sync::watch::Receiver<bool>,
) {
    let lease = Lease::new(*focused.borrow_and_update(), Instant::now());
    run_lease(lease, relay, identity, focused).await;
}

pub(crate) async fn run_lease(
    mut lease: Lease,
    relay: super::registration::RelayClient,
    identity: crate::identity::Identity,
    mut focused: tokio::sync::watch::Receiver<bool>,
) {
    loop {
        let wake = match lease.poll(Instant::now()) {
            Step::Hold => {
                let answer = match relay.hold_attention(&identity).await {
                    Ok(true) => Answer::Held,
                    Ok(false) => Answer::NoDevice,
                    Err(error) => {
                        tracing::debug!(%error, "push: the relay was not told this desktop is in front");
                        Answer::Failed
                    }
                };
                lease.answered(answer, Instant::now());
                continue;
            }
            Step::Release => {
                if let Err(error) = relay.release_attention(&identity).await {
                    tracing::debug!(%error, "push: the lease was not released and will lapse");
                }
                continue;
            }
            Step::At(at) => Some(at),
            Step::Idle => None,
        };
        let sleep = async move {
            match wake {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            _ = sleep => {}
            changed = focused.changed() => if changed.is_err() {
                break;
            },
        }
        lease.focus(*focused.borrow_and_update(), Instant::now());
    }
    if lease.held() {
        let _ = relay.release_attention(&identity).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_desktop_in_front_holds_renews_and_lets_go() {
        let t0 = Instant::now();
        let mut lease = Lease::new(true, t0);
        assert_eq!(lease.poll(t0), Step::At(t0 + HOLD_AFTER));
        assert_eq!(lease.poll(t0 + HOLD_AFTER), Step::Hold);
        lease.answered(Answer::Held, t0 + HOLD_AFTER);
        let renew = t0 + HOLD_AFTER + RENEW;
        assert_eq!(lease.poll(t0 + HOLD_AFTER), Step::At(renew));
        assert_eq!(lease.poll(renew), Step::Hold);
        lease.answered(Answer::Held, renew);

        lease.focus(false, renew);
        assert_eq!(lease.poll(renew), Step::At(renew + RELEASE_AFTER));
        assert_eq!(lease.poll(renew + RELEASE_AFTER), Step::Release);
        assert!(!lease.held());
        assert_eq!(lease.poll(renew + RELEASE_AFTER * 2), Step::Idle);

        let back = renew + RELEASE_AFTER * 3;
        lease.focus(true, back);
        assert_eq!(lease.poll(back), Step::At(back + HOLD_AFTER), "asked anew");
    }

    #[test]
    fn a_glance_away_neither_releases_nor_asks_again() {
        let t0 = Instant::now();
        let mut lease = Lease::new(true, t0);
        lease.answered(Answer::Held, t0);
        let ms = Duration::from_millis;
        for step in 0..10 {
            lease.focus(step % 2 == 1, t0 + ms(3000 + step * 100));
        }
        assert_eq!(
            lease.poll(t0 + ms(4500)),
            Step::At(t0 + RENEW),
            "ended focused"
        );
        assert!(lease.held());
    }

    #[test]
    fn an_account_without_a_phone_is_asked_about_seldom() {
        let t0 = Instant::now();
        let mut lease = Lease::new(true, t0);
        lease.answered(Answer::NoDevice, t0);
        lease.focus(false, t0 + RELEASE_AFTER);
        assert_eq!(
            lease.poll(t0 + RELEASE_AFTER * 3),
            Step::Idle,
            "nothing to release"
        );
        lease.focus(true, t0 + RELEASE_AFTER * 4);
        assert_eq!(
            lease.poll(t0 + RELEASE_AFTER * 4 + HOLD_AFTER),
            Step::At(t0 + NO_DEVICE_RECHECK),
            "coming back to the window is not a reason to ask"
        );
        assert_eq!(lease.poll(t0 + NO_DEVICE_RECHECK), Step::Hold);
    }

    #[test]
    fn a_failed_ask_is_retried_and_keeps_what_was_held() {
        let t0 = Instant::now();
        let mut lease = Lease::new(true, t0);
        lease.answered(Answer::Failed, t0);
        assert!(!lease.held());
        assert_eq!(lease.poll(t0 + HOLD_AFTER), Step::At(t0 + RENEW));
        lease.answered(Answer::Held, t0 + RENEW);
        lease.answered(Answer::Failed, t0 + RENEW * 2);
        assert!(lease.held(), "the relay may still hold the last one");
        lease.focus(false, t0 + RENEW * 2);
        assert_eq!(lease.poll(t0 + RENEW * 2 + RELEASE_AFTER), Step::Release);
    }

    #[test]
    fn only_an_unfocused_ready_is_news() {
        let t0 = Instant::now();
        let mut gate = Gate::new(true, t0);
        assert_eq!(gate.poll(t0), Poll::Idle, "not Ready: nobody to tell");
        assert_eq!(gate.ready(), None);
        assert_eq!(gate.poll(t0), Poll::Idle);

        let mut gate = Gate::new(false, t0);
        assert_eq!(gate.ready(), Some(false));
        assert_eq!(gate.poll(t0 + DEBOUNCE * 5), Poll::Idle);
        gate.lost();
        assert_eq!(gate.ready(), Some(false), "every Ready starts over");
    }

    #[test]
    fn a_flap_shorter_than_the_debounce_sends_nothing() {
        let t0 = Instant::now();
        let mut gate = Gate::new(true, t0);
        gate.ready();
        let ms = Duration::from_millis;
        gate.focus(false, t0 + ms(100));
        assert_eq!(gate.poll(t0 + ms(100)), Poll::At(t0 + ms(1100)));
        gate.focus(true, t0 + ms(400));
        assert_eq!(gate.poll(t0 + ms(1100)), Poll::Idle);
        for step in 0..10 {
            gate.focus(step % 2 == 1, t0 + ms(500 + step * 50));
        }
        assert_eq!(gate.poll(t0 + ms(5000)), Poll::Idle, "ended focused");
    }

    #[test]
    fn a_settled_change_is_sent_once_within_the_debounce() {
        let t0 = Instant::now();
        let mut gate = Gate::new(true, t0);
        gate.ready();
        gate.focus(false, t0);
        assert_eq!(gate.poll(t0 + DEBOUNCE), Poll::Send(false));
        assert_eq!(gate.poll(t0 + DEBOUNCE * 2), Poll::Idle);
        gate.focus(true, t0 + DEBOUNCE * 3);
        assert_eq!(gate.poll(t0 + DEBOUNCE * 3), Poll::At(t0 + DEBOUNCE * 4));
        assert_eq!(gate.poll(t0 + DEBOUNCE * 4), Poll::Send(true));
        gate.lost();
        gate.focus(false, t0 + DEBOUNCE * 5);
        assert_eq!(gate.poll(t0 + DEBOUNCE * 9), Poll::Idle, "not Ready");
    }
}
