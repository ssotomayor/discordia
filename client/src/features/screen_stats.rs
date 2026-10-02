use serde_json::Value;

use crate::state::ScreenShareStats;

#[derive(Default)]
pub(super) struct WebViewStats {
    outbound: Option<Sample>,
    inbound: Option<Sample>,
}

struct Sample {
    key: String,
    timestamp_ms: f64,
    bytes: u64,
    frames: Option<u64>,
}

fn number(msg: &Value, key: &str) -> Option<f64> {
    msg.get(key)
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite() && *v >= 0.0)
}

fn rounded_u32(value: f64) -> Option<u32> {
    let value = value.round();
    (value.is_finite() && value >= 0.0 && value <= u32::MAX as f64).then_some(value as u32)
}

impl WebViewStats {
    pub(super) fn update(&mut self, msg: &Value, outbound: bool) -> Option<ScreenShareStats> {
        let previous = if outbound {
            &mut self.outbound
        } else {
            &mut self.inbound
        };
        if msg.get("active").and_then(Value::as_bool) != Some(true) {
            *previous = None;
            return None;
        }
        let u32_field = |key| {
            msg.get(key)
                .and_then(Value::as_u64)
                .and_then(|v| u32::try_from(v).ok())
        };
        let u64_field = |key| msg.get(key).and_then(Value::as_u64);
        let text_field = |key| msg.get(key).and_then(Value::as_str).map(str::to_owned);
        let current = (|| {
            Some(Sample {
                key: text_field("sampleKey")?,
                timestamp_ms: number(msg, "timestampMs")?,
                bytes: u64_field("bytes")?,
                frames: u64_field("frames"),
            })
        })();
        let delta = previous
            .as_ref()
            .zip(current.as_ref())
            .and_then(|(old, new)| {
                let elapsed = new.timestamp_ms - old.timestamp_ms;
                (old.key == new.key && elapsed > 0.0 && elapsed.is_finite())
                    .then_some((old, new, elapsed))
            });
        let bitrate_kbps = delta.and_then(|(old, new, elapsed)| {
            rounded_u32(new.bytes.checked_sub(old.bytes)? as f64 * 8.0 / elapsed)
        });
        let estimated_fps = delta
            .and_then(|(old, new, elapsed)| {
                Some(new.frames?.checked_sub(old.frames?)? as f64 * 1000.0 / elapsed)
            })
            .filter(|v| v.is_finite());
        *previous = current;
        Some(ScreenShareStats {
            outbound,
            capture_width: u32_field("captureWidth"),
            capture_height: u32_field("captureHeight"),
            capture_fps: number(msg, "captureFps"),
            capture_processing_ms: None,
            encode_ms: None,
            encoded_width: u32_field("encodedWidth"),
            encoded_height: u32_field("encodedHeight"),
            encoded_fps: number(msg, "encodedFps").or(estimated_fps),
            bitrate_kbps,
            target_bitrate_kbps: number(msg, "targetBitrate").and_then(|v| rounded_u32(v / 1000.0)),
            codec: text_field("codec"),
            codec_implementation: text_field("codecImplementation"),
            power_efficient: msg.get("powerEfficient").and_then(Value::as_bool),
            quality_limitation_reason: text_field("qualityLimitationReason"),
            frames: u64_field("frames"),
            packets: u64_field("packets"),
            packets_lost: msg.get("packetsLost").and_then(Value::as_i64),
            jitter_ms: number(msg, "jitterSeconds")
                .map(|v| v * 1000.0)
                .filter(|v| v.is_finite()),
            error: text_field("error"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample(key: &str, timestamp: f64, bytes: u64, frames: u64) -> Value {
        json!({"active": true, "sampleKey": key, "timestampMs": timestamp, "bytes": bytes,
            "frames": frames, "targetBitrate": 8_000_000, "jitterSeconds": 0.014,
            "encodedWidth": 2560, "encodedHeight": 1440, "codec": "video/H264"})
    }

    #[test]
    fn rates_and_units_match_browser_counters() {
        let mut stats = WebViewStats::default();
        assert_eq!(
            stats
                .update(&sample("a", 1000.0, 500, 10), true)
                .unwrap()
                .bitrate_kbps,
            None
        );
        let value = stats
            .update(&sample("a", 2000.0, 1_000_500, 40), true)
            .unwrap();
        assert_eq!(value.bitrate_kbps, Some(8000));
        assert_eq!(value.target_bitrate_kbps, Some(8000));
        assert_eq!(value.encoded_fps, Some(30.0));
        assert_eq!(value.jitter_ms, Some(14.0));
        assert_eq!(value.encoded_width, Some(2560));
        assert_eq!(value.codec.as_deref(), Some("video/H264"));
        let mut next = sample("a", 3000.0, 1_000_500, 40);
        next["encodedFps"] = json!(0);
        let value = stats.update(&next, true).unwrap();
        assert_eq!(value.encoded_fps, Some(0.0));
        assert_eq!(value.bitrate_kbps, Some(0));
    }

    #[test]
    fn stream_changes_resets_and_directions_never_mix() {
        let mut stats = WebViewStats::default();
        stats.update(&sample("a", 1000.0, 1000, 10), true);
        assert_eq!(
            stats
                .update(&sample("a", 2000.0, 2000, 40), false)
                .unwrap()
                .bitrate_kbps,
            None
        );
        assert_eq!(
            stats
                .update(&sample("b", 2000.0, 2000, 40), true)
                .unwrap()
                .bitrate_kbps,
            None
        );
        let value = stats.update(&sample("b", 3000.0, 0, 0), true).unwrap();
        assert_eq!(value.bitrate_kbps, None);
        assert_eq!(value.encoded_fps, None);
        assert!(stats.update(&json!({"active": false}), true).is_none());
        assert_eq!(
            stats
                .update(&sample("b", 4000.0, 1000, 30), true)
                .unwrap()
                .bitrate_kbps,
            None
        );
    }

    #[test]
    fn duplicate_backwards_and_invalid_samples_do_not_invent_rates() {
        let mut stats = WebViewStats::default();
        stats.update(&sample("a", 1000.0, 1000, 10), false);
        for timestamp in [1000.0, 500.0] {
            let value = stats
                .update(&sample("a", timestamp, 2000, 20), false)
                .unwrap();
            assert_eq!(value.bitrate_kbps, None);
            assert_eq!(value.encoded_fps, None);
        }
        let mut invalid = sample("a", 1500.0, 3000, 30);
        invalid["timestampMs"] = json!(-1);
        invalid["encodedFps"] = json!(-30);
        invalid["targetBitrate"] = json!(1e100);
        invalid["jitterSeconds"] = json!(-0.1);
        let value = stats.update(&invalid, false).unwrap();
        assert_eq!(value.bitrate_kbps, None);
        assert_eq!(value.encoded_fps, None);
        assert_eq!(value.target_bitrate_kbps, None);
        assert_eq!(value.jitter_ms, None);
        assert_eq!(
            stats
                .update(&sample("a", 2000.0, 4000, 40), false)
                .unwrap()
                .bitrate_kbps,
            None
        );
    }
}
