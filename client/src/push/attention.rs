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

#[cfg(test)]
mod tests {
    use super::*;

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
