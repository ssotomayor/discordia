//! A guild voice call on this client: which call it is (`VoiceCall`) and where it
//! stands (`CallState`, changed only by `VoiceSession::on`).

use crate::protocol::{ClientMessage, Id};

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

/// What the UI draws: the banner's dot, the status line, the join and leave sounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoicePhase {
    Idle,
    Connecting,
    Connected,
    Error,
}

/// A join sent but not yet answered is `VoiceSession::joining_channel_id`, beside this:
/// a move dials one channel while still connected to another.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum CallState {
    #[default]
    Idle,
    /// A grant for this channel is in hand and its room is being dialled.
    Dialing(Id),
    Connected(Id),
    /// LiveKit lost the room's transport and is resuming it on its own.
    Reconnecting(Id),
    /// The room is being rebuilt on the same grant to apply a device change.
    Restarting(Id),
    Failed(String),
}

impl CallState {
    pub fn channel(&self) -> Option<Id> {
        match self {
            Self::Dialing(c) | Self::Connected(c) | Self::Reconnecting(c) | Self::Restarting(c) => {
                Some(*c)
            }
            Self::Idle | Self::Failed(_) => None,
        }
    }

    /// A restart keeps the call and its sounds: the person changed a device, not the call.
    pub fn phase(&self) -> VoicePhase {
        match self {
            Self::Idle => VoicePhase::Idle,
            Self::Dialing(_) | Self::Reconnecting(_) => VoicePhase::Connecting,
            Self::Connected(_) | Self::Restarting(_) => VoicePhase::Connected,
            Self::Failed(_) => VoicePhase::Error,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceEvent {
    JoinSent(Id),
    LeaveSent,
    /// The server answered with an error, which may have been our join's.
    JoinRefused,
    /// The session the join was sent on ended; no server is left to answer it.
    JoinAbandoned,
    /// The server has us in this channel.
    Placed(Id),
    Granted(Id),
    RoomUp(Id),
    RoomReconnecting(Id),
    RoomReconnected(Id),
    /// The room ended without the server telling us the call did.
    RoomLost {
        channel: Id,
        error: String,
    },
    RestartBegun(Id),
    Failed {
        channel: Id,
        error: String,
    },
    Ended,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceSession {
    call: CallState,
    joining: Option<Id>,
    /// Kept apart from a failed connect: devices are re-checked every tick while a room is up.
    device_error: Option<String>,
    /// Private, so deafened without muted cannot be built: `change_mute` keeps the pair.
    muted: bool,
    deafened: bool,
    muted_before_deafen: bool,
    pub speaking: bool,
}

impl Default for VoiceSession {
    fn default() -> Self {
        Self {
            call: CallState::Idle,
            joining: None,
            device_error: None,
            muted: false,
            deafened: false,
            muted_before_deafen: false,
            speaking: false,
        }
    }
}

impl VoiceSession {
    pub fn call(&self) -> &CallState {
        &self.call
    }

    pub fn phase(&self) -> VoicePhase {
        self.call.phase()
    }

    pub fn channel_id(&self) -> Option<Id> {
        self.call.channel()
    }

    pub fn joining_channel_id(&self) -> Option<Id> {
        self.joining
    }

    pub fn connected_channel(&self) -> Option<Id> {
        match self.call {
            CallState::Connected(c) | CallState::Restarting(c) => Some(c),
            _ => None,
        }
    }

    /// Whether this client's room for `channel` is the call: a room lost while dialling
    /// or restarting was never, or is no longer, the one in use.
    pub fn holds_room(&self, channel: Id) -> bool {
        matches!(self.call, CallState::Connected(c) | CallState::Reconnecting(c) if c == channel)
    }

    /// The banner's line: why the call failed, else which device is missing.
    pub fn error(&self) -> Option<&str> {
        match &self.call {
            CallState::Failed(error) => Some(error),
            _ => self.device_error.as_deref(),
        }
    }

    pub fn device_error(&self) -> Option<&str> {
        self.device_error.as_deref()
    }

    pub fn set_device_error(&mut self, error: Option<String>) -> bool {
        let changed = self.device_error != error;
        self.device_error = error;
        changed
    }

    /// Lets a task skip writing the signal, which would wake every reader for nothing.
    pub fn would_change(&self, event: &VoiceEvent) -> bool {
        self.clone().on(event.clone())
    }

    /// The only writer of the call state and the pending join. Returns whether
    /// anything changed; for `Placed`, that is whether it answered our pending join.
    pub fn on(&mut self, event: VoiceEvent) -> bool {
        use CallState::*;
        let before = (self.call.clone(), self.joining);
        match event {
            VoiceEvent::JoinSent(channel) => self.joining = Some(channel),
            VoiceEvent::LeaveSent | VoiceEvent::JoinRefused | VoiceEvent::JoinAbandoned => {
                self.joining = None
            }
            VoiceEvent::Placed(channel) => {
                if self.joining == Some(channel) {
                    self.joining = None;
                }
            }
            VoiceEvent::Granted(channel) => self.call = Dialing(channel),
            VoiceEvent::RoomUp(channel) => {
                if matches!(self.call, Dialing(c) | Restarting(c) if c == channel) {
                    self.call = Connected(channel);
                }
            }
            VoiceEvent::RoomReconnecting(channel) => {
                if self.call == Connected(channel) {
                    self.call = Reconnecting(channel);
                }
            }
            VoiceEvent::RoomReconnected(channel) => {
                if self.call == Reconnecting(channel) {
                    self.call = Connected(channel);
                }
            }
            VoiceEvent::RoomLost { channel, error } => {
                if matches!(self.call, Connected(c) | Reconnecting(c) if c == channel) {
                    self.call = Failed(error);
                }
            }
            VoiceEvent::RestartBegun(channel) => {
                if matches!(self.call, Connected(c) | Reconnecting(c) if c == channel) {
                    self.call = Restarting(channel);
                }
            }
            VoiceEvent::Failed { channel, error } => {
                if matches!(self.call, Dialing(c) | Restarting(c) if c == channel) {
                    self.call = Failed(error);
                }
            }
            VoiceEvent::Ended => {
                self.call = Idle;
                self.device_error = None;
            }
        }
        (&self.call, self.joining) != (&before.0, before.1)
    }

    pub fn join_message(&self, channel_id: Id) -> ClientMessage {
        ClientMessage::JoinVoice {
            channel_id,
            preferences: Some(crate::protocol::VoicePreferences {
                muted: self.muted || self.deafened,
                deafened: self.deafened,
            }),
        }
    }

    pub fn muted(&self) -> bool {
        self.muted
    }

    pub fn deafened(&self) -> bool {
        self.deafened
    }

    /// The one place our mute or deafen changes. `None` when nothing did, so
    /// a repeated press sends nothing; else what the server and audio must be told.
    pub fn change_mute(&mut self, change: MuteChange) -> Option<MuteFollowup> {
        let before = (self.muted, self.deafened);
        let mut tell_server = true;
        match change {
            MuteChange::ToggleMute if !self.deafened => self.muted = !self.muted,
            MuteChange::SetMuted(muted) if !self.deafened => self.muted = muted,
            MuteChange::ToggleMute | MuteChange::SetMuted(_) => {}
            MuteChange::ToggleDeafen => self.deafen(!self.deafened),
            MuteChange::SetDeafened(deafened) => self.deafen(deafened),
            MuteChange::Unmute => {
                self.deafened = false;
                self.muted = false;
            }
            MuteChange::Server { muted, deafened } => {
                tell_server = false;
                self.deafened = deafened;
                self.muted = muted || deafened;
            }
        }
        ((self.muted, self.deafened) != before).then_some(MuteFollowup {
            muted: self.muted,
            deafened: self.deafened,
            tell_server,
        })
    }

    fn deafen(&mut self, on: bool) {
        if on == self.deafened {
            return;
        }
        if on {
            self.muted_before_deafen = self.muted;
            self.muted = true;
        } else {
            self.muted = self.muted_before_deafen;
        }
        self.deafened = on;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MuteChange {
    ToggleMute,
    ToggleDeafen,
    /// Only Android's notification buttons say which way, since a stale press must not flip it back.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    SetMuted(bool),
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    SetDeafened(bool),
    /// Microphone and call audio both back on.
    Unmute,
    /// What the server says we are. It is not told back.
    Server {
        muted: bool,
        deafened: bool,
    },
}

/// What has to follow a mute change, so no caller writes the triple by hand.
#[must_use]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MuteFollowup {
    pub muted: bool,
    pub deafened: bool,
    tell_server: bool,
}

impl MuteFollowup {
    pub fn server(&self) -> Option<ClientMessage> {
        self.tell_server.then_some(ClientMessage::SetVoiceMute {
            muted: self.muted,
            deafened: self.deafened,
        })
    }

    pub fn audio(&self) -> [crate::features::voice::VoiceCmd; 2] {
        use crate::features::voice::VoiceCmd;
        [
            VoiceCmd::SetDeafen {
                deafened: self.deafened,
            },
            VoiceCmd::SetMute { muted: self.muted },
        ]
    }

    pub fn send(self, gateway: &crate::state::GatewayTx, voice: &crate::features::voice::VoiceTx) {
        if let Some(message) = self.server() {
            gateway.send(message);
        }
        for command in self.audio() {
            voice.send(command);
        }
    }
}

/// Tests set a call up directly instead of replaying the events that lead to it.
#[cfg(test)]
impl VoiceSession {
    pub fn force(&mut self, call: CallState) {
        self.call = call;
    }

    pub fn force_channel(&mut self, channel: Option<Id>) {
        self.call = match (channel, &self.call) {
            (None, _) => CallState::Idle,
            (Some(c), CallState::Connected(_)) => CallState::Connected(c),
            (Some(c), CallState::Reconnecting(_)) => CallState::Reconnecting(c),
            (Some(c), CallState::Restarting(_)) => CallState::Restarting(c),
            (Some(c), _) => CallState::Dialing(c),
        };
    }

    pub fn force_mute(&mut self, muted: bool, deafened: bool) {
        assert!(muted || !deafened, "deafened always mutes");
        self.muted = muted;
        self.deafened = deafened;
    }

    pub fn force_phase(&mut self, phase: VoicePhase) {
        let channel = self.channel_id().unwrap_or_else(Id::nil);
        self.call = match phase {
            VoicePhase::Idle => CallState::Idle,
            VoicePhase::Connecting => CallState::Dialing(channel),
            VoicePhase::Connected => CallState::Connected(channel),
            VoicePhase::Error => CallState::Failed("test".into()),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(n: u128) -> Id {
        Id::from_u128(n)
    }

    fn lost(channel: Id) -> VoiceEvent {
        VoiceEvent::RoomLost {
            channel,
            error: "lost".into(),
        }
    }

    fn failed(channel: Id) -> VoiceEvent {
        VoiceEvent::Failed {
            channel,
            error: "failed".into(),
        }
    }

    #[test]
    fn only_a_room_in_use_can_be_taken_by_another_device() {
        let (here, other) = (Id::from_u128(1), Id::from_u128(2));
        for (call, expected) in [
            (CallState::Connected(here), true),
            (CallState::Reconnecting(here), true),
            (CallState::Connected(other), false),
            (CallState::Dialing(here), false),
            (CallState::Restarting(here), false),
            (CallState::Idle, false),
            (CallState::Failed("x".into()), false),
        ] {
            let session = VoiceSession {
                call: call.clone(),
                ..VoiceSession::default()
            };
            assert_eq!(session.holds_room(here), expected, "{call:?}");
        }
    }

    #[test]
    fn every_call_transition() {
        use CallState::*;
        let (a, b) = (ch(1), ch(2));
        let gone = || Failed("lost".into());
        let refused = || Failed("failed".into());
        let table: Vec<(CallState, VoiceEvent, CallState)> = vec![
            (Idle, VoiceEvent::Granted(a), Dialing(a)),
            (refused(), VoiceEvent::Granted(a), Dialing(a)),
            (Connected(a), VoiceEvent::Granted(b), Dialing(b)),
            (Connected(a), VoiceEvent::Granted(a), Dialing(a)),
            (Dialing(a), VoiceEvent::RoomUp(a), Connected(a)),
            (Restarting(a), VoiceEvent::RoomUp(a), Connected(a)),
            (Dialing(b), VoiceEvent::RoomUp(a), Dialing(b)),
            (Idle, VoiceEvent::RoomUp(a), Idle),
            (refused(), VoiceEvent::RoomUp(a), refused()),
            (
                Connected(a),
                VoiceEvent::RoomReconnecting(a),
                Reconnecting(a),
            ),
            (Dialing(a), VoiceEvent::RoomReconnecting(a), Dialing(a)),
            (
                Restarting(a),
                VoiceEvent::RoomReconnecting(a),
                Restarting(a),
            ),
            (Connected(b), VoiceEvent::RoomReconnecting(a), Connected(b)),
            (
                Reconnecting(a),
                VoiceEvent::RoomReconnected(a),
                Connected(a),
            ),
            (Dialing(a), VoiceEvent::RoomReconnected(a), Dialing(a)),
            (Restarting(a), VoiceEvent::RoomReconnected(a), Restarting(a)),
            (Connected(a), lost(a), gone()),
            (Reconnecting(a), lost(a), gone()),
            (Restarting(a), lost(a), Restarting(a)),
            (Dialing(b), lost(a), Dialing(b)),
            (Connected(b), lost(a), Connected(b)),
            (Connected(a), VoiceEvent::RestartBegun(a), Restarting(a)),
            (Reconnecting(a), VoiceEvent::RestartBegun(a), Restarting(a)),
            (Dialing(a), VoiceEvent::RestartBegun(a), Dialing(a)),
            (gone(), VoiceEvent::RestartBegun(a), gone()),
            (Connected(b), VoiceEvent::RestartBegun(a), Connected(b)),
            (Dialing(a), failed(a), refused()),
            (Restarting(a), failed(a), refused()),
            (Dialing(b), failed(a), Dialing(b)),
            (Connected(a), failed(a), Connected(a)),
            (Idle, failed(a), Idle),
            (Connected(a), VoiceEvent::Ended, Idle),
            (Dialing(a), VoiceEvent::Ended, Idle),
            (refused(), VoiceEvent::Ended, Idle),
        ];
        for (from, event, to) in table {
            let mut voice = VoiceSession::default();
            voice.force(from.clone());
            assert_eq!(
                voice.would_change(&event),
                from != to,
                "{from:?} on {event:?}"
            );
            voice.on(event.clone());
            assert_eq!(voice.call(), &to, "{from:?} on {event:?}");
        }
    }

    #[test]
    fn a_pending_join_is_its_own_slot() {
        let (a, b) = (ch(1), ch(2));
        let table: Vec<(Option<Id>, VoiceEvent, Option<Id>, bool)> = vec![
            (None, VoiceEvent::JoinSent(a), Some(a), true),
            (Some(a), VoiceEvent::JoinSent(b), Some(b), true),
            (Some(a), VoiceEvent::LeaveSent, None, true),
            (Some(a), VoiceEvent::JoinRefused, None, true),
            (Some(a), VoiceEvent::JoinAbandoned, None, true),
            (Some(a), VoiceEvent::Placed(a), None, true),
            (Some(a), VoiceEvent::Placed(b), Some(a), false),
            (None, VoiceEvent::Placed(a), None, false),
            (Some(a), VoiceEvent::Granted(a), Some(a), true),
            (Some(a), VoiceEvent::Ended, Some(a), false),
        ];
        for (joining, event, expected, changed) in table {
            let mut voice = VoiceSession {
                joining,
                ..VoiceSession::default()
            };
            assert_eq!(voice.on(event.clone()), changed, "{joining:?} on {event:?}");
            assert_eq!(
                voice.joining_channel_id(),
                expected,
                "{joining:?} on {event:?}"
            );
        }
    }

    #[test]
    fn a_move_dials_one_channel_while_connected_to_another() {
        let (a, b) = (ch(1), ch(2));
        let mut voice = VoiceSession::default();
        voice.force(CallState::Connected(a));
        voice.on(VoiceEvent::JoinSent(b));
        assert_eq!(
            (voice.channel_id(), voice.joining_channel_id()),
            (Some(a), Some(b))
        );
        assert!(voice.on(VoiceEvent::Placed(b)));
        voice.on(VoiceEvent::Granted(b));
        assert_eq!(
            (voice.channel_id(), voice.joining_channel_id()),
            (Some(b), None)
        );
        assert_eq!(voice.phase(), VoicePhase::Connecting);
    }

    #[test]
    fn what_each_state_shows() {
        use CallState::*;
        let a = ch(1);
        for (call, phase, channel, connected) in [
            (Idle, VoicePhase::Idle, None, None),
            (Dialing(a), VoicePhase::Connecting, Some(a), None),
            (Connected(a), VoicePhase::Connected, Some(a), Some(a)),
            (Reconnecting(a), VoicePhase::Connecting, Some(a), None),
            (Restarting(a), VoicePhase::Connected, Some(a), Some(a)),
            (Failed("x".into()), VoicePhase::Error, None, None),
        ] {
            let mut voice = VoiceSession::default();
            voice.force(call.clone());
            assert_eq!(voice.phase(), phase, "{call:?}");
            assert_eq!(voice.channel_id(), channel, "{call:?}");
            assert_eq!(voice.connected_channel(), connected, "{call:?}");
        }
    }

    #[test]
    fn a_failed_connect_outranks_the_device_banner_and_leaving_clears_both() {
        let a = ch(1);
        let mut voice = VoiceSession::default();
        voice.force(CallState::Connected(a));
        assert!(voice.set_device_error(Some("no mic".into())));
        assert!(!voice.set_device_error(Some("no mic".into())));
        assert_eq!(voice.error(), Some("no mic"));
        voice.on(lost(a));
        assert_eq!(voice.error(), Some("lost"));
        voice.on(VoiceEvent::Granted(a));
        assert_eq!(voice.error(), Some("no mic"));
        voice.on(VoiceEvent::Ended);
        assert_eq!(voice.error(), None);
    }

    fn mute(muted: bool, deafened: bool, before_deafen: bool) -> VoiceSession {
        VoiceSession {
            muted,
            deafened,
            muted_before_deafen: before_deafen,
            ..VoiceSession::default()
        }
    }

    /// (muted, deafened, muted before deafen), change, (muted, deafened), tells the server
    type MuteRow = ((bool, bool, bool), MuteChange, (bool, bool), Option<bool>);

    #[test]
    fn every_mute_transition_keeps_deafened_muted() {
        use MuteChange::*;
        let table: Vec<MuteRow> = vec![
            ((false, false, false), ToggleMute, (true, false), Some(true)),
            ((true, false, false), ToggleMute, (false, false), Some(true)),
            ((true, true, false), ToggleMute, (true, true), None),
            (
                (false, false, false),
                SetMuted(true),
                (true, false),
                Some(true),
            ),
            ((true, false, false), SetMuted(true), (true, false), None),
            ((true, true, false), SetMuted(false), (true, true), None),
            (
                (false, false, false),
                ToggleDeafen,
                (true, true),
                Some(true),
            ),
            ((true, false, false), ToggleDeafen, (true, true), Some(true)),
            (
                (true, true, false),
                ToggleDeafen,
                (false, false),
                Some(true),
            ),
            ((true, true, true), ToggleDeafen, (true, false), Some(true)),
            (
                (false, false, false),
                SetDeafened(true),
                (true, true),
                Some(true),
            ),
            ((true, true, false), SetDeafened(true), (true, true), None),
            (
                (true, true, false),
                SetDeafened(false),
                (false, false),
                Some(true),
            ),
            (
                (false, false, false),
                SetDeafened(false),
                (false, false),
                None,
            ),
            ((true, true, true), Unmute, (false, false), Some(true)),
            ((true, false, false), Unmute, (false, false), Some(true)),
            ((false, false, false), Unmute, (false, false), None),
            (
                (false, false, false),
                Server {
                    muted: true,
                    deafened: false,
                },
                (true, false),
                Some(false),
            ),
            (
                (false, false, false),
                Server {
                    muted: false,
                    deafened: true,
                },
                (true, true),
                Some(false),
            ),
            (
                (true, true, false),
                Server {
                    muted: true,
                    deafened: true,
                },
                (true, true),
                None,
            ),
        ];
        for ((muted, deafened, before), change, after, tells) in table {
            let mut voice = mute(muted, deafened, before);
            let followup = voice.change_mute(change);
            let label = format!("{muted}/{deafened}/{before} on {change:?}");
            assert_eq!((voice.muted(), voice.deafened()), after, "{label}");
            assert!(voice.muted() || !voice.deafened(), "{label}");
            assert_eq!(followup.map(|f| f.server().is_some()), tells, "{label}");
            if let Some(followup) = followup {
                assert_eq!((followup.muted, followup.deafened), after, "{label}");
                if let Some(ClientMessage::SetVoiceMute { muted, deafened }) = followup.server() {
                    assert_eq!((muted, deafened), after, "{label}");
                }
                let [deafen, mic] = followup.audio();
                assert!(
                    matches!(deafen, crate::features::voice::VoiceCmd::SetDeafen { deafened } if deafened == after.1),
                    "{label}"
                );
                assert!(
                    matches!(mic, crate::features::voice::VoiceCmd::SetMute { muted } if muted == after.0),
                    "{label}"
                );
            }
        }
    }

    #[test]
    fn joining_voice_carries_the_local_mute_and_deafen_choices() {
        let channel_id = ch(9);
        for (muted, deafened) in [(false, false), (true, false), (true, true)] {
            let voice = mute(muted, deafened, false);
            match voice.join_message(channel_id) {
                ClientMessage::JoinVoice {
                    channel_id: requested,
                    preferences: Some(prefs),
                } => {
                    assert_eq!(requested, channel_id);
                    assert_eq!((prefs.muted, prefs.deafened), (muted, deafened));
                }
                other => panic!("unexpected join: {other:?}"),
            }
        }
    }

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
