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
    device
        .description()
        .map(|description| description.name().to_owned())
}

pub fn needs_reopen(kind: ErrorKind) -> bool {
    !matches!(
        kind,
        ErrorKind::Xrun | ErrorKind::DeviceChanged | ErrorKind::RealtimeDenied
    )
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
