use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    Natural,
    Balanced,
    Noisy,
    #[default]
    #[serde(other)]
    Custom,
}

impl Profile {
    pub const ALL: [Self; 4] = [Self::Natural, Self::Balanced, Self::Noisy, Self::Custom];

    pub fn label(self) -> &'static str {
        match self {
            Self::Natural => "Natural",
            Self::Balanced => "Balanced",
            Self::Noisy => "Noisy environment",
            Self::Custom => "Custom",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Natural => {
                "Keeps soft speech and music. Best in a quiet room or with an already processed microphone."
            }
            Self::Balanced => {
                "Light noise reduction and a steady microphone level for everyday conversations."
            }
            Self::Noisy => {
                "Stronger noise reduction for fans and keyboards. Adjust your microphone to preserve quiet words."
            }
            Self::Custom => "Keep your own sensitivity, volume and processing choices.",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Processing {
    pub profile: Profile,
    pub echo: bool,
    pub high_pass: bool,
    pub gate: bool,
    pub headphones: bool,
}

impl Default for Processing {
    fn default() -> Self {
        Self {
            profile: Profile::Custom,
            echo: true,
            high_pass: true,
            gate: true,
            headphones: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub processing: Processing,
    pub threshold: u32,
    pub volume: u16,
    pub agc: bool,
    pub denoise: bool,
    pub suppression: u32,
}

impl Config {
    pub fn from_settings(s: &crate::settings::ClientSettings) -> Self {
        Self {
            processing: s.mic_processing,
            threshold: s.mic_sensitivity,
            volume: s.mic_volume,
            agc: s.auto_gain_control,
            denoise: s.noise_cancellation,
            suppression: s.denoise_atten_lim_db,
        }
        .normalized()
    }

    pub fn select_profile(
        self,
        profile: Profile,
        settings: &crate::settings::ClientSettings,
    ) -> Self {
        if profile != Profile::Custom || self.processing.profile == Profile::Custom {
            return self.preset(profile);
        }
        settings
            .mic_custom_config
            .unwrap_or_else(|| Self::from_settings(&crate::settings::ClientSettings::default()))
            .preset(Profile::Custom)
            .normalized()
    }

    pub fn from_state(s: &crate::state::AppState) -> Self {
        Self {
            processing: s.mic_processing,
            threshold: s.mic_sensitivity,
            volume: s.mic_volume,
            agc: s.auto_gain_control,
            denoise: s.noise_cancellation,
            suppression: s.denoise_atten_lim_db,
        }
        .normalized()
    }

    pub fn normalized(mut self) -> Self {
        self.threshold = self.threshold.clamp(1, 1000);
        self.volume = self.volume.min(200);
        self.suppression = self.suppression.clamp(6, 60);
        self
    }

    pub fn preset(self, profile: Profile) -> Self {
        if profile == Profile::Custom {
            return Self {
                processing: Processing {
                    profile,
                    ..self.processing
                },
                ..self
            };
        }
        Self {
            processing: Processing {
                profile,
                echo: !self.processing.headphones,
                high_pass: profile != Profile::Natural,
                gate: profile != Profile::Natural,
                ..self.processing
            },
            threshold: crate::settings::DEFAULT_MIC_SENSITIVITY,
            agc: false,
            denoise: profile != Profile::Natural,
            suppression: if profile == Profile::Noisy { 30 } else { 15 },
            ..self
        }
    }

    pub fn for_new_output(mut self) -> Self {
        self.processing.profile = Profile::Custom;
        self.processing.headphones = false;
        self.processing.echo = true;
        self
    }

    pub fn apply_state(self, s: &mut crate::state::AppState) {
        let c = self.normalized();
        s.mic_processing = c.processing;
        s.mic_sensitivity = c.threshold;
        s.mic_volume = c.volume;
        s.auto_gain_control = c.agc;
        s.noise_cancellation = c.denoise;
        s.denoise_atten_lim_db = c.suppression;
    }

    pub fn apply_settings(self, s: &mut crate::settings::ClientSettings) {
        let c = self.normalized();
        // A preset replaces active values, so retain the manual configuration before leaving it.
        if s.mic_processing.profile == Profile::Custom && c.processing.profile != Profile::Custom {
            s.mic_custom_config = Some(Self::from_settings(s));
        }
        s.mic_processing = c.processing;
        s.mic_sensitivity = c.threshold;
        s.mic_volume = c.volume;
        s.auto_gain_control = c.agc;
        s.noise_cancellation = c.denoise;
        s.denoise_atten_lim_db = c.suppression;
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Context {
    pub input: String,
    pub output: String,
    pub bypass_requested: bool,
    pub raw_capture: bool,
    pub headphones: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Calibration {
    pub version: u8,
    pub context: Context,
    pub config: Config,
}

pub fn remember(saved: &mut Vec<Calibration>, calibration: Calibration) {
    saved.retain(|old| {
        old.context != calibration.context
            || old.config.processing.profile != calibration.config.processing.profile
    });
    saved.push(calibration);
    if saved.len() > 12 {
        saved.drain(..saved.len() - 12);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_configuration_survives_profile_switches_and_restart() {
        let mut settings = crate::settings::ClientSettings {
            selected_input_device: Some("my microphone".into()),
            voice_bitrate_kbps: 64,
            ..Default::default()
        };
        let custom = Config {
            processing: Processing {
                profile: Profile::Custom,
                echo: false,
                high_pass: false,
                gate: true,
                headphones: true,
            },
            threshold: 31,
            volume: 117,
            agc: false,
            denoise: true,
            suppression: 42,
        };
        custom.apply_settings(&mut settings);
        for profile in [Profile::Natural, Profile::Balanced, Profile::Noisy] {
            let next = Config::from_settings(&settings).select_profile(profile, &settings);
            next.apply_settings(&mut settings);
            assert_eq!(settings.mic_custom_config, Some(custom));
        }
        let json = serde_json::to_string(&settings).unwrap();
        let mut restored: crate::settings::ClientSettings = serde_json::from_str(&json).unwrap();
        let next = Config::from_settings(&restored).select_profile(Profile::Custom, &restored);
        next.apply_settings(&mut restored);
        assert_eq!(Config::from_settings(&restored), custom);
        assert_eq!(
            restored.selected_input_device.as_deref(),
            Some("my microphone")
        );
        assert_eq!(restored.voice_bitrate_kbps, 64);
        let mut state = crate::state::AppState::empty();
        next.apply_state(&mut state);
        assert_eq!(Config::from_state(&state), custom);

        let mut edited = next;
        edited.volume = 83;
        edited.suppression = 18;
        edited.processing.echo = true;
        edited.apply_settings(&mut restored);
        let preset = edited.select_profile(Profile::Natural, &restored);
        preset.apply_settings(&mut restored);
        assert_eq!(preset.select_profile(Profile::Custom, &restored), edited);
    }

    #[test]
    fn legacy_custom_values_are_saved_before_the_first_preset() {
        let mut json = serde_json::to_value(crate::settings::ClientSettings::default()).unwrap();
        json.as_object_mut().unwrap().remove("mic_custom_config");
        json["mic_volume"] = 87.into();
        json["mic_sensitivity"] = 44.into();
        json["denoise_atten_lim_db"] = 36.into();
        let mut settings: crate::settings::ClientSettings = serde_json::from_value(json).unwrap();
        let original = Config::from_settings(&settings);
        assert_eq!(
            original.select_profile(Profile::Custom, &settings),
            original
        );
        let preset = original.select_profile(Profile::Balanced, &settings);
        preset.apply_settings(&mut settings);
        assert_eq!(preset.select_profile(Profile::Custom, &settings), original);
    }

    #[test]
    fn missing_manual_history_uses_defaults_instead_of_copying_the_active_preset() {
        let settings = crate::settings::ClientSettings::default();
        let mut preset = Config::from_settings(&settings).preset(Profile::Noisy);
        preset.volume = 64;
        assert_eq!(
            preset.select_profile(Profile::Custom, &settings),
            Config::from_settings(&settings)
        );
    }

    #[test]
    fn legacy_processing_preserves_the_existing_pipeline() {
        assert_eq!(
            serde_json::from_str::<Processing>("{}").unwrap(),
            Processing::default()
        );
        let mut state = crate::state::AppState::empty();
        state.voice.muted = true;
        state.voice_bitrate_kbps = 48;
        let mut config = Config::from_state(&state);
        config.volume = 0;
        config.processing.headphones = true;
        let natural = config.preset(Profile::Natural);
        assert!(
            !natural.processing.echo
                && !natural.processing.gate
                && !natural.denoise
                && !natural.agc
        );
        natural.apply_state(&mut state);
        assert_eq!(state.mic_volume, 0);
        assert!(state.voice.muted);
        assert_eq!(state.voice_bitrate_kbps, 48);
        assert_eq!(natural.preset(Profile::Custom).volume, 0);
        let switched = natural.for_new_output();
        assert!(switched.processing.echo);
        assert!(!switched.processing.headphones);
        assert_eq!(switched.processing.profile, Profile::Custom);
        assert_eq!(switched.volume, 0);
    }

    #[test]
    fn old_settings_keep_manual_choices_and_calibrations_are_bounded_by_device_and_profile() {
        let mut json = serde_json::to_value(crate::settings::ClientSettings::default()).unwrap();
        let fields = json.as_object_mut().unwrap();
        fields.remove("mic_processing");
        fields.remove("mic_calibrations");
        fields.insert("auto_gain_control".into(), false.into());
        fields.insert("mic_volume".into(), 137.into());
        let old: crate::settings::ClientSettings = serde_json::from_value(json).unwrap();
        assert_eq!(old.mic_processing, Processing::default());
        assert_eq!(old.mic_volume, 137);
        assert!(!old.auto_gain_control);
        let mut future = serde_json::to_value(&old).unwrap();
        future["mic_processing"]["profile"] = "future_profile".into();
        let future: crate::settings::ClientSettings = serde_json::from_value(future).unwrap();
        assert_eq!(future.mic_processing.profile, Profile::Custom);
        assert_eq!(future.mic_volume, 137);
        let config = Config::from_state(&crate::state::AppState::empty()).preset(Profile::Balanced);
        let context = Context {
            input: "mic".into(),
            output: "output".into(),
            bypass_requested: false,
            raw_capture: false,
            headphones: false,
        };
        let mut saved = vec![];
        for i in 0..20 {
            remember(
                &mut saved,
                Calibration {
                    version: 1,
                    context: Context {
                        input: i.to_string(),
                        ..context.clone()
                    },
                    config,
                },
            );
        }
        assert_eq!(saved.len(), 12);
        let last = saved.last().unwrap().clone();
        remember(&mut saved, last.clone());
        assert_eq!(saved.len(), 12);
        assert_eq!(saved.last().unwrap(), &last);
    }
}
