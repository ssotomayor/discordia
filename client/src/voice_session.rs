//! A guild voice call on this client: which call it is (`VoiceCall`).

use crate::protocol::Id;

/// The wire's route revision once a self-host's session fell back to the shared SFU;
/// its bundled SFU is 0. The revision never goes back within a session (trap 26).
pub const SHARED_SFU_ROUTE: u32 = 1;

/// A guild call as the server has us in it: the channel, and the route of its latest grant.
/// Every check after an await compares one, because the call may have moved, ended or rerouted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VoiceCall {
    pub channel: Id,
    pub route: u32,
}

impl VoiceCall {
    pub fn on_shared_sfu(self) -> bool {
        self.route >= SHARED_SFU_ROUTE
    }

    pub fn is_current(self, current: Option<VoiceCall>) -> bool {
        current == Some(self)
    }

    /// A grant for an older route than the session's is a late answer to a request
    /// the fallback already replaced, so it must not dial the abandoned SFU.
    pub fn grant_is_stale(self, current: Option<VoiceCall>) -> bool {
        current.is_none_or(|c| c.channel != self.channel || self.route < c.route)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(channel: u128, route: u32) -> VoiceCall {
        VoiceCall {
            channel: Id::from_u128(channel),
            route,
        }
    }

    #[test]
    fn a_call_is_current_only_while_channel_and_route_both_match() {
        for (held, current, expected) in [
            (call(1, 0), Some(call(1, 0)), true),
            (call(1, 0), None, false),
            (call(1, 0), Some(call(2, 0)), false),
            (call(1, 0), Some(call(1, 1)), false),
            (call(1, 1), Some(call(1, 0)), false),
        ] {
            assert_eq!(
                held.is_current(current),
                expected,
                "{held:?} vs {current:?}"
            );
        }
    }

    #[test]
    fn a_grant_is_stale_for_another_channel_or_an_older_route() {
        for (grant, current, stale) in [
            (call(1, 0), Some(call(1, 0)), false),
            (call(1, 1), Some(call(1, 0)), false),
            (call(1, 0), Some(call(1, 1)), true),
            (call(1, 0), Some(call(2, 0)), true),
            (call(1, 0), None, true),
        ] {
            assert_eq!(
                grant.grant_is_stale(current),
                stale,
                "{grant:?} vs {current:?}"
            );
        }
    }

    #[test]
    fn only_a_shared_route_is_the_fallback() {
        assert!(!call(1, 0).on_shared_sfu());
        assert!(call(1, SHARED_SFU_ROUTE).on_shared_sfu());
    }
}
