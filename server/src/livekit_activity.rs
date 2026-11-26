use crate::{
    livekit::{LiveKitConfig, Placement},
    protocol::{ActivityAudioRole, Id, activity_audio_room},
};
use livekit_api::access_token::{AccessToken, VideoGrants};

pub fn mint(
    cfg: &LiveKitConfig,
    instance: Id,
    epoch: &str,
    key: &str,
    connection: u64,
    role: ActivityAudioRole,
) -> Result<String, String> {
    if cfg.minter.is_some() || cfg.placement() == Placement::Shared {
        return Err("this media provider does not support isolated activity audio yet".into());
    }
    if cfg.local_refused() {
        return Err("this host's media server is not running".into());
    }
    let room = activity_audio_room(instance, epoch).ok_or("invalid activity audio session")?;
    let publish = role == ActivityAudioRole::Publisher;
    let identity = if publish {
        format!("music-service:{key}")
    } else {
        format!("music-listener:{key}:{connection}")
    };
    AccessToken::with_api_key(&cfg.api_key, &cfg.api_secret)
        .with_ttl(crate::protocol::activity_audio::TOKEN_TTL)
        .with_identity(&identity)
        .with_grants(VideoGrants {
            room_join: true,
            room,
            can_publish: publish,
            can_subscribe: !publish,
            can_publish_data: false,
            can_publish_sources: if publish {
                vec!["microphone".into()]
            } else {
                Vec::new()
            },
            ..Default::default()
        })
        .to_jwt()
        .map_err(|_| "could not sign the activity audio credential".into())
}
