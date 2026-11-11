//! The gateway task's own view of its connection. The task steps it and
//! publishes `ConnectionStatus` from it; nothing reads the UI field back.

use crate::state::ConnectionStatus;

/// Whether this session has been in before. Before the first Ready a failure
/// ends the session; after it, the task redials.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Try {
    First,
    /// A redial after this many failed attempts since the session was last Live.
    Again(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Link {
    /// Starting the embedded host or looking a code up.
    Resolving,
    Dialing(Try),
    AwaitHello(Try),
    AwaitReady(Try),
    Live,
    /// Waiting `reconnect_delay(n)` before the next redial.
    Backoff(u32),
    /// A leave asked the task to stop; the socket is closing.
    Closing,
    /// The task returned; `spawn_gateway` publishes it.
    Ended,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Event {
    Resolved,
    Connected,
    Hello,
    Ready,
    Failed(Refusal),
    Waited,
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Refusal {
    Transient,
    /// The server said retrying cannot change its answer.
    Final,
}

impl Link {
    pub(super) fn step(self, event: Event) -> Link {
        use Link::*;
        match (self, event) {
            (Ended, _) => Ended,
            (Closing, _) | (_, Event::Shutdown) => Closing,
            (Resolving, Event::Resolved) => Dialing(Try::First),
            (Dialing(attempt), Event::Connected) => AwaitHello(attempt),
            (AwaitHello(attempt), Event::Hello) => AwaitReady(attempt),
            (AwaitReady(_), Event::Ready) => Live,
            (Backoff(n), Event::Waited) => Dialing(Try::Again(n)),
            (_, Event::Failed(Refusal::Final)) => Ended,
            (Live, Event::Failed(_)) => Backoff(0),
            (
                Dialing(Try::Again(n)) | AwaitHello(Try::Again(n)) | AwaitReady(Try::Again(n)),
                Event::Failed(_),
            ) => Backoff(n.saturating_add(1)),
            (Resolving | Dialing(_) | AwaitHello(_) | AwaitReady(_), Event::Failed(_)) => Ended,
            (unchanged, _) => unchanged,
        }
    }

    /// What the UI is told. `None` keeps the last status through the close
    /// handshake, so a leave does not flash the session as lost.
    pub(super) fn status(self) -> Option<ConnectionStatus> {
        use Link::*;
        match self {
            Resolving | Dialing(Try::First) | AwaitHello(Try::First) | AwaitReady(Try::First) => {
                Some(ConnectionStatus::Connecting)
            }
            Dialing(Try::Again(_))
            | AwaitHello(Try::Again(_))
            | AwaitReady(Try::Again(_))
            | Backoff(_) => Some(ConnectionStatus::Reconnecting),
            Live => Some(ConnectionStatus::Ready),
            Closing => None,
            Ended => Some(ConnectionStatus::Disconnected),
        }
    }

    /// A Ready that restores a session keeps what was selected in it.
    pub(super) fn recovering(self) -> bool {
        matches!(self, Link::AwaitReady(Try::Again(_)))
    }
}

/// What the server said when it refused the session, in its own words. Only
/// the server's refusals carry the `gateway admission:` prefix.
pub(super) fn admission_refusal(reason: &str) -> Option<&str> {
    let refusal = reason.strip_prefix("gateway admission:")?;
    Some(refusal.lines().next().unwrap_or(refusal).trim())
}

/// The server's own words about why a session ended, if it gave any.
pub(super) fn server_words(reason: &str) -> Option<&str> {
    admission_refusal(reason).or_else(|| {
        reason
            .lines()
            .find_map(|line| line.strip_prefix("Server reason: "))
            .filter(|said| *said != "not received")
    })
}

pub(super) fn refusal(reason: &str) -> Refusal {
    let rejected = discordia_server::gateway::IDENTIFY_REJECTED;
    match admission_refusal(reason) {
        Some(said) if said.starts_with(rejected) => Refusal::Final,
        _ => Refusal::Transient,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Event::*;
    use Link::*;

    const TRANSIENT: Event = Failed(Refusal::Transient);
    const FINAL: Event = Failed(Refusal::Final);

    #[test]
    fn every_transition() {
        let again = Try::Again(2);
        let table = [
            (Resolving, Resolved, Dialing(Try::First)),
            (Resolving, TRANSIENT, Ended),
            (Resolving, Shutdown, Closing),
            (Dialing(Try::First), Connected, AwaitHello(Try::First)),
            (Dialing(Try::First), TRANSIENT, Ended),
            (Dialing(again), Connected, AwaitHello(again)),
            (Dialing(again), TRANSIENT, Backoff(3)),
            (Dialing(again), FINAL, Ended),
            (AwaitHello(Try::First), Hello, AwaitReady(Try::First)),
            (AwaitHello(Try::First), TRANSIENT, Ended),
            (AwaitHello(again), TRANSIENT, Backoff(3)),
            (AwaitReady(Try::First), Ready, Live),
            (AwaitReady(Try::First), FINAL, Ended),
            (AwaitReady(again), Ready, Live),
            (AwaitReady(again), TRANSIENT, Backoff(3)),
            (AwaitReady(again), FINAL, Ended),
            (Live, TRANSIENT, Backoff(0)),
            (Live, FINAL, Ended),
            (Live, Shutdown, Closing),
            (Backoff(0), Waited, Dialing(Try::Again(0))),
            (Backoff(3), Shutdown, Closing),
            (Backoff(u32::MAX), Waited, Dialing(Try::Again(u32::MAX))),
            (Dialing(Try::Again(u32::MAX)), TRANSIENT, Backoff(u32::MAX)),
            (Closing, TRANSIENT, Closing),
            (Closing, Ready, Closing),
            (Closing, Shutdown, Closing),
            (Ended, Resolved, Ended),
            (Ended, Shutdown, Ended),
            // Out of order events change nothing.
            (Resolving, Ready, Resolving),
            (Dialing(Try::First), Hello, Dialing(Try::First)),
            (Live, Waited, Live),
            (Backoff(1), Connected, Backoff(1)),
        ];
        for (from, event, to) in table {
            assert_eq!(from.step(event), to, "{from:?} + {event:?}");
        }
    }

    #[test]
    fn published_status() {
        use ConnectionStatus::*;
        let table = [
            (Resolving, Some(Connecting)),
            (Dialing(Try::First), Some(Connecting)),
            (AwaitHello(Try::First), Some(Connecting)),
            (AwaitReady(Try::First), Some(Connecting)),
            (Dialing(Try::Again(0)), Some(Reconnecting)),
            (AwaitHello(Try::Again(0)), Some(Reconnecting)),
            (AwaitReady(Try::Again(0)), Some(Reconnecting)),
            (Backoff(4), Some(Reconnecting)),
            (Live, Some(Ready)),
            (Closing, None),
            (Ended, Some(Disconnected)),
        ];
        for (link, status) in table {
            assert_eq!(link.status(), status, "{link:?}");
        }
        assert!(AwaitReady(Try::Again(0)).recovering());
        assert!(!AwaitReady(Try::First).recovering());
    }

    #[test]
    fn only_a_rejected_login_is_final() {
        let table = [
            (
                "gateway admission: identify rejected: signature did not verify",
                Refusal::Final,
            ),
            (
                "gateway admission: identify rejected: this server does not answer to 'x'\nQUIC close: not received",
                Refusal::Final,
            ),
            (
                "gateway admission: this key already has 8 open sessions",
                Refusal::Transient,
            ),
            ("gateway admission: this server is full", Refusal::Transient),
            (
                "gateway admission: too many connections from your address",
                Refusal::Transient,
            ),
            (
                "gateway admission: the host stopped this server",
                Refusal::Transient,
            ),
            ("connect failed: connection refused", Refusal::Transient),
            // Words a client could be sent mid-session are not an admission.
            (
                "gateway send: stopped\nServer reason: identify rejected: x",
                Refusal::Transient,
            ),
            ("identify rejected: no prefix", Refusal::Transient),
        ];
        for (reason, expected) in table {
            assert_eq!(refusal(reason), expected, "{reason}");
        }
    }

    #[test]
    fn the_servers_words_are_told_from_ours() {
        let table = [
            (
                "gateway admission: this key already has 8 open sessions",
                Some("this key already has 8 open sessions"),
            ),
            (
                "gateway send: x\nOperation: y\nServer reason: message too large",
                Some("message too large"),
            ),
            ("gateway send: x\nServer reason: not received", None),
            ("server closed before Hello", None),
            ("recv: connection reset", None),
        ];
        for (reason, said) in table {
            assert_eq!(server_words(reason), said, "{reason}");
        }
    }
}
