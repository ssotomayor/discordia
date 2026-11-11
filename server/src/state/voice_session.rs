use std::collections::HashMap;

use super::AppState;
use crate::protocol::{Id, VoicePreferences, VoiceState};
use crate::store::EndedCall;

/// One record so membership and ownership cannot come apart; `voice_states`
/// mirrors it for readers and changes only through the transitions below.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VoiceSession {
    pub owner: u64,
    /// The owner's socket closed: the call waits out the reconnect grace for
    /// `hand_off_voice` or `leave_voice`.
    pub owner_lost: bool,
    pub channel_id: Id,
    pub join: u64,
}

/// What a grant minted for one join must still match when it is delivered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoiceCall {
    pub pubkey: String,
    pub channel_id: Id,
    pub owner: u64,
    pub join: u64,
}

impl VoiceSession {
    fn call(&self, pubkey: &str) -> VoiceCall {
        VoiceCall {
            pubkey: pubkey.to_string(),
            channel_id: self.channel_id,
            owner: self.owner,
            join: self.join,
        }
    }
}

#[derive(Debug)]
pub struct VoiceJoin {
    /// The call this join replaced, read under the same lock that replaced it.
    pub previous: Option<VoiceState>,
    pub state: VoiceState,
    pub call: VoiceCall,
}

#[derive(Default)]
pub(crate) struct VoiceSessions {
    by_key: HashMap<String, VoiceSession>,
    last_join: u64,
}

impl AppState {
    fn voice_sessions(&self) -> std::sync::MutexGuard<'_, VoiceSessions> {
        self.voice_sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Always a new join, even into the same channel, so grants minted for the
    /// old one go stale.
    pub fn join_voice(
        &self,
        conn_id: u64,
        user_pubkey: &str,
        guild_id: Id,
        channel_id: Id,
        preferences: Option<VoicePreferences>,
    ) -> VoiceJoin {
        let mut sessions = self.voice_sessions();
        self.put_in_call(
            &mut sessions,
            conn_id,
            user_pubkey,
            guild_id,
            channel_id,
            preferences,
        )
    }

    /// By `LeaveVoice`, a close or the grace running out. A socket id is never
    /// reused, so it names this ownership as a join generation would.
    pub fn leave_voice(&self, user_pubkey: &str, conn_id: u64) -> Option<(VoiceState, VoiceState)> {
        let mut sessions = self.voice_sessions();
        if sessions.by_key.get(user_pubkey)?.owner != conn_id {
            return None;
        }
        let ended = self.take_out_of_call(&mut sessions, user_pubkey)?;
        Some((ended.previous, ended.cleared))
    }

    /// Checked under the lock, so a decision about one call cannot end the
    /// next one the person joined meanwhile.
    pub fn end_voice(
        &self,
        user_pubkey: &str,
        which: impl FnOnce(&VoiceState) -> bool,
    ) -> Option<EndedCall> {
        let mut sessions = self.voice_sessions();
        let current = self.voice_states.get(user_pubkey).map(|v| which(&v))?;
        if !current {
            return None;
        }
        self.take_out_of_call(&mut sessions, user_pubkey)
    }

    /// A new join, so the grant minted for the old room is never delivered.
    pub(crate) fn move_voice_session(
        &self,
        user_pubkey: &str,
        guild_id: Id,
        from_channel_id: Id,
        channel_id: Id,
    ) -> Result<(VoiceState, VoiceJoin), String> {
        let mut sessions = self.voice_sessions();
        let session = *sessions
            .by_key
            .get(user_pubkey)
            .ok_or("they aren't in a voice channel here")?;
        if session.owner_lost {
            return Err("their voice connection is unavailable".into());
        }
        let previous = self
            .voice_states
            .get(user_pubkey)
            .map(|v| v.clone())
            .ok_or("they aren't in a voice channel here")?;
        if previous.channel_id != Some(from_channel_id) || previous.guild_id != guild_id {
            return Err("their voice channel changed before the move".into());
        }
        let joined = self.put_in_call(
            &mut sessions,
            session.owner,
            user_pubkey,
            guild_id,
            channel_id,
            None,
        );
        Ok((previous, joined))
    }

    /// Only during the grace: a live owner keeps its call.
    pub fn hand_off_voice(&self, user_pubkey: &str, conn_id: u64) {
        let mut sessions = self.voice_sessions();
        if let Some(session) = sessions.by_key.get_mut(user_pubkey)
            && session.owner_lost
        {
            session.owner = conn_id;
            session.owner_lost = false;
        }
    }

    pub(crate) fn voice_owner_lost(&self, user_pubkey: &str, conn_id: u64) {
        let mut sessions = self.voice_sessions();
        if let Some(session) = sessions.by_key.get_mut(user_pubkey)
            && session.owner == conn_id
        {
            session.owner_lost = true;
        }
    }

    pub fn voice_session(&self, user_pubkey: &str) -> Option<VoiceSession> {
        self.voice_sessions().by_key.get(user_pubkey).copied()
    }

    pub fn voice_owned_by(&self, user_pubkey: &str, conn_id: u64) -> bool {
        self.voice_sessions()
            .by_key
            .get(user_pubkey)
            .is_some_and(|s| s.owner == conn_id)
    }

    /// The one freshness check for every voice grant (trap 26).
    pub fn is_current_call(&self, call: &VoiceCall) -> bool {
        self.voice_sessions()
            .by_key
            .get(&call.pubkey)
            .is_some_and(|s| s.join == call.join && s.owner == call.owner && !s.owner_lost)
    }

    pub fn live_calls(&self) -> Vec<VoiceCall> {
        self.voice_sessions()
            .by_key
            .iter()
            .filter(|(_, s)| !s.owner_lost)
            .map(|(pk, s)| s.call(pk))
            .collect()
    }

    /// Under the session lock, so no transition runs between the check and the change.
    pub fn update_owned_voice(
        &self,
        pubkey: &str,
        conn_id: u64,
        update: impl FnOnce(&Self) -> Option<VoiceState>,
    ) -> Option<VoiceState> {
        let sessions = self.voice_sessions();
        if sessions.by_key.get(pubkey).map(|s| s.owner) != Some(conn_id) {
            return None;
        }
        update(self)
    }

    fn put_in_call(
        &self,
        sessions: &mut VoiceSessions,
        owner: u64,
        user_pubkey: &str,
        guild_id: Id,
        channel_id: Id,
        preferences: Option<VoicePreferences>,
    ) -> VoiceJoin {
        let previous = self.voice_states.get(user_pubkey).map(|v| v.clone());
        let deafened = preferences
            .map(|p| p.deafened)
            .unwrap_or_else(|| previous.as_ref().is_some_and(|p| p.deafened));
        let muted = deafened
            || preferences
                .map(|p| p.muted)
                .unwrap_or_else(|| previous.as_ref().is_some_and(|p| p.muted));
        let state = VoiceState {
            user_pubkey: user_pubkey.to_string(),
            guild_id,
            channel_id: Some(channel_id),
            muted,
            deafened,
            speaking: false,
            screen_sharing: false,
            screen_watching: Vec::new(),
            camera_watching: Vec::new(),
            camera_on: false,
        };
        sessions.last_join += 1;
        let session = VoiceSession {
            owner,
            owner_lost: false,
            channel_id,
            join: sessions.last_join,
        };
        sessions.by_key.insert(user_pubkey.to_string(), session);
        self.voice_states
            .insert(user_pubkey.to_string(), state.clone());
        VoiceJoin {
            previous,
            state,
            call: session.call(user_pubkey),
        }
    }

    fn take_out_of_call(
        &self,
        sessions: &mut VoiceSessions,
        user_pubkey: &str,
    ) -> Option<EndedCall> {
        sessions.by_key.remove(user_pubkey);
        let previous = self.voice_states.remove(user_pubkey)?.1;
        let cleared = VoiceState {
            channel_id: None,
            speaking: false,
            camera_on: false,
            screen_sharing: false,
            screen_watching: Vec::new(),
            camera_watching: Vec::new(),
            ..previous.clone()
        };
        Some(EndedCall { previous, cleared })
    }
}

#[cfg(test)]
#[path = "voice_session_tests.rs"]
mod tests;
