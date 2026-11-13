use super::{Encoder, Settings};

const SOFTWARE_PIXEL_BUDGET: u64 = 1280 * 720 * 30;

#[cfg(any(target_os = "windows", test))]
pub(crate) fn active_output(report: &[livekit::webrtc::stats::RtcStats]) -> Option<(u32, u64)> {
    report
        .iter()
        .filter_map(|entry| match entry {
            livekit::webrtc::stats::RtcStats::OutboundRtp(o)
                if o.stream.kind == "video"
                    && o.outbound.active
                    && o.outbound.target_bitrate.is_finite()
                    && o.outbound.target_bitrate > 0.0 =>
            {
                Some((o.stream.ssrc, u64::from(o.outbound.frames_encoded)))
            }
            _ => None,
        })
        .fold(None, |total, (ssrc, frames)| {
            Some(match total {
                None => (ssrc, frames),
                Some((first, count)) => (first.min(ssrc), count.saturating_add(frames)),
            })
        })
}

#[cfg(any(target_os = "windows", test))]
#[derive(Default)]
pub(crate) struct OutputProgress {
    last: Option<(u32, u64)>,
    waiting_since: Option<std::time::Instant>,
    sampled_at: Option<std::time::Instant>,
}

#[cfg(any(target_os = "windows", test))]
impl OutputProgress {
    pub(crate) fn stalled(
        &mut self,
        now: std::time::Instant,
        expecting_output: bool,
        output: Option<(u32, u64)>,
    ) -> bool {
        let gap = self.sampled_at.is_some_and(|previous| {
            now.saturating_duration_since(previous) > std::time::Duration::from_secs(7)
        });
        self.sampled_at = Some(now);
        if gap || output != self.last || !expecting_output {
            self.last = output;
            self.waiting_since = None;
        }
        if !expecting_output {
            return false;
        }
        let since = *self.waiting_since.get_or_insert(now);
        now.saturating_duration_since(since) >= std::time::Duration::from_secs(5)
    }
}

pub(crate) fn stop_software_encoding(
    settings: Settings,
    automatic_hardware: bool,
    implementation: &str,
) -> bool {
    let implementation = implementation.to_ascii_lowercase();
    if !implementation.contains("openh264") && !implementation.contains("libvpx") {
        return false;
    }
    match settings.encoder {
        Encoder::Gpu => true,
        Encoder::Cpu => false,
        // An automatic hardware failure must not silently become an expensive software stream.
        Encoder::Auto => {
            automatic_hardware
                && u64::from(settings.width)
                    .saturating_mul(u64::from(settings.height))
                    .saturating_mul(u64::from(settings.fps))
                    > SOFTWARE_PIXEL_BUDGET
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outbound(
        ssrc: u32,
        frames: u32,
        active: bool,
        bitrate: f64,
    ) -> livekit::webrtc::stats::RtcStats {
        let mut stats = livekit::webrtc::stats::OutboundRtpStats::default();
        stats.stream.kind = "video".into();
        stats.stream.ssrc = ssrc;
        stats.outbound.frames_encoded = frames;
        stats.outbound.active = active;
        stats.outbound.target_bitrate = bitrate;
        livekit::webrtc::stats::RtcStats::OutboundRtp(stats)
    }

    #[test]
    fn paused_layers_and_unassigned_bitrate_do_not_trigger_a_stall() {
        let samples = [
            outbound(1, 100, false, 1_000_000.0),
            outbound(2, 0, true, 0.0),
            outbound(3, 0, true, f64::NAN),
            outbound(4, 0, true, f64::INFINITY),
        ];
        assert_eq!(active_output(&samples), None);
        let mut progress = OutputProgress::default();
        let now = std::time::Instant::now();
        for seconds in 0..20 {
            let output = active_output(&samples);
            assert!(!progress.stalled(
                now + std::time::Duration::from_secs(seconds),
                output.is_some(),
                output,
            ));
        }
    }

    #[test]
    fn a_paused_primary_does_not_hide_progress_on_another_layer() {
        let now = std::time::Instant::now();
        let mut progress = OutputProgress::default();
        for seconds in 0..10 {
            let samples = [
                outbound(1, 1000, false, 1_000_000.0),
                outbound(3, seconds, true, 300_000.0),
                outbound(2, 10, true, 100_000.0),
            ];
            let output = active_output(&samples);
            assert_eq!(output, Some((2, 10 + u64::from(seconds))));
            let mut reversed = samples.to_vec();
            reversed.reverse();
            assert_eq!(active_output(&reversed), output);
            assert!(!progress.stalled(
                now + std::time::Duration::from_secs(u64::from(seconds)),
                true,
                output,
            ));
        }
    }

    #[test]
    fn blocked_initialization_or_output_cannot_wait_forever_while_capturing() {
        use std::time::{Duration, Instant};
        for output in [None, Some((1, 0))] {
            let mut progress = OutputProgress::default();
            let now = Instant::now();
            assert!(!progress.stalled(now, true, output));
            assert!(!progress.stalled(now + Duration::from_secs(4), true, output));
            assert!(progress.stalled(now + Duration::from_secs(5), true, output));
        }
    }

    #[test]
    fn live_frames_idle_sources_track_replacement_and_counter_resets_reset_the_deadline() {
        use std::time::{Duration, Instant};
        let mut progress = OutputProgress::default();
        let now = Instant::now();
        for second in 0..10 {
            assert!(!progress.stalled(now + Duration::from_secs(second), true, Some((1, second))));
        }
        assert!(!progress.stalled(now + Duration::from_secs(20), false, Some((1, 9))));
        assert!(!progress.stalled(now + Duration::from_secs(30), true, Some((1, 9))));
        assert!(!progress.stalled(now + Duration::from_secs(34), true, Some((2, 0))));
        assert!(!progress.stalled(now + Duration::from_secs(38), true, Some((2, 4))));
        assert!(!progress.stalled(now + Duration::from_secs(42), true, Some((2, 0))));
        assert!(progress.stalled(now + Duration::from_secs(47), true, Some((2, 0))));
    }

    #[test]
    fn a_sampling_gap_gets_a_fresh_deadline_on_resume() {
        use std::time::{Duration, Instant};
        let mut progress = OutputProgress::default();
        let now = Instant::now();
        assert!(!progress.stalled(now, true, Some((1, 0))));
        assert!(!progress.stalled(now + Duration::from_secs(30), true, Some((1, 0))));
        assert!(!progress.stalled(now + Duration::from_secs(34), true, Some((1, 0))));
        assert!(progress.stalled(now + Duration::from_secs(35), true, Some((1, 0))));
    }

    fn settings(encoder: Encoder, width: u32, height: u32, fps: u32) -> Settings {
        Settings {
            encoder,
            width,
            height,
            fps,
            codec: super::super::Codec::H264,
            max_bitrate: 8_000_000,
            adaptive_quality: false,
            priority: super::super::Priority::Motion,
        }
    }

    #[test]
    fn automatic_high_load_share_stops_when_stats_change_to_software() {
        let settings = settings(Encoder::Auto, 1920, 1080, 60);
        for implementation in ["Media Foundation AMD", "", "unknown", "H264"] {
            assert!(!stop_software_encoding(settings, true, implementation));
        }
        for implementation in ["OpenH264", "SimulcastEncoderAdapter (OpenH264)", "libvpx"] {
            assert!(stop_software_encoding(settings, true, implementation));
        }
    }

    #[test]
    fn deliberate_software_encoding_and_auto_without_hardware_stay_available() {
        for (encoder, hardware) in [(Encoder::Cpu, true), (Encoder::Auto, false)] {
            assert!(!stop_software_encoding(
                settings(encoder, 1920, 1080, 60),
                hardware,
                "OpenH264"
            ));
        }
    }

    #[test]
    fn budget_uses_pixels_and_frame_rate_including_portrait_and_ultrawide() {
        for (width, height, fps, stop) in [
            (1280, 720, 30, false),
            (1280, 720, 60, true),
            (1920, 1080, 30, true),
            (720, 1280, 30, false),
            (3840, 720, 30, true),
            (640, 360, 60, false),
            (u32::MAX, u32::MAX, u32::MAX, true),
        ] {
            assert_eq!(
                stop_software_encoding(
                    settings(Encoder::Auto, width, height, fps),
                    true,
                    "OpenH264"
                ),
                stop,
                "{width}x{height}@{fps}"
            );
        }
    }

    #[test]
    fn gpu_mode_rejects_software_even_at_low_resolution() {
        assert!(stop_software_encoding(
            settings(Encoder::Gpu, 640, 360, 15),
            false,
            "OpenH264"
        ));
    }
}
