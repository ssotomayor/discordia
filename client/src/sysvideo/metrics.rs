use std::time::Duration;

#[derive(Clone, Copy, Default)]
pub(crate) struct Snapshot {
    pub frames: u64,
    pub processing_us: u64,
    pub width: u32,
    pub height: u32,
    pub callbacks: u64,
    pub paced_out: u64,
    pub largest_gap_us: u64,
    pub readback_us: u64,
    pub conversion_us: u64,
}

#[derive(Default)]
pub(crate) struct Metrics(parking_lot::Mutex<Snapshot>);

impl Metrics {
    #[cfg(any(target_os = "windows", test))]
    pub fn callback(&self, gap: Duration, paced_out: bool) {
        let mut snapshot = self.0.lock();
        snapshot.callbacks += 1;
        snapshot.paced_out += u64::from(paced_out);
        snapshot.largest_gap_us = snapshot.largest_gap_us.max(gap.as_micros() as u64);
    }

    #[cfg(any(target_os = "windows", test))]
    pub fn record_windows(
        &self,
        width: u32,
        height: u32,
        elapsed: Duration,
        readback: Duration,
        conversion: Duration,
    ) {
        let mut snapshot = self.0.lock();
        snapshot.readback_us += readback.as_micros() as u64;
        snapshot.conversion_us += conversion.as_micros() as u64;
        snapshot.record(width, height, elapsed);
    }

    #[cfg(any(target_os = "macos", test))]
    pub fn record(&self, width: u32, height: u32, elapsed: Duration) {
        let mut snapshot = self.0.lock();
        snapshot.record(width, height, elapsed);
    }

    pub fn snapshot(&self) -> Snapshot {
        *self.0.lock()
    }
}

impl Snapshot {
    fn record(&mut self, width: u32, height: u32, elapsed: Duration) {
        self.frames += 1;
        self.processing_us += elapsed.as_micros() as u64;
        self.width = width;
        self.height = height;
    }

    pub fn readback_ms_since(self, previous: Self) -> Option<f64> {
        self.stage_ms_since(previous, self.readback_us, previous.readback_us)
    }

    pub fn conversion_ms_since(self, previous: Self) -> Option<f64> {
        self.stage_ms_since(previous, self.conversion_us, previous.conversion_us)
    }

    fn stage_ms_since(self, previous: Self, current_us: u64, previous_us: u64) -> Option<f64> {
        let frames = self.frames.checked_sub(previous.frames)?;
        let elapsed = current_us.checked_sub(previous_us)?;
        (frames > 0 && self.callbacks > 0).then(|| elapsed as f64 / frames as f64 / 1000.0)
    }

    pub fn processing_ms_since(self, previous: Self) -> Option<f64> {
        let frames = self.frames.checked_sub(previous.frames)?;
        let elapsed = self.processing_us.checked_sub(previous.processing_us)?;
        (frames > 0).then(|| elapsed as f64 / frames as f64 / 1000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_stalls_are_distinct_from_pacing_and_conversion_costs() {
        let metrics = Metrics::default();
        metrics.callback(Duration::ZERO, false);
        metrics.record_windows(
            2560,
            1440,
            Duration::from_millis(3),
            Duration::from_millis(2),
            Duration::from_millis(1),
        );
        let before = metrics.snapshot();
        metrics.callback(Duration::from_millis(8), true);
        metrics.callback(Duration::from_millis(80), false);
        metrics.record_windows(
            2560,
            1440,
            Duration::from_millis(10),
            Duration::from_millis(7),
            Duration::from_millis(3),
        );
        let after = metrics.snapshot();
        assert_eq!(after.callbacks - before.callbacks, 2);
        assert_eq!(after.paced_out - before.paced_out, 1);
        assert_eq!(after.largest_gap_us, 80_000);
        assert_eq!(after.readback_ms_since(before), Some(7.0));
        assert_eq!(after.conversion_ms_since(before), Some(3.0));
        assert_eq!(after.readback_ms_since(after), None);
        assert_eq!(Snapshot::default().readback_ms_since(after), None);
    }

    #[test]
    fn measurements_belong_to_the_capture_and_use_interval_deltas() {
        let share = Metrics::default();
        let preview = Metrics::default();
        share.record(1920, 1080, Duration::from_millis(4));
        let before = share.snapshot();
        preview.record(320, 180, Duration::from_millis(30));
        share.record(1920, 1080, Duration::from_millis(2));
        share.record(1920, 1080, Duration::from_millis(6));
        let after = share.snapshot();
        assert_eq!(after.frames - before.frames, 2);
        assert_eq!(after.processing_ms_since(before), Some(4.0));
        assert_eq!((after.width, after.height), (1920, 1080));
        assert_eq!(after.processing_ms_since(after), None);
        assert_eq!(Snapshot::default().processing_ms_since(after), None);
    }
}
