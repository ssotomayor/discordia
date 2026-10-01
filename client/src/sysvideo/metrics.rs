use std::time::Duration;

#[derive(Clone, Copy, Default)]
pub(crate) struct Snapshot {
    pub frames: u64,
    pub processing_us: u64,
    pub width: u32,
    pub height: u32,
}

#[derive(Default)]
pub(crate) struct Metrics(parking_lot::Mutex<Snapshot>);

impl Metrics {
    pub fn record(&self, width: u32, height: u32, elapsed: Duration) {
        let mut snapshot = self.0.lock();
        snapshot.frames += 1;
        snapshot.processing_us += elapsed.as_micros() as u64;
        snapshot.width = width;
        snapshot.height = height;
    }

    pub fn snapshot(&self) -> Snapshot {
        *self.0.lock()
    }
}

impl Snapshot {
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
