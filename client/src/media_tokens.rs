use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use base64::Engine as _;
use dioxus::prelude::*;

use crate::protocol::{ClientMessage, MediaTokenKind};
use crate::state::{AppState, GatewayTx};

const RETRY: Duration = Duration::from_secs(15);
static REQUEST: AtomicU64 = AtomicU64::new(1);
const KINDS: [MediaTokenKind; 4] = [
    MediaTokenKind::Screen,
    MediaTokenKind::ScreenAudio,
    MediaTokenKind::ScreenVideo,
    MediaTokenKind::ScreenViewer,
];

#[derive(Clone)]
struct Pending {
    id: u64,
    epoch: u64,
    call: crate::voice_session::VoiceCall,
    token: String,
    at: Instant,
    complete: bool,
    renew_after: Option<Instant>,
}

#[derive(Clone, Default)]
pub(crate) struct Renewals(HashMap<MediaTokenKind, Pending>);

#[derive(Clone, Copy)]
pub(crate) struct CallGuard {
    call: Option<crate::voice_session::VoiceCall>,
    connected: Option<crate::protocol::Id>,
    epoch: u64,
}

impl CallGuard {
    pub(crate) fn new(s: &AppState) -> Self {
        Self {
            call: s.voice_call(),
            connected: s.connected_voice_channel(),
            epoch: s.voice_session_epoch,
        }
    }

    pub(crate) fn current(self, s: &AppState) -> bool {
        self.call == s.voice_call()
            && self.connected == s.connected_voice_channel()
            && self.epoch == s.voice_session_epoch
    }
}

pub(crate) fn room(s: &AppState, kind: MediaTokenKind) -> Option<&(String, String)> {
    match kind {
        MediaTokenKind::Screen => &s.screen_token,
        MediaTokenKind::ScreenAudio => &s.screen_audio_token,
        MediaTokenKind::ScreenVideo => &s.screen_video_token,
        MediaTokenKind::ScreenViewer => &s.screen_viewer_token,
    }
    .as_ref()
}

fn claims(token: &str) -> Option<serde_json::Value> {
    if token.len() > 16 * 1024 {
        return None;
    }
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub(crate) fn expires_soon(token: &str, now: i64) -> bool {
    claims(token)
        .and_then(|c| c["exp"].as_i64())
        .is_some_and(|expires| expires <= now.saturating_add(60))
}

// These claims only keep an already authenticated room alive; the SFU authenticates every join.
pub(crate) fn same_room(a: &(String, String), b: &(String, String)) -> bool {
    if a == b {
        return true;
    }
    if a.0 != b.0 {
        return false;
    }
    same_token_scope(&a.1, &b.1)
}

pub(crate) fn same_token_scope(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let (Some(a), Some(b)) = (claims(a), claims(b)) else {
        return false;
    };
    ["iss", "sub"]
        .iter()
        .all(|key| a[*key].as_str().is_some() && a[*key] == b[*key])
        && a["video"]["room"].as_str().is_some()
        && a["video"]["room"] == b["video"]["room"]
}

fn can_request(s: &AppState, kind: MediaTokenKind, now: Instant) -> bool {
    s.media_token_renewals.0.get(&kind).is_none_or(|p| {
        p.epoch != s.voice_session_epoch
            || Some(p.call) != s.voice_call()
            || now.duration_since(p.at) >= RETRY
    })
}

pub(crate) fn request(s: &mut AppState, kind: MediaTokenKind) -> Option<ClientMessage> {
    let call = s.voice_call()?;
    if !s.server_media_token_refresh
        || s.voice.channel_id() != Some(call.channel)
        || !can_request(s, kind, Instant::now())
    {
        return None;
    }
    let token = room(s, kind)?.1.clone();
    let id = REQUEST.fetch_add(1, Ordering::Relaxed);
    s.media_token_renewals.0.insert(
        kind,
        Pending {
            id,
            epoch: s.voice_session_epoch,
            call,
            token,
            at: Instant::now(),
            complete: false,
            renew_after: None,
        },
    );
    Some(ClientMessage::RefreshMediaToken {
        channel_id: call.channel,
        route_revision: call.route,
        kind,
        request_id: id,
    })
}

fn renewal_due(s: &AppState, kind: MediaTokenKind, now: Instant, wall: i64) -> bool {
    let Some((_, token)) = room(s, kind) else {
        return false;
    };
    if let Some(p) = s.media_token_renewals.0.get(&kind)
        && p.epoch == s.voice_session_epoch
        && Some(p.call) == s.voice_call()
    {
        if !p.complete {
            return true;
        }
        if p.token == *token
            && let Some(at) = p.renew_after
        {
            return now >= at;
        }
    }
    expires_soon(token, wall)
}

fn refresh_after(token: &str, now: Instant) -> Option<Instant> {
    let c = claims(token)?;
    let lifetime = c["exp"].as_u64()?.checked_sub(c["nbf"].as_u64()?)?;
    // Issuer-relative lifetime avoids a bad client clock repeatedly renewing a fresh grant.
    now.checked_add(Duration::from_secs(
        lifetime.min(24 * 3600).saturating_sub(60),
    ))
}

pub(crate) fn poll(mut state: Signal<AppState>, gateway: &GatewayTx) {
    let now = Instant::now();
    let wall = chrono::Utc::now().timestamp();
    let due: Vec<_> = {
        let s = state.peek();
        if !s.server_media_token_refresh || s.voice.channel_id().is_none() {
            return;
        }
        KINDS
            .into_iter()
            .filter(|kind| can_request(&s, *kind, now) && renewal_due(&s, *kind, now, wall))
            .collect()
    };
    for kind in due {
        if let Some(message) = request(&mut state.write(), kind) {
            gateway.send(message);
        }
    }
}

pub(crate) fn accept(
    s: &mut AppState,
    channel: crate::protocol::Id,
    route: u32,
    kind: MediaTokenKind,
    id: u64,
    token: String,
    ice_servers: Vec<crate::protocol::IceServer>,
) {
    let Some(pending) = s.media_token_renewals.0.get(&kind) else {
        return;
    };
    if pending.complete
        || pending.id != id
        || pending.epoch != s.voice_session_epoch
        || pending.call != (crate::voice_session::VoiceCall { channel, route })
        || s.voice_call() != Some(pending.call)
        || s.voice.channel_id() != Some(channel)
        || room(s, kind).is_none_or(|r| r.1 != pending.token)
        || token.is_empty()
    {
        return;
    }
    s.ice_servers = ice_servers;
    let target = match kind {
        MediaTokenKind::Screen => &mut s.screen_token,
        MediaTokenKind::ScreenAudio => &mut s.screen_audio_token,
        MediaTokenKind::ScreenVideo => &mut s.screen_video_token,
        MediaTokenKind::ScreenViewer => &mut s.screen_viewer_token,
    };
    if let Some((_, current)) = target {
        *current = token.clone();
    }
    if let Some(pending) = s.media_token_renewals.0.get_mut(&kind) {
        pending.complete = true;
        pending.renew_after = refresh_after(&token, Instant::now());
        pending.token = token;
    }
}

pub(crate) fn authentication_error(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    error.contains("stream permission was rejected")
        || error.contains("media permission was rejected")
        || error.contains("token is expired")
        || error.contains("token expired")
        || error.contains("invalid token")
        || error.contains("unauthorized")
        || error.contains("status code: 401")
        || error.contains("status: 401")
        || error.contains("http error 401")
        || error.contains("http 401")
}

pub(crate) fn error_detail(error: &str) -> String {
    if authentication_error(error) {
        return "The media permission was rejected or expired. If it persists, rejoin voice and check the voice server's clock and configuration.".into();
    }
    let mut redacted = String::new();
    for word in error
        .split_inclusive(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')))
    {
        let mut parts = word.split('.');
        let jwt = (0..3).all(|_| parts.next().is_some_and(|p| p.len() >= 8));
        if jwt || word.starts_with("eyJ") && word.contains('.') {
            redacted.push_str("[redacted token] ");
        } else {
            redacted.push_str(word);
        }
        if redacted.len() >= 300 {
            break;
        }
    }
    redacted.chars().take(300).collect()
}

pub(crate) fn pending(s: &AppState, kind: MediaTokenKind) -> bool {
    s.media_token_renewals.0.get(&kind).is_some_and(|p| {
        !p.complete && p.epoch == s.voice_session_epoch && Some(p.call) == s.voice_call()
    })
}

#[cfg(test)]
pub(crate) fn empty(renewals: &Renewals) -> bool {
    renewals.0.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Id, User, VoiceState};

    fn token(exp: i64, sub: &str, room: &str) -> String {
        let data = serde_json::json!({"exp":exp,"nbf":exp.saturating_sub(3600),"iss":"issuer","sub":sub,"video":{"room":room}});
        format!(
            "e30.{}.signature",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data.to_string())
        )
    }

    fn active() -> AppState {
        let mut s = AppState::empty();
        let channel = Id::new_v4();
        s.server_media_token_refresh = true;
        s.self_user = Some(User {
            pubkey: "me".into(),
            username: "Me".into(),
        });
        s.voice.force_channel(Some(channel));
        s.voice.force_phase(crate::state::VoicePhase::Connected);
        s.voice_states.push(VoiceState {
            user_pubkey: "me".into(),
            guild_id: Id::new_v4(),
            channel_id: Some(channel),
            muted: false,
            deafened: false,
            speaking: false,
            camera_on: false,
            screen_sharing: false,
            screen_watching: Vec::new(),
            camera_watching: Vec::new(),
        });
        s.screen_token = Some(("ws://127.0.0.1:1234/sfu".into(), "old".into()));
        s.screen_audio_token = Some(("audio-url".into(), "audio".into()));
        s
    }

    fn ask(s: &mut AppState) -> (Id, u32, u64) {
        let Some(ClientMessage::RefreshMediaToken {
            channel_id,
            route_revision,
            request_id,
            ..
        }) = request(s, MediaTokenKind::Screen)
        else {
            panic!("request expected")
        };
        (channel_id, route_revision, request_id)
    }

    #[test]
    fn renewals_update_only_the_requested_seat_and_preserve_the_tunnel_and_call() {
        let mut s = active();
        let epoch = s.voice_session_epoch;
        let fresh_ice = vec![crate::protocol::IceServer {
            urls: vec!["turn:relay.test:3478".into()],
            username: "fresh".into(),
            credential: "credential".into(),
        }];
        let (channel, route, id) = ask(&mut s);
        assert!(request(&mut s, MediaTokenKind::Screen).is_none());
        accept(
            &mut s,
            channel,
            route,
            MediaTokenKind::Screen,
            id + 1,
            "wrong".into(),
            fresh_ice.clone(),
        );
        assert_eq!(s.screen_token.as_ref().unwrap().1, "old");
        assert!(s.ice_servers.is_empty());
        accept(
            &mut s,
            channel,
            route,
            MediaTokenKind::Screen,
            id,
            "new".into(),
            fresh_ice.clone(),
        );
        assert_eq!(
            s.screen_token.as_ref().unwrap(),
            &("ws://127.0.0.1:1234/sfu".into(), "new".into())
        );
        assert_eq!(s.screen_audio_token.as_ref().unwrap().1, "audio");
        assert_eq!(s.voice_session_epoch, epoch);
        assert_eq!(s.connected_voice_channel(), Some(channel));
        accept(
            &mut s,
            channel,
            route,
            MediaTokenKind::Screen,
            id,
            "duplicate".into(),
            Vec::new(),
        );
        assert_eq!(s.screen_token.as_ref().unwrap().1, "new");
        assert_eq!(s.ice_servers, fresh_ice);
    }

    #[test]
    fn renewals_cannot_restore_a_ended_replaced_or_rerouted_call() {
        for change in 0..5 {
            let mut s = active();
            let (channel, route, id) = ask(&mut s);
            match change {
                0 => s.end_voice_locally(),
                1 => s.voice_session_epoch += 1,
                2 => s.voice_route_revision += 1,
                3 => s.voice.force_channel(Some(Id::new_v4())),
                _ => s.screen_token.as_mut().unwrap().1 = "replacement".into(),
            }
            let before = s.screen_token.clone();
            accept(
                &mut s,
                channel,
                route,
                MediaTokenKind::Screen,
                id,
                "stale".into(),
                Vec::new(),
            );
            assert_eq!(s.screen_token, before, "change {change}");
        }
    }

    #[test]
    fn asynchronous_media_start_cannot_follow_a_changed_call() {
        let mut s = active();
        let guard = CallGuard::new(&s);
        assert!(guard.current(&s));
        s.voice_route_revision += 1;
        assert!(!guard.current(&s));
        s = active();
        let guard = CallGuard::new(&s);
        s.voice.force_phase(crate::state::VoicePhase::Idle);
        assert!(!guard.current(&s));
        s = active();
        let guard = CallGuard::new(&s);
        s.voice_session_epoch += 1;
        assert!(!guard.current(&s));
    }

    #[test]
    fn renewal_retries_are_bounded_and_require_server_support_and_a_local_call() {
        let mut s = active();
        s.server_media_token_refresh = false;
        assert!(request(&mut s, MediaTokenKind::Screen).is_none());
        s.server_media_token_refresh = true;
        let (_, _, first) = ask(&mut s);
        assert!(request(&mut s, MediaTokenKind::Screen).is_none());
        s.media_token_renewals
            .0
            .get_mut(&MediaTokenKind::Screen)
            .unwrap()
            .at -= RETRY;
        let (_, _, second) = ask(&mut s);
        assert_ne!(first, second);
        s.end_voice_locally();
        assert!(request(&mut s, MediaTokenKind::Screen).is_none());
        assert!(empty(&s.media_token_renewals));
    }

    #[test]
    fn expiry_scheduling_and_room_reuse_do_not_authorize_other_rooms_or_seats() {
        let old = token(100, "me", "screen-one");
        let new = token(1000, "me", "screen-one");
        assert!(expires_soon(&old, 100));
        assert!(expires_soon(&old, 40));
        assert!(!expires_soon(&new, 100));
        assert!(!expires_soon("not-a-token", 100));
        let pair = |t| ("wss://sfu".into(), t);
        assert!(same_room(&pair(old.clone()), &pair(new)));
        assert!(!same_room(
            &pair(old.clone()),
            &pair(token(1000, "other", "screen-one"))
        ));
        assert!(!same_room(
            &pair(old.clone()),
            &pair(token(1000, "me", "screen-two"))
        ));
        assert!(!same_room(
            &pair(old.clone()),
            &("wss://elsewhere".into(), old)
        ));
    }

    #[test]
    fn a_renewed_grant_uses_issuer_lifetime_even_if_the_clients_clock_is_wrong() {
        let mut s = active();
        let (channel, route, id) = ask(&mut s);
        let fresh = token(10_000, "me", "screen-one");
        accept(
            &mut s,
            channel,
            route,
            MediaTokenKind::Screen,
            id,
            fresh,
            Vec::new(),
        );
        let now = Instant::now();
        assert!(!renewal_due(&s, MediaTokenKind::Screen, now, 100_000_000));
        assert!(renewal_due(
            &s,
            MediaTokenKind::Screen,
            now + Duration::from_secs(3600),
            0
        ));
    }

    #[test]
    fn connection_errors_never_display_a_bearer_token() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJleHAiOjE3MDAwMDAwMDB9.bearer_signature";
        for error in [
            format!("invalid token: {jwt}: token is expired"),
            format!("failed: ({jwt})."),
            format!("server rejected {jwt}\ntry again"),
        ] {
            let safe = error_detail(&error);
            assert!(!safe.contains(jwt));
            assert!(!safe.contains("eyJleHA"));
        }
        assert_eq!(error_detail("network timeout"), "network timeout");
        assert!(authentication_error(
            "could not establish signal connection: invalid token"
        ));
        assert!(!authentication_error("network timeout"));
    }
}
