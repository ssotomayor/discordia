use crate::protocol::{
    ActivityAudioCredential, ActivityAudioRole, ActivityInstance, ClientMessage, IceServer, Id,
    ServerMessage, activity_audio_epoch,
};
use discordia_nostr::activity_audio::{AudioKey, KeyContext};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::{Duration, Instant};

pub const JS: &str = include_str!("../assets/activity-audio.js");

#[derive(Default)]
pub struct Listener {
    attempt: Option<Attempt>,
}

struct Attempt {
    instance: ActivityInstance,
    epoch: String,
    request: Id,
    since: Instant,
    grant: Option<Grant>,
    key: Option<AudioKey>,
    started: bool,
}

struct Grant {
    url: String,
    token: ActivityAudioCredential,
    ice: Vec<IceServer>,
    deadline: Instant,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Connection {
    request: Id,
    url: String,
    token: ActivityAudioCredential,
    publisher: String,
    key_hex: String,
    ice_servers: Vec<IceServer>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    __dxaudio: String,
    context: KeyContext,
    ciphertext: String,
}

impl Listener {
    pub fn begin(&mut self, instance: ActivityInstance) -> Vec<ClientMessage> {
        let request = Id::new_v4();
        let epoch = activity_audio_epoch(&instance);
        let messages = vec![
            ClientMessage::RequestActivityAudio {
                instance_id: instance.id,
                request_id: request,
                epoch: epoch.clone(),
                role: ActivityAudioRole::Listener,
            },
            ClientMessage::ActivitySend {
                instance_id: instance.id,
                to: Some(instance.app.clone()),
                data: json!({"__dxaudio":"key_request", "request_id":request, "epoch":epoch})
                    .to_string(),
            },
        ];
        self.attempt = Some(Attempt {
            instance,
            epoch,
            request,
            since: Instant::now(),
            grant: None,
            key: None,
            started: false,
        });
        messages
    }

    pub fn stop(&mut self) {
        self.attempt = None;
    }

    pub fn pending(&self) -> bool {
        self.attempt.as_ref().is_some_and(|a| !a.started)
    }

    pub fn active(&self) -> bool {
        self.attempt.is_some()
    }

    pub fn expired(&self) -> bool {
        self.attempt
            .as_ref()
            .is_some_and(|a| !a.started && a.since.elapsed() >= Duration::from_secs(20))
    }

    pub fn matches(&self, request: Id) -> bool {
        self.attempt.as_ref().is_some_and(|a| a.request == request)
    }

    pub fn changed(&self, instance: &ActivityInstance) -> bool {
        self.attempt.as_ref().is_some_and(|a| {
            a.instance.id != instance.id
                || a.instance.channel_id != instance.channel_id
                || a.instance.app != instance.app
                || a.epoch != activity_audio_epoch(instance)
        })
    }

    pub fn token(&mut self, message: ServerMessage) -> Option<&'static str> {
        let a = self.attempt.as_mut()?;
        match message {
            ServerMessage::ActivityAudioError {
                instance_id,
                request_id,
                ..
            } if instance_id == a.instance.id && request_id == a.request => {
                Some("permission_denied")
            }
            ServerMessage::ActivityAudioToken {
                instance_id,
                request_id,
                epoch,
                role,
                livekit_url,
                token,
                expires_in_secs,
                ice_servers,
                ..
            } if instance_id == a.instance.id
                && request_id == a.request
                && epoch == a.epoch
                && role == ActivityAudioRole::Listener
                && !a.started
                && !self::invalid_url(&livekit_url)
                && (1..=60).contains(&expires_in_secs) =>
            {
                a.grant = Some(Grant {
                    url: livekit_url,
                    token,
                    ice: ice_servers,
                    deadline: Instant::now() + Duration::from_secs(expires_in_secs),
                });
                None
            }
            _ => None,
        }
    }

    pub fn key(&mut self, from: &str, data: &str, secret: &secp256k1::SecretKey) {
        let Some(a) = self
            .attempt
            .as_mut()
            .filter(|a| !a.started && a.instance.app == from)
        else {
            return;
        };
        let Ok(envelope) = serde_json::from_str::<Envelope>(data) else {
            return;
        };
        let context = &envelope.context;
        if envelope.__dxaudio != "key"
            || context.instance != *a.instance.id.as_bytes()
            || context.channel != *a.instance.channel_id.as_bytes()
            || context.request != *a.request.as_bytes()
            || hex::encode(context.epoch) != a.epoch
            || context.publication == [0; 16]
        {
            return;
        }
        if let Ok(key) =
            discordia_nostr::activity_audio::open(secret, from, context, &envelope.ciphertext)
        {
            a.key = Some(key);
        }
    }

    pub fn ready(&mut self) -> Option<Connection> {
        let a = self.attempt.as_mut()?;
        if a.started
            || a.key.is_none()
            || a.grant
                .as_ref()
                .is_none_or(|g| g.deadline <= Instant::now() + Duration::from_secs(2))
        {
            return None;
        }
        let grant = a.grant.take()?;
        let key = a.key.take()?;
        a.started = true;
        Some(Connection {
            request: a.request,
            url: grant.url,
            token: grant.token,
            publisher: format!("music-service:{}", a.instance.app),
            key_hex: hex::encode(key.as_bytes()),
            ice_servers: grant.ice,
        })
    }
}

fn invalid_url(value: &str) -> bool {
    !url::Url::parse(value).is_ok_and(|url| {
        matches!(url.scheme(), "ws" | "wss")
            && url.username().is_empty()
            && url.password().is_none()
            && url.host_str().is_some()
    })
}

pub fn stop() {
    document_eval("window.__dxfActivityAudio?.stop();");
}

pub fn set_volume(value: f64) {
    if value.is_finite() {
        document_eval(&format!(
            "window.__dxfActivityAudio?.setVolume({});",
            value.clamp(0.0, 1.0)
        ));
    }
}

fn document_eval(script: &str) {
    dioxus::document::eval(script);
}

pub fn start(connection: Connection) {
    use dioxus::prelude::*;
    let request = connection.request;
    let Ok(options) = serde_json::to_string(&connection) else {
        return;
    };
    let source = format!(
        "{}\n{}\nwindow.__dxfActivityAudio.start({}, status => dioxus.send(status));",
        crate::app::livekit_bundle_js(),
        JS,
        options
    );
    spawn(async move {
        let mut eval = document::eval(&source);
        while let Ok(status) = eval.recv::<serde_json::Value>().await {
            let Some(request) = status
                .get("request")
                .and_then(|r| r.as_str())
                .and_then(|r| r.parse().ok())
            else {
                continue;
            };
            let Some(status) = status.get("status").and_then(|s| s.as_str()) else {
                continue;
            };
            crate::features::activities::inbound(
                crate::features::activities::Inbound::AudioState {
                    request,
                    status: status.into(),
                },
            );
            if matches!(
                status,
                "stopped"
                    | "disconnected"
                    | "connection_failed"
                    | "encryption_failed"
                    | "connection_timeout"
            ) {
                return;
            }
        }
        crate::features::activities::inbound(crate::features::activities::Inbound::AudioState {
            request,
            status: "connection_failed".into(),
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{ActivityParticipant, ActivityRole};
    use secp256k1::{Secp256k1, SecretKey};

    fn identity(value: u8) -> (SecretKey, String) {
        let secret = SecretKey::from_slice(&[value; 32]).unwrap();
        let public = hex::encode(secret.x_only_public_key(&Secp256k1::new()).0.serialize());
        (secret, public)
    }

    fn setup() -> (Listener, ActivityInstance, SecretKey, SecretKey, Id) {
        let (app, app_key) = identity(1);
        let (me, my_key) = identity(2);
        let instance = ActivityInstance {
            id: Id::new_v4(),
            channel_id: Id::new_v4(),
            app: app_key,
            max_players: 16,
            participants: vec![ActivityParticipant {
                pubkey: my_key,
                role: ActivityRole::Player,
            }],
        };
        let mut listener = Listener::default();
        listener.begin(instance.clone());
        let request = listener.attempt.as_ref().unwrap().request;
        (listener, instance, app, me, request)
    }

    fn grant(instance: &ActivityInstance, request: Id) -> ServerMessage {
        ServerMessage::ActivityAudioToken {
            instance_id: instance.id,
            request_id: request,
            epoch: activity_audio_epoch(instance),
            role: ActivityAudioRole::Listener,
            livekit_url: "ws://127.0.0.1:1234".into(),
            livekit_urls: vec![],
            token: "private-test-token".to_string().into(),
            expires_in_secs: 60,
            ice_servers: vec![],
        }
    }

    fn envelope(instance: &ActivityInstance, app: &SecretKey, request: Id) -> String {
        let mut epoch = [0; 16];
        hex::decode_to_slice(activity_audio_epoch(instance), &mut epoch).unwrap();
        let context = KeyContext {
            instance: *instance.id.as_bytes(),
            channel: *instance.channel_id.as_bytes(),
            epoch,
            request: *request.as_bytes(),
            publication: *Id::new_v4().as_bytes(),
        };
        let ciphertext = discordia_nostr::activity_audio::seal(
            app,
            &instance.participants[0].pubkey,
            &context,
            &AudioKey::generate(),
        )
        .unwrap();
        json!({"__dxaudio":"key", "context":context, "ciphertext":ciphertext}).to_string()
    }

    #[test]
    fn key_and_token_can_arrive_in_either_order_and_start_only_once() {
        for token_first in [false, true] {
            let (mut listener, instance, app, me, request) = setup();
            let data = envelope(&instance, &app, request);
            if token_first {
                listener.token(grant(&instance, request));
            } else {
                listener.key(&instance.app, &data, &me);
            }
            assert!(listener.ready().is_none());
            if token_first {
                listener.key(&instance.app, &data, &me);
            } else {
                listener.token(grant(&instance, request));
            }
            let connection = listener.ready().unwrap();
            assert_eq!(
                connection.publisher,
                format!("music-service:{}", instance.app)
            );
            assert_eq!(connection.key_hex.len(), 64);
            assert!(listener.ready().is_none());
            listener.stop();
            listener.token(grant(&instance, request));
            listener.key(&instance.app, &data, &me);
            assert!(listener.ready().is_none());
        }
    }

    #[test]
    fn stale_credentials_wrong_sender_and_expired_grants_cannot_start_playback() {
        let (mut listener, instance, app, me, request) = setup();
        let data = envelope(&instance, &app, request);
        listener.begin(instance.clone());
        listener.token(grant(&instance, request));
        listener.key(&instance.app, &data, &me);
        assert!(listener.ready().is_none());
        let request = listener.attempt.as_ref().unwrap().request;
        let data = envelope(&instance, &app, request);
        listener.token(grant(&instance, request));
        listener.key("another-app", &data, &me);
        assert!(listener.ready().is_none());
        listener.key(&instance.app, &data, &me);
        listener
            .attempt
            .as_mut()
            .unwrap()
            .grant
            .as_mut()
            .unwrap()
            .deadline = Instant::now();
        assert!(listener.ready().is_none());
        let mut changed = instance;
        changed.participants[0].role = ActivityRole::Spectator;
        assert!(listener.changed(&changed));
        listener.attempt.as_mut().unwrap().since = Instant::now() - Duration::from_secs(21);
        assert!(listener.expired());
    }
}
