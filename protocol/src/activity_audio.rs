use crate::{ActivityInstance, ActivityRole, Id};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ActivityAudioCredential(String);

impl From<String> for ActivityAudioCredential {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl ActivityAudioCredential {
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl std::fmt::Debug for ActivityAudioCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted activity audio credential>")
    }
}

pub const TOKEN_TTL: std::time::Duration = std::time::Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityAudioRole {
    Publisher,
    Listener,
}

// Old grants can still join old rooms: every membership change must also close
// the old publication and rotate its key, even if a prior scope reappears.
pub fn activity_audio_epoch(instance: &ActivityInstance) -> String {
    let mut hash = Sha256::new();
    hash.update(b"discordia-activity-audio-v1\0");
    hash.update(instance.id.as_bytes());
    hash.update(instance.channel_id.as_bytes());
    hash.update((instance.app.len() as u64).to_be_bytes());
    hash.update(instance.app.as_bytes());
    for participant in &instance.participants {
        hash.update((participant.pubkey.len() as u64).to_be_bytes());
        hash.update(participant.pubkey.as_bytes());
        hash.update([match participant.role {
            ActivityRole::Player => 0,
            ActivityRole::Spectator => 1,
        }]);
    }
    format!("{:x}", hash.finalize())[..32].to_string()
}

pub fn activity_audio_room(instance_id: Id, epoch: &str) -> Option<String> {
    if epoch.len() != 32
        || !epoch
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    Some(format!("activity-audio-{instance_id}-{epoch}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ActivityParticipant;

    #[test]
    fn credentials_travel_as_strings_but_never_appear_in_debug_output() {
        let token = ActivityAudioCredential::from("private-test-credential".to_string());
        assert!(!format!("{token:?}").contains("private-test-credential"));
        assert_eq!(
            serde_json::to_string(&token).unwrap(),
            "\"private-test-credential\""
        );
        let decoded: ActivityAudioCredential =
            serde_json::from_str("\"private-test-credential\"").unwrap();
        assert_eq!(decoded.as_str(), "private-test-credential");
    }

    #[test]
    fn audio_is_a_permission_increase_and_requires_instance_access() {
        use crate::{ActivityCapability, ActivityManifest, AppManifest, validate_manifest};
        let old = AppManifest {
            name: "Music test".into(),
            description: String::new(),
            icon: "music".into(),
            version: 1,
            permissions: vec![],
            intents: vec![],
            commands: vec![],
            agent: None,
            activity: Some(ActivityManifest {
                sha256: "a".repeat(64),
                capabilities: vec![ActivityCapability::Instance],
                max_players: 16,
                url: None,
            }),
        };
        let mut new = old.clone();
        new.version = 2;
        new.activity
            .as_mut()
            .unwrap()
            .capabilities
            .push(ActivityCapability::Audio);
        assert!(old.widens(&new));
        assert!(validate_manifest(new.clone()).is_ok());
        new.activity.as_mut().unwrap().capabilities = vec![ActivityCapability::Audio];
        assert!(validate_manifest(new).is_err());
    }

    #[test]
    fn membership_and_roles_change_the_audio_scope() {
        let mut instance = ActivityInstance {
            id: Id::new_v4(),
            channel_id: Id::new_v4(),
            app: "app".into(),
            max_players: 16,
            participants: vec![ActivityParticipant {
                pubkey: "alice".into(),
                role: ActivityRole::Player,
            }],
        };
        let original = activity_audio_epoch(&instance);
        assert_eq!(original, activity_audio_epoch(&instance));
        instance.participants.push(ActivityParticipant {
            pubkey: "bob".into(),
            role: ActivityRole::Spectator,
        });
        assert_ne!(original, activity_audio_epoch(&instance));
        let joined = activity_audio_epoch(&instance);
        instance.participants[1].role = ActivityRole::Player;
        assert_ne!(joined, activity_audio_epoch(&instance));
        assert!(activity_audio_room(instance.id, "../voice-room").is_none());
        let room = activity_audio_room(instance.id, &original).unwrap();
        assert!(room.starts_with("activity-audio-"));
        assert!(!room.starts_with("voice-"));
        assert!(!room.starts_with("screen-"));
    }
}
