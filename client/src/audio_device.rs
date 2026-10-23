use cpal::traits::DeviceTrait;
use cpal::{ErrorKind, SampleFormat, SupportedStreamConfig, SupportedStreamConfigRange};

pub fn name(device: &cpal::Device) -> Result<String, cpal::Error> {
    // ALSA's old name was the PCM identifier; descriptions would invalidate saved selections.
    #[cfg(any(
        target_os = "linux",
        target_os = "freebsd",
        target_os = "dragonfly",
        target_os = "netbsd"
    ))]
    {
        let id = device.id()?;
        if id.host() == cpal::HostId::Alsa {
            return Ok(id.id().to_owned());
        }
    }
    let description = device.description()?;
    #[cfg(target_os = "android")]
    if let Some(address) = description.address().filter(|address| !address.is_empty()) {
        return Ok(format!("{} · {address}", description.name()));
    }
    Ok(description.name().to_owned())
}

pub fn selectable(device: &cpal::Device, input: bool) -> bool {
    #[cfg(target_os = "android")]
    {
        let Ok(description) = device.description() else {
            return false;
        };
        if !android_endpoint(description.device_type(), description.interface_type()) {
            return false;
        }
    }
    if input {
        device.supports_input()
    } else {
        device.supports_output()
    }
}

#[cfg(any(target_os = "android", test))]
fn android_endpoint(kind: cpal::DeviceType, interface: cpal::InterfaceType) -> bool {
    use cpal::{DeviceType as D, InterfaceType as I};
    matches!(
        kind,
        D::Microphone
            | D::Headset
            | D::Headphones
            | D::Speaker
            | D::Earpiece
            | D::HearingAid
            | D::Dock
    ) || (kind == D::Unknown && matches!(interface, I::Usb | I::Bluetooth))
}

pub fn needs_reopen(kind: ErrorKind) -> bool {
    !matches!(
        kind,
        ErrorKind::Xrun | ErrorKind::DeviceChanged | ErrorKind::RealtimeDenied
    )
}

pub fn missing(name: &str, available: &[String], default_route: Option<&str>) -> bool {
    default_route != Some(name) && !available.iter().any(|device| device == name)
}

pub fn config(
    device: &cpal::Device,
    input: bool,
    formats: &[SampleFormat],
) -> Result<SupportedStreamConfig, String> {
    let default = if input {
        device.default_input_config()
    } else {
        device.default_output_config()
    }
    .map_err(|error| error.to_string())?;
    if valid(&default, formats) {
        return Ok(default);
    }
    let chosen = if input {
        let supported = device
            .supported_input_configs()
            .map_err(|error| error.to_string())?;
        select(default, supported, formats)
    } else {
        let supported = device
            .supported_output_configs()
            .map_err(|error| error.to_string())?;
        select(default, supported, formats)
    };
    chosen.ok_or_else(|| "No supported PCM configuration for this audio device.".to_owned())
}

fn valid(config: &SupportedStreamConfig, formats: &[SampleFormat]) -> bool {
    config.channels() > 0 && config.sample_rate() > 0 && formats.contains(&config.sample_format())
}

fn select(
    default: SupportedStreamConfig,
    supported: impl Iterator<Item = SupportedStreamConfigRange>,
    formats: &[SampleFormat],
) -> Option<SupportedStreamConfig> {
    supported
        .filter_map(|range| {
            let format_rank = formats
                .iter()
                .position(|format| *format == range.sample_format())?;
            if range.channels() == 0 || range.min_sample_rate() == 0 {
                return None;
            }
            [
                48_000,
                default.sample_rate(),
                44_100,
                range.max_sample_rate(),
            ]
            .into_iter()
            .enumerate()
            .find_map(|(rate_rank, rate)| {
                range.try_with_sample_rate(rate).map(|config| {
                    (
                        (
                            format_rank,
                            range.channels() != default.channels(),
                            rate_rank,
                        ),
                        config,
                    )
                })
            })
        })
        .min_by_key(|(rank, _)| *rank)
        .map(|(_, config)| config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cpal::SupportedBufferSize;

    #[test]
    fn android_picker_excludes_internal_routes_but_keeps_microphones_and_accessories() {
        use cpal::{DeviceType as D, InterfaceType as I};
        for kind in [D::Virtual, D::Tuner, D::Handset, D::Unknown] {
            assert!(!android_endpoint(kind, I::Unknown));
        }
        assert!(android_endpoint(D::Microphone, I::BuiltIn));
        assert!(android_endpoint(D::Headset, I::Bluetooth));
        assert!(android_endpoint(D::Unknown, I::Usb));
        assert!(android_endpoint(D::Speaker, I::BuiltIn));
    }

    #[test]
    fn android_default_route_survives_physical_device_enumeration() {
        let physical = vec!["Built-in microphone".into(), "Speaker".into()];
        assert!(!missing(
            "Default Device",
            &physical,
            Some("Default Device")
        ));
        assert!(missing(
            "Unplugged headset",
            &physical,
            Some("Default Device")
        ));
        assert!(missing("Default Device", &physical, None));
        assert!(!missing("Speaker", &physical, None));
    }

    fn range(
        format: SampleFormat,
        channels: u16,
        min: u32,
        max: u32,
    ) -> SupportedStreamConfigRange {
        SupportedStreamConfigRange::new(channels, min, max, SupportedBufferSize::Unknown, format)
    }

    #[test]
    fn integer_default_falls_back_to_supported_float_at_pipeline_rate() {
        let default = range(SampleFormat::I32, 2, 44_100, 96_000).with_sample_rate(44_100);
        let chosen = select(
            default,
            [
                range(SampleFormat::I16, 2, 48_000, 48_000),
                range(SampleFormat::F32, 2, 44_100, 96_000),
            ]
            .into_iter(),
            &[SampleFormat::F32, SampleFormat::I16],
        )
        .unwrap();
        assert_eq!(chosen.sample_format(), SampleFormat::F32);
        assert_eq!(chosen.sample_rate(), 48_000);
        assert_eq!(chosen.channels(), 2);
    }

    #[test]
    fn bluetooth_rate_is_retained_when_pipeline_rate_is_unavailable() {
        let default = range(SampleFormat::I32, 1, 16_000, 16_000).with_sample_rate(16_000);
        let chosen = select(
            default,
            [range(SampleFormat::I16, 1, 16_000, 16_000)].into_iter(),
            &[SampleFormat::F32, SampleFormat::I16],
        )
        .unwrap();
        assert_eq!(chosen.sample_rate(), 16_000);
        assert_eq!(chosen.channels(), 1);
    }

    #[test]
    fn unsupported_and_invalid_ranges_do_not_produce_a_stream() {
        let default = range(SampleFormat::I32, 2, 48_000, 48_000).with_sample_rate(48_000);
        assert!(
            select(
                default,
                [
                    range(SampleFormat::I32, 2, 48_000, 48_000),
                    range(SampleFormat::F32, 0, 48_000, 48_000)
                ]
                .into_iter(),
                &[SampleFormat::F32]
            )
            .is_none()
        );
    }

    #[test]
    fn transient_callbacks_do_not_destroy_a_running_stream() {
        for kind in [
            ErrorKind::Xrun,
            ErrorKind::DeviceChanged,
            ErrorKind::RealtimeDenied,
        ] {
            assert!(!needs_reopen(kind));
        }
        for kind in [
            ErrorKind::DeviceNotAvailable,
            ErrorKind::StreamInvalidated,
            ErrorKind::DeviceBusy,
            ErrorKind::PermissionDenied,
        ] {
            assert!(needs_reopen(kind));
        }
    }
}
