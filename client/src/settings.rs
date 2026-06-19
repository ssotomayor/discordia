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
    /// Optional background image as a `data:image/...;base64,...` URL.
    pub background: Option<String>,
    /// Darkening scrim over the background, 0..=90 (percent opacity of black).
    pub background_dim: u8,
}

impl Default for ClientSettings {
    fn default() -> Self {
        Self {
            theme: "ember".into(),
            accent: None,
            background: None,
            background_dim: 55,
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
