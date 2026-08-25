use serde::{Deserialize, Serialize};

use crate::identity::config_dir;

const FILE_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClientSettings {
    pub theme: String,
    #[serde(default)]
    pub accent: Option<String>,
    #[serde(default = "default_pattern")]
    pub pattern: String,
    pub background: Option<String>,
    pub background_dim: u8,
    #[serde(default = "default_blossom_server")]
    pub blossom_server: String,
    #[serde(default = "default_rendezvous_servers")]
    pub rendezvous_servers: Vec<String>,

    #[serde(default)]
    pub dm_relays: Vec<String>,

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
    #[serde(default = "default_screenshare_quality")]
    pub screenshare_quality: String,
    #[serde(default = "default_screenshare_audio")]
    pub screenshare_audio: bool,
    #[serde(default = "default_sfx_volume")]
    pub sfx_volume: u8,
    #[serde(default)]
    pub camera_device_id: Option<String>,
    #[serde(default)]
    pub camera_device_label: Option<String>,
}

pub fn default_screenshare_quality() -> String {
    "balanced".into()
}

fn default_screenshare_audio() -> bool {
    true
}

fn default_sfx_volume() -> u8 {
    70
}

fn default_mic_sensitivity() -> u32 {
    50
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

fn default_blossom_server() -> String {
    "https://blossom.band".into()
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
            accent: None,
            pattern: default_pattern(),
            background: None,
            background_dim: 55,
            blossom_server: default_blossom_server(),
            rendezvous_servers: default_rendezvous_servers(),
            dm_relays: Vec::new(),
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
            screenshare_quality: default_screenshare_quality(),
            screenshare_audio: default_screenshare_audio(),
            sfx_volume: default_sfx_volume(),
            camera_device_id: None,
            camera_device_label: None,
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
    if let Ok(content) = std::fs::read_to_string(&path)
        && let Ok(stored) = serde_json::from_str::<Stored>(&content)
        && stored.version == FILE_VERSION
    {
        return stored.settings;
    }
    ClientSettings::default()
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
    if let Ok(content) = serde_json::to_string_pretty(&stored) {
        let _ = std::fs::write(&path, content);
    }
}
