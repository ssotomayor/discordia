use serde::{Deserialize, Serialize};

use crate::identity::config_dir;

const FILE_VERSION: u32 = 1;

fn default_ui_size() -> u16 {
    100
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClientSettings {
    pub theme: String,
    #[serde(default = "default_ui_size")]
    pub text_size_percent: u16,
    #[serde(default = "default_ui_size")]
    pub emoji_size_percent: u16,
    #[serde(default)]
    pub guild_order: Vec<crate::protocol::Id>,
    #[serde(default)]
    pub accent: Option<String>,
    #[serde(default = "default_pattern")]
    pub pattern: String,
    pub background: Option<String>,
    pub background_dim: u8,
    #[serde(default = "default_rendezvous_servers")]
    pub rendezvous_servers: Vec<String>,

    #[serde(default)]
    pub dm_relays: Vec<String>,

    /// Peer pubkey → the second a conversation was last cleared. A watermark,
    /// not a tombstone: relays keep the events, so deleting can only mean
    /// "hide everything up to here", and a newer message reopens the chat.
    #[serde(default)]
    pub dm_cleared_at: Vec<(String, i64)>,

    /// Peer pubkey → the second of the newest message already read. Persisted
    /// because `AppState::dm_unread` is rebuilt from the relay replay on every
    /// launch, so a read that only lives in memory is undone by the next one.
    #[serde(default)]
    pub dm_read_at: Vec<(String, i64)>,

    /// Author pubkey → seconds that author's clock runs ahead of ours.
    /// Persisted because the estimate is only measurable on a live message and
    /// the correction has to apply to the history the relays replay first.
    #[serde(default)]
    pub dm_clock_offset: Vec<(String, i64)>,

    /// Channels and whole guilds that should never ring. Personal and local:
    /// nothing about muting is sent to the server or seen by anyone else.
    #[serde(default)]
    pub muted_channels: Vec<crate::protocol::Id>,
    #[serde(default)]
    pub muted_guilds: Vec<crate::protocol::Id>,

    #[serde(default)]
    pub selected_input_device: Option<String>,
    #[serde(default)]
    pub selected_output_device: Option<String>,
    #[serde(default = "default_mic_sensitivity")]
    pub mic_sensitivity: u32,
    #[serde(default = "default_mic_volume")]
    pub mic_volume: u16,
    #[serde(default = "default_auto_gain_control")]
    pub auto_gain_control: bool,
    #[serde(default)]
    pub noise_cancellation: bool,
    #[serde(default = "default_denoise_atten_lim_db")]
    pub denoise_atten_lim_db: u32,
    #[serde(default)]
    pub bypass_system_audio_processing: bool,
    #[serde(default = "default_voice_bitrate_kbps")]
    pub voice_bitrate_kbps: u32,
    #[serde(default)]
    pub layout_cells: Vec<(String, [u32; 4])>,
    #[serde(default)]
    pub layout_free: Vec<(String, [f64; 4])>,
    #[serde(default)]
    pub stream_layout_cells: Vec<(String, [u32; 4])>,
    #[serde(default)]
    pub stream_layout_free: Vec<(String, [f64; 4])>,
    #[serde(default = "default_screenshare_quality")]
    pub screenshare_quality: String,
    #[serde(default)]
    pub screenshare_fps: Option<u32>,
    #[serde(default)]
    pub screenshare_codec: crate::sysvideo::Codec,
    #[serde(default)]
    pub screenshare_encoder: crate::sysvideo::Encoder,
    #[serde(default = "default_screenshare_audio")]
    pub screenshare_audio: bool,
    #[serde(default = "default_sfx_volume")]
    pub sfx_volume: u8,
    #[serde(default = "default_soundboard_volume")]
    pub soundboard_volume: u8,
    /// Person pubkey → playback percent, and who is muted for us. A key is the
    /// account, so what we chose for someone still applies next launch.
    #[serde(default)]
    pub user_volumes: Vec<(String, u32)>,
    #[serde(default)]
    pub user_muted: Vec<String>,
    #[serde(default)]
    pub stream_volumes: Vec<(String, u32)>,
    #[serde(default)]
    pub stream_muted: Vec<String>,
    #[serde(default)]
    pub camera_device_id: Option<String>,
    #[serde(default)]
    pub camera_device_label: Option<String>,

    /// Suppresses a guild's accent, which otherwise wins inside that guild
    /// because it is set on a descendant of the element yours is set on.
    /// Off by default: a guild's branding applying in its own rooms is the
    /// behaviour that existed, and this is the refusal, not the rule.
    #[serde(default)]
    pub keep_my_accent: bool,

    /// A level that spans servers is a number only this machine can add up, so
    /// publishing it is how anyone else ever sees one. On by default because it
    /// carries no server names — see `nostr::xp`.
    #[serde(default = "default_publish_global_level")]
    pub publish_global_level: bool,

    /// Off by default, and the master switch for both producers: nothing about
    /// what is running on this machine leaves it until this is on.
    #[serde(default)]
    pub share_activity: bool,
    #[serde(default)]
    pub detect_games: bool,
    /// Bind `discord-ipc-N` rather than our own names. Free integration with
    /// every game already shipping Rich Presence, at the cost of taking the
    /// slot a running Discord wants — first to bind wins.
    #[serde(default)]
    pub discord_rpc_socket: bool,
    /// Executable to display name, for what the committed catalogue misses.
    #[serde(default)]
    pub detect_extra: Vec<(String, String)>,
}

fn default_publish_global_level() -> bool {
    true
}

pub fn default_screenshare_quality() -> String {
    "smooth".into()
}

fn default_screenshare_audio() -> bool {
    true
}

fn default_sfx_volume() -> u8 {
    70
}

pub const DEFAULT_SOUNDBOARD_VOLUME: u8 = 60;

fn default_soundboard_volume() -> u8 {
    DEFAULT_SOUNDBOARD_VOLUME
}

pub const DEFAULT_MIC_SENSITIVITY: u32 = 10;

fn default_mic_sensitivity() -> u32 {
    DEFAULT_MIC_SENSITIVITY
}

fn default_mic_volume() -> u16 {
    100
}

fn default_denoise_atten_lim_db() -> u32 {
    30
}

fn default_auto_gain_control() -> bool {
    true
}

fn default_voice_bitrate_kbps() -> u32 {
    48
}

fn default_pattern() -> String {
    "dots".into()
}

pub fn default_rendezvous_url() -> String {
    std::env::var("DIOXUSFUN_RENDEZVOUS_URL").unwrap_or_else(|_| "ws://localhost:7700".into())
}

fn default_rendezvous_servers() -> Vec<String> {
    vec![default_rendezvous_url()]
}

impl Default for ClientSettings {
    fn default() -> Self {
        Self {
            theme: "ember".into(),
            text_size_percent: default_ui_size(),
            emoji_size_percent: default_ui_size(),
            guild_order: Vec::new(),
            accent: None,
            pattern: default_pattern(),
            background: None,
            background_dim: 55,
            rendezvous_servers: default_rendezvous_servers(),
            dm_relays: Vec::new(),
            dm_cleared_at: Vec::new(),
            dm_clock_offset: Vec::new(),
            dm_read_at: Vec::new(),
            muted_channels: Vec::new(),
            muted_guilds: Vec::new(),
            selected_input_device: None,
            selected_output_device: None,
            mic_sensitivity: default_mic_sensitivity(),
            mic_volume: default_mic_volume(),
            auto_gain_control: default_auto_gain_control(),
            noise_cancellation: false,
            bypass_system_audio_processing: false,
            denoise_atten_lim_db: default_denoise_atten_lim_db(),
            voice_bitrate_kbps: default_voice_bitrate_kbps(),
            layout_cells: Vec::new(),
            layout_free: Vec::new(),
            stream_layout_cells: Vec::new(),
            stream_layout_free: Vec::new(),
            screenshare_quality: default_screenshare_quality(),
            screenshare_fps: None,
            screenshare_codec: crate::sysvideo::Codec::default(),
            screenshare_encoder: crate::sysvideo::Encoder::default(),
            screenshare_audio: default_screenshare_audio(),
            sfx_volume: default_sfx_volume(),
            soundboard_volume: default_soundboard_volume(),
            user_volumes: Vec::new(),
            user_muted: Vec::new(),
            stream_volumes: Vec::new(),
            stream_muted: Vec::new(),
            camera_device_id: None,
            camera_device_label: None,
            keep_my_accent: false,
            publish_global_level: true,
            share_activity: false,
            detect_games: false,
            discord_rpc_socket: false,
            detect_extra: Vec::new(),
        }
    }
}

impl ClientSettings {
    pub fn active_rendezvous(&self) -> String {
        self.rendezvous_servers
            .first()
            .cloned()
            .unwrap_or_else(default_rendezvous_url)
    }

    pub fn use_rendezvous(&mut self, url: &str) {
        let url = url.trim().trim_end_matches('/').to_string();
        if url.is_empty() {
            return;
        }
        self.rendezvous_servers.retain(|s| s != &url);
        self.rendezvous_servers.insert(0, url);
        self.rendezvous_servers.truncate(8);
    }

    pub fn remove_rendezvous(&mut self, url: &str) {
        self.rendezvous_servers.retain(|s| s != url);
        if self.rendezvous_servers.is_empty() {
            self.rendezvous_servers.push(default_rendezvous_url());
        }
    }

    /// Clearing twice must keep the later mark, or the second delete would
    /// bring back everything the first one hid.
    pub fn clear_dm(&mut self, peer: &str, at: i64) {
        match self.dm_cleared_at.iter_mut().find(|(p, _)| p == peer) {
            Some(entry) => entry.1 = entry.1.max(at),
            None => self.dm_cleared_at.push((peer.to_string(), at)),
        }
    }

    /// Records an author's clock offset. Returns whether anything changed, so
    /// the caller can skip a file write for an estimate it already holds.
    pub fn set_clock_offset(&mut self, author: &str, offset: i64) -> bool {
        match self.dm_clock_offset.iter_mut().find(|(p, _)| p == author) {
            Some(entry) if entry.1 == offset => false,
            Some(entry) => {
                entry.1 = offset;
                true
            }
            None => {
                self.dm_clock_offset.push((author.to_string(), offset));
                true
            }
        }
    }

    pub fn set_muted_channel(&mut self, channel_id: crate::protocol::Id, muted: bool) {
        set_membership(&mut self.muted_channels, channel_id, muted);
    }

    pub fn set_muted_guild(&mut self, guild_id: crate::protocol::Id, muted: bool) {
        set_membership(&mut self.muted_guilds, guild_id, muted);
    }

    /// Moves a read watermark forward. Returns whether anything changed, so the
    /// caller can skip a file write for the marks it already holds.
    pub fn mark_dm_read(&mut self, peer: &str, at: i64) -> bool {
        match self.dm_read_at.iter_mut().find(|(p, _)| p == peer) {
            Some(entry) if entry.1 >= at => false,
            Some(entry) => {
                entry.1 = at;
                true
            }
            None => {
                self.dm_read_at.push((peer.to_string(), at));
                true
            }
        }
    }
}

fn set_membership(list: &mut Vec<crate::protocol::Id>, id: crate::protocol::Id, present: bool) {
    match (present, list.iter().position(|x| *x == id)) {
        (true, None) => list.push(id),
        (false, Some(at)) => {
            list.remove(at);
        }
        _ => {}
    }
}

#[derive(Serialize, Deserialize)]
struct Stored {
    version: u32,
    settings: ClientSettings,
}

fn settings_path() -> std::path::PathBuf {
    config_dir().join("settings.json")
}

pub fn load_or_default() -> ClientSettings {
    let path = settings_path();
    match std::fs::read_to_string(&path) {
        Ok(content) => parse(&content).unwrap_or_else(|| {
            tracing::warn!(path = %path.display(), "settings file unreadable; starting from defaults");
            ClientSettings::default()
        }),
        Err(_) => ClientSettings::default(),
    }
}

/// One field that no longer parses costs that field, not the file: a whole-file
/// fallback reset every audio setting, and the next save wrote the defaults over them.
fn parse(content: &str) -> Option<ClientSettings> {
    use serde_json::Value;
    let stored: Value = serde_json::from_str(content).ok()?;
    if stored.get("version").and_then(Value::as_u64) != Some(u64::from(FILE_VERSION)) {
        return None;
    }
    let saved = stored.get("settings")?.as_object()?;
    if let Ok(settings) = serde_json::from_value::<ClientSettings>(Value::Object(saved.clone())) {
        return Some(settings);
    }
    let mut merged = serde_json::to_value(ClientSettings::default())
        .ok()?
        .as_object()?
        .clone();
    for (field, value) in saved {
        let mut trial = merged.clone();
        trial.insert(field.clone(), value.clone());
        if serde_json::from_value::<ClientSettings>(Value::Object(trial.clone())).is_ok() {
            merged = trial;
        } else {
            tracing::warn!(%field, "a saved setting could not be read; it is back to its default");
        }
    }
    serde_json::from_value(Value::Object(merged)).ok()
}

pub fn save(settings: &ClientSettings) {
    let path = settings_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let stored = Stored {
        version: FILE_VERSION,
        settings: settings.clone(),
    };
    match serde_json::to_string_pretty(&stored) {
        Ok(content) => {
            if let Err(e) = std::fs::write(&path, content) {
                tracing::warn!(path = %path.display(), error = %e, "could not save settings");
            }
        }
        Err(e) => tracing::warn!(error = %e, "could not encode settings"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_sharing_defaults_to_the_game_friendly_preset() {
        assert_eq!(default_screenshare_quality(), "smooth");
    }

    fn file(settings: serde_json::Value) -> String {
        serde_json::json!({ "version": FILE_VERSION, "settings": settings }).to_string()
    }

    #[test]
    fn appearance_sizes_and_personal_guild_order_round_trip() {
        let settings = ClientSettings {
            text_size_percent: 120,
            emoji_size_percent: 200,
            guild_order: vec![uuid::Uuid::new_v4(), uuid::Uuid::new_v4()],
            ..ClientSettings::default()
        };
        let loaded = parse(&file(serde_json::to_value(&settings).unwrap())).unwrap();
        assert_eq!(loaded, settings);
    }

    #[test]
    fn one_unreadable_field_does_not_cost_the_rest() {
        let mut good = serde_json::to_value(ClientSettings::default()).unwrap();
        good["noise_cancellation"] = true.into();
        good["mic_volume"] = 150.into();
        good["user_volumes"] = serde_json::json!([["abc", 40]]);
        // What serde_json writes for a NaN, and cannot read back as an f64.
        good["layout_free"] = serde_json::json!([["chat", [0.0, null, 1.0, 1.0]]]);
        let loaded = parse(&file(good)).expect("salvaged");
        assert!(loaded.noise_cancellation);
        assert_eq!(loaded.mic_volume, 150);
        assert_eq!(loaded.user_volumes, vec![("abc".to_string(), 40)]);
        assert!(
            loaded.layout_free.is_empty(),
            "only the broken field is dropped"
        );
    }

    #[test]
    fn a_file_written_before_a_field_existed_still_loads() {
        let mut old = serde_json::to_value(ClientSettings::default()).unwrap();
        for field in ["text_size_percent", "emoji_size_percent", "guild_order"] {
            old.as_object_mut().unwrap().remove(field);
        }
        old.as_object_mut().unwrap().remove("user_volumes");
        old.as_object_mut().unwrap().remove("screenshare_codec");
        old.as_object_mut().unwrap().remove("screenshare_encoder");
        old["auto_gain_control"] = false.into();
        let loaded = parse(&file(old)).expect("loads");
        assert_eq!(loaded.text_size_percent, 100);
        assert_eq!(loaded.emoji_size_percent, 100);
        assert!(loaded.guild_order.is_empty());
        assert!(!loaded.auto_gain_control);
        assert!(loaded.user_volumes.is_empty());
        assert_eq!(loaded.screenshare_codec, crate::sysvideo::Codec::H264);
        assert_eq!(loaded.screenshare_encoder, crate::sysvideo::Encoder::Auto);
    }
}
