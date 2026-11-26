use super::AppState;
use crate::protocol::{ActivityAudioRole, ActivityCapability, Id, activity_audio_epoch};

impl AppState {
    pub fn authorize_activity_audio(
        &self,
        key: &str,
        is_bot: bool,
        instance_id: Id,
        epoch: &str,
        role: ActivityAudioRole,
    ) -> Result<(), String> {
        let denied = "you cannot use this activity audio seat";
        let instance = self.instance(instance_id).ok_or(denied)?;
        let channel = self.channel(instance.channel_id).ok_or(denied)?;
        let eligible = match role {
            ActivityAudioRole::Publisher => {
                is_bot && key == instance.app && !instance.app.starts_with("builtin:")
            }
            ActivityAudioRole::Listener => {
                !is_bot
                    && instance.role_of(key).is_some()
                    && self.is_guild_member(channel.guild_id, key)
                    && self.can_see_channel(key, &channel)
            }
        };
        if !eligible {
            return Err(denied.into());
        }
        let install = self
            .bot_install(channel.guild_id, &instance.app)
            .ok_or(denied)?;
        if instance.app.starts_with("builtin:")
            || !install.access.reads(channel.id)
            || !install
                .app
                .as_ref()
                .and_then(|app| app.activity.as_ref())
                .is_some_and(|a| {
                    a.capabilities.contains(&ActivityCapability::Audio)
                        && a.capabilities.contains(&ActivityCapability::Instance)
                })
        {
            return Err("activity audio has not been approved for this app and channel".into());
        }
        if role == ActivityAudioRole::Listener && !self.may_use_app(&install, key) {
            return Err(denied.into());
        }
        if activity_audio_epoch(&instance) != epoch {
            return Err("activity participants changed; request a fresh audio session".into());
        }
        Ok(())
    }
}
