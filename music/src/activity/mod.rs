#[cfg(feature = "audio-keys")]
pub mod keys;
pub mod runtime;
pub mod session;

use discordia_bot::protocol::{
    ActivityCapability, ActivityManifest, AppManifest, MAX_ACTIVITY_PLAYERS,
};
use sha2::{Digest, Sha256};

pub const BUNDLE: &str = include_str!("../../assets/index.html");
// Hosts reject changed bundle hashes unless the signed manifest version increases.
const VERSION: u64 = 1;

pub fn manifest() -> AppManifest {
    AppManifest {
        name: "Music Lab".into(),
        description: "Experimental shared music queue and source diagnostics. Audio playback is not connected yet.".into(),
        icon: "🎵".into(),
        version: VERSION,
        permissions: Vec::new(),
        intents: Vec::new(),
        commands: Vec::new(),
        agent: None,
        activity: Some(ActivityManifest {
            sha256: hex::encode(Sha256::digest(BUNDLE.as_bytes())),
            capabilities: vec![ActivityCapability::Instance],
            max_players: MAX_ACTIVITY_PLAYERS,
            // The running service supplies the exact signed bundle, including before release.
            url: None,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_only_requests_instance_access() {
        let app = manifest();
        assert!(app.permissions.is_empty());
        assert!(app.intents.is_empty());
        let activity = app.activity.unwrap();
        assert_eq!(activity.capabilities, [ActivityCapability::Instance]);
        assert_eq!(
            activity.sha256,
            hex::encode(Sha256::digest(BUNDLE.as_bytes()))
        );
        assert!(!BUNDLE.contains("<iframe"));
    }
}
