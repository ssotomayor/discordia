// Copyright 2025 LiveKit, Inc.
// SPDX-License-Identifier: Apache-2.0

pub(crate) fn start_bitrate_kbps(target_bps: Option<u64>, start_bps: Option<u64>) -> Option<u32> {
    let target_kbps = (target_bps? / 1000).min(u64::from(u32::MAX)) as u32;
    if target_kbps < 300 {
        return None;
    }
    // Unconfigured tracks retain the SDK default; a hint never imposes a minimum bitrate.
    let hint_kbps = start_bps
        .filter(|bps| *bps >= 1000)
        .map(|bps| (bps / 1000).min(u64::from(u32::MAX)) as u32)
        .unwrap_or(1000);
    let budget_kbps = (target_kbps as f64 * 0.9).round() as u32;
    Some(budget_kbps.min(target_kbps).min(hint_kbps))
}

#[cfg(test)]
mod tests {
    use super::start_bitrate_kbps;

    #[test]
    fn startup_hints_preserve_defaults_and_stay_inside_encoding_budget() {
        assert_eq!(start_bitrate_kbps(None, Some(4_000_000)), None);
        assert_eq!(start_bitrate_kbps(Some(12_000_000), None), Some(1000));
        assert_eq!(
            start_bitrate_kbps(Some(12_000_000), Some(4_000_000)),
            Some(4000)
        );
        assert_eq!(
            start_bitrate_kbps(Some(3_000_000), Some(4_000_000)),
            Some(2700)
        );
        assert_eq!(start_bitrate_kbps(Some(12_000_000), Some(0)), Some(1000));
        assert_eq!(start_bitrate_kbps(Some(200_000), Some(4_000_000)), None);
        assert_eq!(
            start_bitrate_kbps(Some(u64::MAX), Some(u64::MAX)),
            Some(3_865_470_566)
        );
    }
}
