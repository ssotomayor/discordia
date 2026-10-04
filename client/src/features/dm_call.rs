use std::collections::HashMap;
use std::time::{Duration, Instant};

use dioxus::core::Task;
use dioxus::prelude::*;
use libwebrtc::{
    media_stream_track::MediaStreamTrack,
    peer_connection::{
        AnswerOptions, IceGatheringState, OfferOptions, PeerConnection, PeerConnectionState,
    },
    peer_connection_factory::{
        ContinualGatheringPolicy, IceServer, IceTransportsType, PeerConnectionFactory,
        RtcConfiguration,
    },
    session_description::{SdpType, SessionDescription},
};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use uuid::Uuid;

use crate::nostr::calls::{Body, EndReason, Signal as CallSignal};
use crate::state::{AppState, VoicePhase, use_app_state};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Incoming,
    Ringing,
    Connecting,
    Connected,
    Reconnecting,
}

impl Phase {
    fn label(self) -> &'static str {
        match self {
            Self::Incoming => "Incoming voice call",
            Self::Ringing => "Calling…",
            Self::Connecting => "Connecting…",
            Self::Connected => "Voice call connected",
            Self::Reconnecting => "Reconnecting…",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallView {
    pub peer: String,
    pub phase: Phase,
    pub microphone_error: Option<String>,
}

pub enum Action {
    Start(String),
    Accept,
    End,
}

pub struct Incoming {
    pub author: String,
    pub signal: CallSignal,
}

pub enum Command {
    Action(Action),
    Incoming(Incoming),
    Published { call_id: Uuid, accepted: bool },
    Native { call_id: Uuid, event: NativeEvent },
}

pub enum NativeEvent {
    State(PeerConnectionState),
    Track(libwebrtc::audio_track::RtcAudioTrack),
    Description(Result<(bool, String), String>),
    RemoteDescription(Result<(), String>),
}

struct Session {
    id: Uuid,
    peer: String,
    remote_device: Option<Uuid>,
    phase: Phase,
    deadline: Option<Instant>,
    connection: Option<PeerConnection>,
    audio: Option<super::voice::direct::Audio>,
    job: Option<Task>,
    remote_answer_set: bool,
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(job) = self.job.take() {
            job.cancel();
        }
        self.audio.take();
        if let Some(connection) = self.connection.take() {
            connection.on_track(None);
            connection.on_connection_state_change(None);
            connection.close();
        }
    }
}

pub fn spawn_service(
    our_key: String,
    state: Signal<AppState>,
    outgoing: UnboundedSender<(String, CallSignal)>,
) -> UnboundedSender<Command> {
    let (tx, mut rx) = unbounded_channel();
    let service_tx = tx.clone();
    spawn(async move {
        let mut actor = Actor {
            our_key,
            device: Uuid::new_v4(),
            state,
            outgoing,
            tx: service_tx,
            session: None,
            finished: HashMap::new(),
        };
        let mut tick = tokio::time::interval(Duration::from_millis(150));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                command = rx.recv() => match command {
                    Some(Command::Action(action)) => actor.action(action),
                    Some(Command::Incoming(incoming)) => actor.incoming(incoming),
                    Some(Command::Native { call_id, event }) => actor.native(call_id, event),
                    Some(Command::Published { call_id, accepted: false }) if actor.session.as_ref().is_some_and(|s| s.id == call_id) => actor.finish(EndReason::Failed, Some("The call signal could not reach a relay.")),
                    Some(Command::Published { .. }) => {},
                    None => break,
                },
                _ = tick.tick() => actor.tick(),
            }
        }
    });
    tx
}

struct Actor {
    our_key: String,
    device: Uuid,
    state: Signal<AppState>,
    outgoing: UnboundedSender<(String, CallSignal)>,
    tx: UnboundedSender<Command>,
    session: Option<Session>,
    finished: HashMap<(String, Uuid), Instant>,
}

fn guild_busy(state: &AppState) -> bool {
    state.server_voice_channel().is_some()
        || state.voice.channel_id.is_some()
        || matches!(
            state.voice.phase,
            VoicePhase::Connecting | VoicePhase::Connected
        )
}

impl Actor {
    fn emit(&self, peer: &str, id: Uuid, target: Option<Uuid>, body: Body) {
        let signal = CallSignal {
            version: 1,
            call_id: id,
            device: self.device,
            target,
            sent_at: chrono::Utc::now().timestamp(),
            body,
        };
        if self.outgoing.send((peer.into(), signal)).is_err() {
            tracing::warn!("DM call signaling service stopped");
        }
    }

    fn send(&self, body: Body) {
        if let Some(s) = &self.session {
            self.emit(&s.peer, s.id, s.remote_device, body);
        }
    }

    fn update_view(&mut self) {
        self.state.write().dm_call = self.session.as_ref().map(|s| CallView {
            peer: s.peer.clone(),
            phase: s.phase,
            microphone_error: s
                .audio
                .as_ref()
                .and_then(|audio| audio.microphone_error.clone()),
        });
    }

    fn finish(&mut self, reason: EndReason, error: Option<&str>) {
        self.send(Body::End { reason });
        self.clear();
        if let Some(error) = error {
            self.state.write().error_toast = Some(error.into());
        }
    }

    fn clear(&mut self) {
        if let Some(s) = self.session.take() {
            self.remember(s.peer.clone(), s.id);
        }
        self.update_view();
    }

    fn remember(&mut self, peer: String, id: Uuid) {
        if self.finished.len() >= 256
            && let Some(oldest) = self
                .finished
                .iter()
                .min_by_key(|(_, time)| **time)
                .map(|(key, _)| key.clone())
        {
            self.finished.remove(&oldest);
        }
        self.finished.insert((peer, id), Instant::now());
    }

    fn action(&mut self, action: Action) {
        match action {
            Action::Start(peer) => {
                if self.session.is_some() || guild_busy(&self.state.peek()) {
                    self.state.write().error_toast =
                        Some("End your current voice session before starting a DM call.".into());
                    return;
                }
                if peer == self.our_key || peer.len() != 64 || hex::decode(&peer).is_err() {
                    return;
                }
                self.session = Some(Session {
                    id: Uuid::new_v4(),
                    peer,
                    remote_device: None,
                    phase: Phase::Ringing,
                    deadline: Some(Instant::now() + Duration::from_secs(45)),
                    connection: None,
                    audio: None,
                    job: None,
                    remote_answer_set: false,
                });
                self.send(Body::Invite);
                self.update_view();
            }
            Action::Accept => {
                if guild_busy(&self.state.peek()) {
                    self.finish(EndReason::Busy, None);
                    return;
                }
                if let Some(s) = &mut self.session
                    && s.phase == Phase::Incoming
                {
                    s.phase = Phase::Connecting;
                    s.deadline = Some(Instant::now() + Duration::from_secs(60));
                    self.send(Body::Accept);
                    self.update_view();
                }
            }
            Action::End => {
                let reason = if self
                    .session
                    .as_ref()
                    .is_some_and(|s| s.phase == Phase::Incoming)
                {
                    EndReason::Declined
                } else {
                    EndReason::Hangup
                };
                self.finish(reason, None);
            }
        }
    }

    fn incoming(&mut self, incoming: Incoming) {
        let Incoming { author, signal } = incoming;
        if author == self.our_key {
            return;
        }
        let offset = self.state.peek().clock_offset(&author);
        let now = crate::nostr::calls::sender_now(chrono::Utc::now().timestamp(), offset);
        if let Err(reason) = signal.validate(now) {
            tracing::warn!(%reason, call_id = %signal.call_id, clock_offset = offset, "Rejected queued DM call signal");
            return;
        }
        if !self.state.peek().contacts.contains(&author)
            && !self
                .state
                .peek()
                .dms
                .iter()
                .any(|dm| dm.other_pubkey == author)
        {
            return;
        }
        if signal.target.is_some_and(|target| target != self.device) {
            if matches!(signal.body, Body::Offer { .. })
                && self.session.as_ref().is_some_and(|s| {
                    s.id == signal.call_id
                        && s.peer == author
                        && matches!(s.phase, Phase::Incoming | Phase::Connecting)
                })
            {
                self.clear();
            }
            return;
        }
        if self
            .finished
            .contains_key(&(author.clone(), signal.call_id))
        {
            return;
        }
        if matches!(signal.body, Body::Invite) {
            if self.finished.len() >= 256 {
                return;
            }
            if self
                .session
                .as_ref()
                .is_some_and(|s| s.peer == author && s.id == signal.call_id)
            {
                return;
            }
            // A simultaneous outgoing call yields on the same key ordering at both ends.
            if self
                .session
                .as_ref()
                .is_some_and(|s| s.peer == author && s.phase == Phase::Ringing)
                && self.our_key > author
            {
                self.finish(EndReason::Hangup, None);
            }
            if self.session.is_some() || guild_busy(&self.state.peek()) {
                self.emit(
                    &author,
                    signal.call_id,
                    Some(signal.device),
                    Body::End {
                        reason: EndReason::Busy,
                    },
                );
                self.remember(author, signal.call_id);
                return;
            }
            self.session = Some(Session {
                id: signal.call_id,
                peer: author,
                remote_device: Some(signal.device),
                phase: Phase::Incoming,
                deadline: Some(Instant::now() + Duration::from_secs(45)),
                connection: None,
                audio: None,
                job: None,
                remote_answer_set: false,
            });
            self.update_view();
            return;
        }
        let Some(s) = &mut self.session else {
            if matches!(signal.body, Body::End { .. }) {
                self.remember(author, signal.call_id);
            }
            return;
        };
        if s.id != signal.call_id || s.peer != author {
            if matches!(signal.body, Body::End { .. }) {
                self.remember(author, signal.call_id);
            }
            return;
        }
        if s.remote_device
            .is_some_and(|device| device != signal.device)
        {
            return;
        }
        match signal.body {
            Body::Accept if s.phase == Phase::Ringing => {
                s.remote_device = Some(signal.device);
                s.phase = Phase::Connecting;
                s.deadline = Some(Instant::now() + Duration::from_secs(60));
                self.negotiate(None);
            }
            Body::Offer { sdp } if s.phase == Phase::Connecting && s.connection.is_none() => {
                self.negotiate(Some(sdp))
            }
            Body::Answer { sdp }
                if s.phase == Phase::Connecting
                    && s.connection.is_some()
                    && s.job.is_none()
                    && !s.remote_answer_set =>
            {
                if let Some(peer) = s.connection.clone() {
                    s.remote_answer_set = true;
                    let tx = self.tx.clone();
                    let id = s.id;
                    s.job = Some(spawn(async move {
                        let result = apply_description(&peer, &sdp, SdpType::Answer).await;
                        if tx
                            .send(Command::Native {
                                call_id: id,
                                event: NativeEvent::RemoteDescription(result),
                            })
                            .is_err()
                        {
                            peer.close();
                        }
                    }));
                }
            }
            Body::End { reason } => {
                self.clear();
                if matches!(
                    reason,
                    EndReason::Busy | EndReason::Declined | EndReason::Failed
                ) {
                    self.state.write().error_toast = Some(
                        match reason {
                            EndReason::Busy => "This contact is busy.",
                            EndReason::Declined => "The call was declined.",
                            _ => "The contact could not connect.",
                        }
                        .into(),
                    );
                }
            }
            _ => {}
        }
        self.update_view();
    }

    fn negotiate(&mut self, offer: Option<String>) {
        if let Err(error) = self.prepare(offer) {
            self.finish(EndReason::Failed, Some(&error));
        }
    }

    fn prepare(&mut self, offer: Option<String>) -> Result<(), String> {
        let factory = PeerConnectionFactory::default();
        let connection = factory
            .create_peer_connection(ice_configuration()?)
            .map_err(|e| e.to_string())?;
        let audio = match super::voice::direct::Audio::start(&factory, self.state) {
            Ok(audio) => audio,
            Err(error) => {
                connection.close();
                return Err(error);
            }
        };
        if let Err(error) = connection.add_track(audio.track.clone().into(), &["dm-voice"]) {
            connection.close();
            return Err(error.to_string());
        }
        let Some(s) = &mut self.session else {
            connection.close();
            return Err("Call ended".into());
        };
        let id = s.id;
        let tx = self.tx.clone();
        connection.on_connection_state_change(Some(Box::new(move |state| {
            if tx
                .send(Command::Native {
                    call_id: id,
                    event: NativeEvent::State(state),
                })
                .is_err()
            {
                tracing::debug!("DM call receiver stopped");
            }
        })));
        let tx = self.tx.clone();
        connection.on_track(Some(Box::new(move |event| {
            if let MediaStreamTrack::Audio(track) = event.track
                && tx
                    .send(Command::Native {
                        call_id: id,
                        event: NativeEvent::Track(track),
                    })
                    .is_err()
            {
                tracing::debug!("DM call receiver stopped");
            }
        })));
        s.audio = Some(audio);
        s.connection = Some(connection.clone());
        let tx = self.tx.clone();
        s.job = Some(spawn(async move {
            let result = tokio::time::timeout(
                Duration::from_secs(20),
                local_description(&connection, offer),
            )
            .await
            .map_err(|_| "ICE discovery timed out. Check your STUN/TURN configuration.".to_string())
            .and_then(|result| result);
            if tx
                .send(Command::Native {
                    call_id: id,
                    event: NativeEvent::Description(result),
                })
                .is_err()
            {
                connection.close();
            }
        }));
        self.update_view();
        Ok(())
    }

    fn native(&mut self, id: Uuid, event: NativeEvent) {
        let Some(s) = &mut self.session else {
            return;
        };
        if s.id != id {
            return;
        }
        match event {
            NativeEvent::State(PeerConnectionState::Connected) => {
                s.phase = Phase::Connected;
                s.deadline = None;
            }
            NativeEvent::State(PeerConnectionState::Disconnected) => {
                if s.phase != Phase::Reconnecting {
                    s.phase = Phase::Reconnecting;
                    s.deadline = Some(Instant::now() + Duration::from_secs(15));
                }
            }
            NativeEvent::State(PeerConnectionState::Failed | PeerConnectionState::Closed) => self
                .finish(
                    EndReason::Failed,
                    Some("Voice connection ended. Check your network and TURN configuration."),
                ),
            NativeEvent::Track(track) => {
                if let Some(audio) = &mut s.audio {
                    audio.receive(track, s.peer.clone());
                }
            }
            NativeEvent::Description(result) => {
                s.job = None;
                match result {
                    Ok((answer, sdp)) => self.send(if answer {
                        Body::Answer { sdp }
                    } else {
                        Body::Offer { sdp }
                    }),
                    Err(error) => self.finish(EndReason::Failed, Some(&error)),
                }
            }
            NativeEvent::RemoteDescription(result) => {
                s.job = None;
                if let Err(error) = result {
                    self.finish(EndReason::Failed, Some(&error));
                }
            }
            _ => {}
        }
        self.update_view();
    }

    fn tick(&mut self) {
        self.finished
            .retain(|_, ended| ended.elapsed() < Duration::from_secs(120));
        if self.session.as_ref().is_some_and(|s| {
            s.deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
        }) {
            self.finish(
                EndReason::Timeout,
                Some("The call timed out. The contact may be offline or unreachable."),
            );
        } else if self.session.is_some() && guild_busy(&self.state.peek()) {
            self.finish(
                EndReason::Busy,
                Some("DM call ended because a voice channel was joined."),
            );
        } else if let Some(s) = &mut self.session
            && let Some(audio) = &mut s.audio
        {
            let previous_error = audio.microphone_error.clone();
            audio.update(self.state);
            if previous_error != audio.microphone_error {
                self.update_view();
            }
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ServerConfig {
    urls: Vec<String>,
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: String,
}

fn ice_configuration() -> Result<RtcConfiguration, String> {
    let raw = std::env::var("DIOXUSFUN_DM_ICE_SERVERS")
        .unwrap_or_else(|_| "[{\"urls\":[\"stun:stun.cloudflare.com:3478\"]}]".into());
    let relay_only = std::env::var("DIOXUSFUN_DM_RELAY_ONLY").is_ok_and(|value| value == "1");
    parse_ice_configuration(&raw, relay_only)
}

fn parse_ice_configuration(raw: &str, relay_only: bool) -> Result<RtcConfiguration, String> {
    let servers: Vec<ServerConfig> = serde_json::from_str(raw)
        .map_err(|_| "Invalid DIOXUSFUN_DM_ICE_SERVERS JSON".to_string())?;
    if servers.len() > 8
        || servers.iter().any(|s| {
            s.urls.is_empty()
                || s.urls.len() > 8
                || s.urls.iter().any(|url| {
                    url.len() > 512
                        || !["stun:", "stuns:", "turn:", "turns:"]
                            .iter()
                            .any(|scheme| url.starts_with(scheme))
                })
        })
    {
        return Err("Invalid STUN/TURN server list".into());
    }
    if relay_only
        && !servers.iter().any(|s| {
            s.urls
                .iter()
                .any(|url| url.starts_with("turn:") || url.starts_with("turns:"))
        })
    {
        return Err("Relay-only calls require a TURN server.".into());
    }
    Ok(RtcConfiguration {
        ice_servers: servers
            .into_iter()
            .map(|s| IceServer {
                urls: s.urls,
                username: s.username,
                password: s.password,
            })
            .collect(),
        continual_gathering_policy: ContinualGatheringPolicy::GatherOnce,
        ice_transport_type: if relay_only {
            IceTransportsType::Relay
        } else {
            IceTransportsType::All
        },
    })
}

async fn apply_description(peer: &PeerConnection, sdp: &str, kind: SdpType) -> Result<(), String> {
    let description = SessionDescription::parse(sdp, kind).map_err(|e| e.to_string())?;
    peer.set_remote_description(description)
        .await
        .map_err(|e| e.to_string())
}

async fn local_description(
    peer: &PeerConnection,
    offer: Option<String>,
) -> Result<(bool, String), String> {
    let candidates = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
    let gathered = candidates.clone();
    peer.on_ice_candidate(Some(Box::new(move |candidate| {
        let mut gathered = gathered.lock();
        if gathered.len() < 64 {
            gathered.push(candidate.to_string());
        }
    })));
    struct Gathering(PeerConnection);
    impl Drop for Gathering {
        fn drop(&mut self) {
            self.0.on_ice_candidate(None);
        }
    }
    let _gathering = Gathering(peer.clone());
    let answer = offer.is_some();
    let description = if let Some(offer) = offer {
        apply_description(peer, &offer, SdpType::Offer).await?;
        peer.create_answer(AnswerOptions::default()).await
    } else {
        peer.create_offer(OfferOptions {
            offer_to_receive_audio: true,
            ..Default::default()
        })
        .await
    }
    .map_err(|e| e.to_string())?;
    // The SDK exposes only the committed SDP; an offer is pending until answered.
    let mut description_text = description.to_string();
    peer.set_local_description(description)
        .await
        .map_err(|e| e.to_string())?;
    while peer.ice_gathering_state() != IceGatheringState::Complete {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    for candidate in candidates.lock().iter() {
        description_text.push_str("a=");
        description_text.push_str(candidate);
        description_text.push_str("\r\n");
    }
    description_text.push_str("a=end-of-candidates\r\n");
    if description_text.len() > crate::nostr::calls::MAX_SDP {
        return Err("Voice description is too large".into());
    }
    Ok((answer, description_text))
}

#[component]
pub fn CallButton(peer: String) -> Element {
    let state = use_app_state();
    let nostr = use_context::<crate::nostr::service::NostrTx>();
    let unavailable = state.read().dm_call.is_some() || guild_busy(&state.read());
    let opacity = if unavailable { "0.45" } else { "1" };
    rsx! {
        dioxus_grid_layout::NoDrag { button {
            class: "transition-colors hover:brightness-125",
            style: "display: inline-flex; align-items: center; gap: 7px; margin-left: auto; padding: 7px 11px; border: 1px solid var(--border); border-radius: 8px; background-color: var(--bg2); color: var(--text-muted); font-size: 12px; opacity: {opacity};",
            disabled: unavailable,
            title: if unavailable { "End your current voice session to start a call" } else { "Start a private voice call" },
            onclick: move |_| nostr.send(crate::nostr::service::NostrCmd::Call(Action::Start(peer.clone()))),
            span { style: "display: flex;", aria_hidden: "true", dangerous_inner_html: super::icons::PHONE }
            "Call"
        } }
    }
}

#[cfg(test)]
#[path = "dm_call_tests.rs"]
mod tests;

#[component]
pub fn CallAlert() -> Element {
    let state = use_app_state();
    let phase = use_memo(move || state.read().dm_call.as_ref().map(|call| call.phase));
    let mut ringing = use_signal(|| None::<Task>);
    let mut previous = use_signal(|| None::<Phase>);
    use_effect(move || {
        let current = phase();
        if let Some(task) = ringing.write().take() {
            task.cancel();
        }
        crate::native_sounds::call_ring(None);
        let tone = match current {
            Some(Phase::Incoming) => Some("call-incoming"),
            Some(Phase::Ringing) => Some("call-outgoing"),
            _ => None,
        };
        if let Some(tone) = tone {
            ringing.set(Some(spawn(async move {
                loop {
                    crate::native_sounds::call_ring(Some(tone));
                    tokio::time::sleep(Duration::from_secs(3)).await;
                }
            })));
        } else if current == Some(Phase::Connected) && *previous.peek() == Some(Phase::Connecting) {
            super::sounds::sfx("call-connected");
        } else if current.is_none() && previous.peek().is_some() {
            super::sounds::sfx("call-ended");
        }
        previous.set(current);
    });
    use_drop(move || {
        if let Some(task) = ringing.peek().as_ref() {
            task.cancel();
        }
        crate::native_sounds::call_ring(None);
    });
    rsx! {}
}

#[component]
pub fn CallPanel(#[props(default)] embedded: bool) -> Element {
    let mut state = use_app_state();
    let mut settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let nostr = use_context::<crate::nostr::service::NostrTx>();
    let Some(call) = state.read().dm_call.clone() else {
        return rsx! {};
    };
    let name = state.read().display_name(&call.peer);
    let muted = state.read().voice.muted;
    let mic_volume = state.read().mic_volume;
    let deafened = state.read().voice.deafened;
    let volume = state
        .read()
        .user_volumes
        .get(&call.peer)
        .copied()
        .unwrap_or(100)
        .min(200);
    let locally_muted = state.read().user_muted.contains(&call.peer);
    let status_color = match call.phase {
        Phase::Connected => "var(--success)",
        Phase::Reconnecting => "var(--warn)",
        _ => "var(--accent)",
    };
    let hint = match call.phase {
        Phase::Incoming => "Your microphone stays off until you accept.",
        Phase::Ringing => "Waiting for an answer.",
        Phase::Connecting => "Establishing a secure voice connection.",
        Phase::Connected => "Only you and this contact can hear this call.",
        Phase::Reconnecting => "Trying to restore your voice connection.",
    };
    let accept = nostr.clone();
    let audio_peer = call.peer.clone();
    let placement = if embedded {
        "position: relative; width: 100%; border-radius: 0;"
    } else {
        "position: fixed; top: 72px; right: 20px; z-index: 10000; width: min(356px, calc(100vw - 40px)); max-height: calc(100vh - 96px); overflow-y: auto; border-radius: 14px;"
    };
    rsx! {
        dioxus_grid_layout::NoDrag {
            section {
                class: "dxf-pop-in",
                style: "{placement} border-width: 1px; border-style: solid; border-color: var(--border-strong); background-color: var(--panel-solid, var(--bg2)); color: var(--text);",
                role: "region", aria_label: "Private voice call",
                div { style: "height: 3px; background-color: {status_color};" }
                div { style: "padding: 16px;",
                    div { style: "display: flex; align-items: center; gap: 12px;",
                        super::profiles::Avatar { pubkey: call.peer.clone(), name: name.clone(), size: "w-10 h-10", text: "text-sm" }
                        div { style: "flex: 1; min-width: 0;",
                            div { style: "display: flex; align-items: center; gap: 5px; margin-bottom: 3px; font-size: 10px; letter-spacing: 0.06em; color: var(--text-dim);",
                                span { style: "display: flex;", aria_hidden: "true", dangerous_inner_html: super::icons::SHIELD }
                                "PRIVATE VOICE CALL"
                            }
                            div { style: "overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 16px; font-weight: 650;", title: "{name}", "{name}" }
                        }
                    }
                    div { style: "display: flex; align-items: center; gap: 7px; margin-top: 15px; font-size: 12px; color: {status_color};", role: "status", aria_live: "polite",
                        span { style: "width: 6px; height: 6px; flex-shrink: 0; border-radius: 50%; background-color: {status_color};" }
                        "{call.phase.label()}"
                    }
                    p { style: "margin: 6px 0 0; font-size: 11px; line-height: 1.5; color: var(--text-dim);", "{hint}" }
                    if let Some(error) = &call.microphone_error {
                        p { role: "status", style: "margin: 6px 0 0; font-size: 11px; color: var(--warn);",
                            "Microphone unavailable — retrying. {error}"
                        }
                    }
                }
                div { style: "display: flex; gap: 8px; padding: 0 16px 16px;",
                    if call.phase == Phase::Incoming {
                        CallControl { icon: super::icons::PHONE, label: "Accept", tone: "accept",
                            on_press: move |_| accept.send(crate::nostr::service::NostrCmd::Call(Action::Accept)) }
                        CallControl { icon: super::icons::PHONE_HANGUP, label: "Decline", tone: "danger",
                            on_press: move |_| nostr.send(crate::nostr::service::NostrCmd::Call(Action::End)) }
                    } else {
                        CallControl {
                            icon: if muted || deafened { super::icons::MIC_OFF } else { super::icons::MIC },
                            label: if muted { "Unmute mic" } else { "Mute mic" },
                            active: muted || deafened, toggle: true, disabled: deafened,
                            on_press: move |_| { let value = !state.peek().voice.muted; state.write().voice.muted = value; }
                        }
                        CallControl {
                            icon: if deafened { super::icons::HEADPHONES_OFF } else { super::icons::HEADPHONES },
                            label: if deafened { "Undeafen" } else { "Deafen" },
                            active: deafened, toggle: true,
                            on_press: move |_| {
                                let mut s = state.write();
                                if s.voice.deafened { s.voice.muted = s.voice.muted_before_deafen; s.voice.deafened = false; }
                                else { s.voice.muted_before_deafen = s.voice.muted; s.voice.deafened = true; s.voice.muted = true; }
                            }
                        }
                        CallControl { icon: super::icons::PHONE_HANGUP,
                            label: if call.phase == Phase::Ringing { "Cancel" } else { "End call" }, tone: "danger",
                            on_press: move |_| nostr.send(crate::nostr::service::NostrCmd::Call(Action::End)) }
                    }
                }
                if call.phase != Phase::Incoming {
                    div { style: "padding: 12px 16px; border-top-width: 1px; border-top-style: solid; border-top-color: var(--border); background-color: var(--bg2);",
                        div { style: "display: flex; align-items: center; justify-content: space-between; margin-bottom: 8px; font-size: 11px;",
                            label { r#for: "dm-microphone-volume", style: "color: var(--text-muted);", "Microphone volume" }
                            span { style: "font-variant-numeric: tabular-nums; color: var(--text-dim);", "{mic_volume}%" }
                        }
                        input { id: "dm-microphone-volume", r#type: "range", min: "0", max: "200", step: "5", value: "{mic_volume}",
                            style: "width: 100%; min-width: 0; accent-color: var(--accent); cursor: pointer;",
                            aria_label: "Microphone volume", aria_valuetext: "{mic_volume} percent",
                            oninput: move |event| {
                                if let Ok(value) = event.value().parse::<u16>() {
                                    let volume = value.min(200);
                                    state.write().mic_volume = volume;
                                    settings.write().mic_volume = volume;
                                }
                            },
                            onchange: move |_| crate::settings::save(&settings.peek()),
                        }
                        p { style: "margin: 6px 0 0; font-size: 10px; color: var(--text-dim);",
                            "Changes how loud your contact hears you."
                        }
                    }
                    div { style: "padding: 12px 16px; border-top-width: 1px; border-top-style: solid; border-top-color: var(--border); background-color: var(--bg2);",
                        div { style: "display: flex; align-items: center; justify-content: space-between; margin-bottom: 8px; font-size: 11px;",
                            label { r#for: "dm-call-volume", style: "color: var(--text-muted);", "Call volume" }
                            span { style: "font-variant-numeric: tabular-nums; color: var(--text-dim);", if locally_muted || deafened { "Muted" } else { "{volume}%" } }
                        }
                        div { style: "display: flex; align-items: center; gap: 10px;",
                            button {
                                class: "hover:brightness-125", title: if locally_muted { "Unmute contact" } else { "Mute contact" },
                                aria_label: if locally_muted { "Unmute contact" } else { "Mute contact" }, aria_pressed: locally_muted.to_string(),
                                style: "display: flex; padding: 5px; border-radius: 5px; color: var(--text-muted);",
                                onclick: move |_| {
                                    let mut s = state.write();
                                    if !s.user_muted.remove(&audio_peer) { s.user_muted.insert(audio_peer.clone()); }
                                },
                                span { aria_hidden: "true", dangerous_inner_html: if locally_muted || deafened { super::icons::SPEAKER_OFF } else { super::icons::SPEAKER } }
                            }
                            input { id: "dm-call-volume", r#type: "range", min: "0", max: "200", step: "5", value: "{volume}",
                                style: "width: 100%; min-width: 0; accent-color: var(--accent); cursor: pointer;", disabled: deafened,
                                aria_label: "Call volume", aria_valuetext: "{volume} percent",
                                oninput: move |event| {
                                    if let Ok(value) = event.value().parse::<u32>() {
                                        let mut s = state.write();
                                        s.user_volumes.insert(call.peer.clone(), value.min(200));
                                        if value > 0 { s.user_muted.remove(&call.peer); }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn CallControl(
    icon: &'static str,
    #[props(into)] label: String,
    #[props(default = "neutral")] tone: &'static str,
    #[props(default)] active: bool,
    #[props(default)] toggle: bool,
    #[props(default)] disabled: bool,
    on_press: EventHandler<()>,
) -> Element {
    let opacity = if disabled { "0.5" } else { "1" };
    let (color, background, border) = if tone == "danger" || active {
        (
            "var(--danger)",
            "color-mix(in srgb, var(--danger) 10%, transparent)",
            "color-mix(in srgb, var(--danger) 30%, transparent)",
        )
    } else if tone == "accept" {
        (
            "var(--success)",
            "color-mix(in srgb, var(--success) 12%, transparent)",
            "color-mix(in srgb, var(--success) 40%, transparent)",
        )
    } else {
        ("var(--text-muted)", "var(--bg2)", "var(--border)")
    };
    rsx! {
        button {
            class: "transition-colors hover:brightness-125",
            style: "display: flex; flex: 1; flex-direction: column; align-items: center; justify-content: center; gap: 7px; min-height: 58px; padding: 9px 5px; border-width: 1px; border-style: solid; border-color: {border}; border-radius: 9px; background-color: {background}; color: {color}; font-size: 11px; font-weight: 550; opacity: {opacity};",
            disabled, title: if disabled { "Undeafen before enabling your microphone" } else { label.as_str() },
            aria_label: label.clone(), aria_pressed: if toggle { Some(active.to_string()) } else { None },
            onclick: move |_| on_press.call(()),
            span { style: "display: flex;", aria_hidden: "true", dangerous_inner_html: icon }
            "{label}"
        }
    }
}
