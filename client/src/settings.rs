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

    /// Nostr relays used for direct messages, in preference order.
    ///
    /// DMs do not travel over the gateway at all — they are gift-wrapped events
    /// on these relays — so this list is what decides whether a friend can
    /// reach you, independently of which server either of you is on. Empty
    /// means the defaults in `nostr::relay::DEFAULT_RELAYS`, which is several
    /// unaffiliated operators on purpose: one default would let a single
    /// operator watch every Discordia user's DM metadata.
    #[serde(default)]
    pub dm_relays: Vec<String>,

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
    /// libwebrtc automatic gain control on the captured mic. On by default, and
    /// it fights `mic_volume`, so it is exposed as its own switch.
    ///
    /// **It does nothing, and this comment used to say it "rescues a quiet mic
    /// without the user knowing there's a slider".** Measured at ±0.01 dB over
    /// four runs by
    /// `live_sfu::agc_is_measured_against_the_assumption_the_gate_default_rests_on`,
    /// and the cause is structural rather than a defect: the APM runs on
    /// libwebrtc's own capture path, and we push finished hops into an external
    /// source instead, which is documented not to be fed by `AudioState`. The
    /// options are stored and read back faithfully — which is why `set_apm`'s
    /// check reports them "kept" — and consumed by nobody. See `TODO.md`.
    ///
    /// Left on and left exposed on purpose: the switch is harmless, and an SDK
    /// where it starts working would be welcome. What must not survive is the
    /// belief in the rescue, because `default_mic_sensitivity` was chosen
    /// expecting it.
    #[serde(default = "default_auto_gain_control")]
    pub auto_gain_control: bool,
    /// DeepFilterNet noise suppression on captured microphone audio. Off by
    /// default: it costs ~1.5% of a core and users should opt into it.
    #[serde(default)]
    pub noise_cancellation: bool,
    /// Ceiling on how far DeepFilterNet may pull a hop down, in dB. Only has
    /// an effect while `noise_cancellation` is on.
    ///
    /// A ceiling rather than a strength dial: the model attenuates what it
    /// judges to be noise, and this bounds how far it is allowed to go. Lower
    /// leaves more of the original signal — including the quiet speech the
    /// transmit gate needs to see — at the cost of removing less noise.
    ///
    /// Measured on one microphone in one room, moving this from 30 to 12
    /// changed what the model actually applied by about a decibel, because
    /// speech rarely reaches the ceiling at all. It is exposed because that
    /// was one microphone: a noisier room, or a model that judges a given
    /// voice more harshly, is where it would bite.
    #[serde(default = "default_denoise_atten_lim_db")]
    pub denoise_atten_lim_db: u32,
    /// Capture the microphone in WASAPI raw mode, skipping the processing the
    /// driver applies before any app sees a sample. Off by default: the
    /// endpoint's effects are what the machine was tuned with, and turning them
    /// off is a choice about which suppressor gets the signal — see
    /// `crate::rawmic`.
    ///
    /// Windows-only, and the UI hides it elsewhere: macOS applies its mic modes
    /// only to clients of the voice-processing audio unit, which cpal is not,
    /// so there a capture is already raw.
    #[serde(default)]
    pub bypass_system_audio_processing: bool,
    /// Opus bitrate for the microphone track, in kbit/s. Only 24 and 48 are
    /// offered — 24 is LiveKit's speech preset, 48 is roughly what other voice
    /// chats spend on a talking head and is audibly better on low voices,
    /// laughter and anything with music behind it.
    ///
    /// Read when the mic track is published, so a change lands on the next
    /// voice connect rather than mid-call.
    #[serde(default = "default_voice_bitrate_kbps")]
    pub voice_bitrate_kbps: u32,
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
    /// The camera to open, by `enumerateDevices` id. `None` = system default.
    ///
    /// Written back from what actually *opened*, not from what was asked for, so
    /// a fallback to another camera never gets persisted as the user's choice.
    #[serde(default)]
    pub camera_device_id: Option<String>,
    /// Its label, kept as the fallback matcher: deviceIds are origin-salted and
    /// can rotate between sessions while labels usually do not. Unlike the audio
    /// devices, which are named by a string that *is* the identity, a camera id
    /// can go stale while the camera is still plugged in.
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

/// ×1000 peak, so 50 is −26 dBFS. Deliberately 6 dB less sensitive than the
/// −32 dB this used to default to: at −32 the gate opened for fan noise and
/// keyboard clatter, which is exactly the traffic the gate exists to stop.
///
/// **Both settings are known to be wrong, and they fail as the same fact.** At
/// −32 the gate passed keyboards; at −26 it cuts ordinary speech mid-phrase,
/// reported from a real call and reproduced here. A threshold decides by
/// *level*, while quiet speech and fan noise differ by *character* at the same
/// level — so no number is right, and moving this one only chooses which of the
/// two failures to have.
///
/// It is not rescued from elsewhere either: `auto_gain_control` is inert (see
/// its own note), and DeepFilterNet — the one suppressor that works — is off by
/// default, so a fresh install has no suppressor at all and this number is
/// alone. Turning the model on does not fix it by itself: the gate measures the
/// *denoised* hop, which the model pulls down 2.7–3.9 dB, so at an unchanged
/// threshold it cuts more rather than less. Suppression and threshold have to
/// move together.
///
/// `TODO.md` records the recommendation rather than a new constant: calibrate.
/// The VU bar already draws this threshold against a live meter, so what is
/// missing is a "speak normally" step to place it — which is also the only
/// arrangement in which changing the suppressor can ask for a recalibration
/// instead of silently invalidating a number somebody tuned by ear.
fn default_mic_sensitivity() -> u32 {
    50
}

/// Unity. The slider is there for mics that need help, not as a thing to set.
fn default_mic_volume() -> u16 {
    100
}

/// The model's own default, and what shipped before this was adjustable.
fn default_denoise_atten_lim_db() -> u32 {
    30
}

fn default_auto_gain_control() -> bool {
    true
}

/// 48 kbit/s, not the 24 the SDK's speech preset carries. The extra 24 kbit/s
/// per speaker is small next to what a screen share already costs, and it is
/// the difference most audible on the voices that sound worst at 24.
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
