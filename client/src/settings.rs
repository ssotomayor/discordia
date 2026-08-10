//! Local, per-client appearance settings — theme + optional background image.
//! Stored in `settings.json` in the config dir (so it respects
//! `DIOXUSFUN_CONFIG_DIR`). Purely local: never sent to any host.

use serde::{Deserialize, Serialize};

use crate::identity::config_dir;

const FILE_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClientSettings {
    /// Theme id (see `crate::app::THEMES`).
    pub theme: String,
    /// Optional accent-color override (hex), layered on top of the theme.
    #[serde(default)]
    pub accent: Option<String>,
    /// Procedural app background: "grid" | "dots" | "aurora" | "mesh" |
    /// "sunset" | "none". A user-supplied `background` image (below) overrides
    /// this when set.
    #[serde(default = "default_pattern")]
    pub pattern: String,
    /// Optional background image as a `data:image/...;base64,...` URL.
    pub background: Option<String>,
    /// Darkening scrim over the background, 0..=90 (percent opacity of black).
    pub background_dim: u8,
    /// Blossom media server used to host profile images (avatar/banner).
    #[serde(default = "default_blossom_server")]
    pub blossom_server: String,
    /// Rendezvous servers the user has added, most-recently-used first. The
    /// first entry is the active one. Kept local — a personal address book, not
    /// something any host sees.
    #[serde(default = "default_rendezvous_servers")]
    pub rendezvous_servers: Vec<String>,

    /// Persisted audio device choices (None = system default).
    #[serde(default)]
    pub selected_input_device: Option<String>,
    #[serde(default)]
    pub selected_output_device: Option<String>,
    /// Microphone gate threshold (1..=1000, peak ×1000). Below it the mic is
    /// treated as inactive: nothing is transmitted and the speaking indicator
    /// stays off. Lower = more sensitive. Default 50 (≈ -26 dBFS).
    #[serde(default = "default_mic_sensitivity")]
    pub mic_sensitivity: u32,
    /// Microphone input gain as a percentage, 0..=200 (100 = unity). Applied
    /// *before* the meter and the transmit gate, so the VU bar shows what
    /// listeners actually hear and the threshold stays relative to it — which
    /// also means turning input up makes the gate open more readily.
    #[serde(default = "default_mic_volume")]
    pub mic_volume: u16,
    /// libwebrtc automatic gain control on the captured mic. On by default (it
    /// rescues a quiet mic without the user knowing there's a slider), but it
    /// fights `mic_volume`, so it's exposed as its own switch.
    #[serde(default = "default_auto_gain_control")]
    pub auto_gain_control: bool,
    /// DeepFilterNet noise suppression on captured microphone audio. Off by
    /// default: it costs ~1.5% of a core and users should opt into it.
    #[serde(default)]
    pub noise_cancellation: bool,
    /// Persisted panel positions. Stored as flat arrays rather than the grid
    /// crate's types so `settings.json` stays readable and this file doesn't
    /// depend on the layout crate's serialization shape.
    /// `id -> [x, y, w, h]` in grid cells.
    #[serde(default)]
    pub layout_cells: Vec<(String, [u32; 4])>,
    /// `id -> [x, y, w, h]` as fractions of the workspace (0..=1), for the
    /// free-floating layout. Fractions rather than pixels so a saved
    /// arrangement rescales with the window and can never restore off-screen.
    #[serde(default)]
    pub layout_free: Vec<(String, [f64; 4])>,
    /// Screen-share quality preset id — see
    /// `features::screenshare::quality_preset`. Trades resolution against
    /// framerate and bitrate; "balanced" (1080p30) is the default.
    #[serde(default = "default_screenshare_quality")]
    pub screenshare_quality: String,
    /// Send the machine's sound along with a screen share. On by default:
    /// sharing a video or a game without its audio is the surprising outcome,
    /// not the safe one, and the share itself is already deliberate. Turning it
    /// off is the opt-out for someone who doesn't want their machine heard.
    ///
    /// This governs what *we* do — the native capture path, and whether the
    /// engine is asked for audio at all. Where the engine is in charge it still
    /// shows its own unticked "Share audio" box, which no constraint can
    /// pre-tick; that choice stays the user's.
    #[serde(default = "default_screenshare_audio")]
    pub screenshare_audio: bool,
    /// Master volume for UI sound effects, 0..=100. 0 = muted. Scales the
    /// gain of every synthesized tone in `window.dxSfx`.
    #[serde(default = "default_sfx_volume")]
    pub sfx_volume: u8,
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

/// ×1000 peak, so 50 is −26 dBFS. Deliberately 6 dB less sensitive than the
/// −32 dB this used to default to: at −32 the gate opened for fan noise and
/// keyboard clatter, which is exactly the traffic the gate exists to stop.
fn default_mic_sensitivity() -> u32 {
    50
}

/// Unity. The slider is there for mics that need help, not as a thing to set.
fn default_mic_volume() -> u16 {
    100
}

fn default_auto_gain_control() -> bool {
    true
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
            selected_input_device: None,
            selected_output_device: None,
            mic_sensitivity: default_mic_sensitivity(),
            mic_volume: default_mic_volume(),
            auto_gain_control: default_auto_gain_control(),
            noise_cancellation: false,
            layout_cells: Vec::new(),
            layout_free: Vec::new(),
            screenshare_quality: default_screenshare_quality(),
            screenshare_audio: default_screenshare_audio(),
            sfx_volume: default_sfx_volume(),
        }
    }
}

impl ClientSettings {
    /// The active rendezvous (first entry), falling back to the default.
    pub fn active_rendezvous(&self) -> String {
        self.rendezvous_servers
            .first()
            .cloned()
            .unwrap_or_else(default_rendezvous_url)
    }

    /// Add (or promote) `url` to the front of the list, de-duplicated.
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
    if let Ok(content) = std::fs::read_to_string(&path) {
        if let Ok(stored) = serde_json::from_str::<Stored>(&content) {
            if stored.version == FILE_VERSION {
                return stored.settings;
            }
        }
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
