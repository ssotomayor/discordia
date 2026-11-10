//! A tapped mention notification: open its channel, going to its server first
//! when the app is elsewhere, but never at the cost of a voice call.

use std::sync::OnceLock;

use tokio::sync::watch;

use crate::known_servers::KnownServer;
use crate::protocol::Id;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MentionTap {
    pub account: String,
    /// The server's dial origin, as `push.json` and `Alert::Mention` spell it.
    pub origin: String,
    pub guild: Id,
    pub channel: Id,
    pub message: Id,
}

/// Held by the account until it lands. `launched` once a connection or a
/// switch was started for it, so a failed one is not started again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingTap {
    pub tap: MentionTap,
    pub launched: bool,
}

pub(crate) fn taps() -> &'static watch::Sender<Option<MentionTap>> {
    static TAPS: OnceLock<watch::Sender<Option<MentionTap>>> = OnceLock::new();
    TAPS.get_or_init(|| watch::channel(None).0)
}

/// For the platform layer, when a mention notification is tapped. Kept until
/// an account is mounted to take it, as a cold start opens the app first.
pub fn open_mention(tap: MentionTap) {
    if crate::protocol::is_canonical_pubkey(&tap.account) && !tap.origin.is_empty() {
        taps().send_replace(Some(tap));
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Here<'a> {
    pub account: &'a str,
    pub at_home: bool,
    /// `AppState::server_origin` of the mounted session.
    pub live_origin: Option<&'a str>,
    pub ready: bool,
    /// In or joining a guild voice call; a switch would end it.
    pub in_call: bool,
    /// The server rail is already asking or confirming a switch.
    pub switching: bool,
    /// The saved server last seen at the tap's origin.
    pub known: Option<&'a KnownServer>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Route {
    Wait,
    Drop,
    Select,
    /// From Home, the way the saved servers connect.
    Connect(KnownServer),
    /// Through the server rail, which tears down in order (trap 17).
    Switch(KnownServer),
    /// Going there would end the call; stay put.
    StayInCall,
}

pub fn route(pending: &PendingTap, here: &Here<'_>) -> Route {
    let tap = &pending.tap;
    if tap.account != here.account {
        return Route::Drop;
    }
    if here.live_origin == Some(tap.origin.as_str()) {
        return if here.ready {
            Route::Select
        } else {
            Route::Wait
        };
    }
    if pending.launched {
        // Landed elsewhere: the switch was refused, undone or failed.
        return if here.at_home || here.switching || !here.ready {
            Route::Wait
        } else {
            Route::Drop
        };
    }
    if !here.at_home && !here.ready {
        return Route::Wait;
    }
    let Some(known) = here.known else {
        return Route::Drop;
    };
    if here.at_home {
        Route::Connect(known.clone())
    } else if here.in_call {
        Route::StayInCall
    } else {
        Route::Switch(known.clone())
    }
}

/// `known_servers` records an origin at each Ready; a tap names one.
pub fn known_at<'a>(known: &'a [KnownServer], origin: &str) -> Option<&'a KnownServer> {
    known
        .iter()
        .find(|server| server.origin.as_deref() == Some(origin))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::SessionMode;

    const ME: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const THERE: &str = "chat.example:443";
    const HERE: &str = "quic:hostkey";

    fn pending(launched: bool) -> PendingTap {
        PendingTap {
            tap: MentionTap {
                account: ME.into(),
                origin: THERE.into(),
                guild: Id::from_u128(1),
                channel: Id::from_u128(2),
                message: Id::from_u128(3),
            },
            launched,
        }
    }

    fn server() -> KnownServer {
        KnownServer {
            origin: Some(THERE.into()),
            ..KnownServer::new(SessionMode::Remote {
                server_url: "wss://chat.example".into(),
            })
        }
    }

    fn elsewhere(known: Option<&KnownServer>) -> Here<'_> {
        Here {
            account: ME,
            at_home: false,
            live_origin: Some(HERE),
            ready: true,
            in_call: false,
            switching: false,
            known,
        }
    }

    #[test]
    fn the_same_server_selects_once_it_is_ready() {
        let known = server();
        let here = Here {
            live_origin: Some(THERE),
            ..elsewhere(Some(&known))
        };
        assert_eq!(route(&pending(false), &here), Route::Select);
        assert_eq!(route(&pending(true), &here), Route::Select);
        let connecting = Here {
            ready: false,
            ..here
        };
        assert_eq!(route(&pending(true), &connecting), Route::Wait);
        let in_call = Here {
            in_call: true,
            ..here
        };
        assert_eq!(
            route(&pending(false), &in_call),
            Route::Select,
            "a channel on the call's own server ends nothing"
        );
    }

    #[test]
    fn another_known_server_is_reached_the_way_the_app_reaches_it() {
        let known = server();
        assert_eq!(
            route(&pending(false), &elsewhere(Some(&known))),
            Route::Switch(known.clone())
        );
        let home = Here {
            at_home: true,
            live_origin: None,
            ready: false,
            ..elsewhere(Some(&known))
        };
        assert_eq!(route(&pending(false), &home), Route::Connect(known.clone()));
        assert_eq!(route(&pending(true), &home), Route::Wait);
    }

    #[test]
    fn a_voice_call_is_never_ended_by_a_tap() {
        let known = server();
        let here = Here {
            in_call: true,
            ..elsewhere(Some(&known))
        };
        assert_eq!(route(&pending(false), &here), Route::StayInCall);
    }

    #[test]
    fn unknown_servers_and_other_accounts_are_dropped() {
        assert_eq!(route(&pending(false), &elsewhere(None)), Route::Drop);
        let known = server();
        let other = Here {
            account: "b",
            ..elsewhere(Some(&known))
        };
        assert_eq!(route(&pending(false), &other), Route::Drop);
        assert_eq!(known_at(std::slice::from_ref(&known), THERE), Some(&known));
        assert_eq!(known_at(&[known], HERE), None);
    }

    #[test]
    fn a_launched_tap_waits_for_the_switch_and_drops_where_it_failed() {
        let known = server();
        let asking = Here {
            switching: true,
            ..elsewhere(Some(&known))
        };
        assert_eq!(route(&pending(true), &asking), Route::Wait);
        let underway = Here {
            live_origin: None,
            ready: false,
            ..elsewhere(Some(&known))
        };
        assert_eq!(route(&pending(true), &underway), Route::Wait);
        assert_eq!(
            route(&pending(true), &elsewhere(Some(&known))),
            Route::Drop,
            "back where it started: refused, unreachable or undone"
        );
        let settling = Here {
            ready: false,
            ..elsewhere(Some(&known))
        };
        assert_eq!(
            route(&pending(false), &settling),
            Route::Wait,
            "a session still settling is left to settle first"
        );
    }
}
