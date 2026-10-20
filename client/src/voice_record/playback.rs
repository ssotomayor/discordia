use super::*;

const SAMPLES: usize = 480;
const BLOCKS: usize = 64;

#[derive(Clone, Copy)]
struct Block {
    generation: u64,
    at: Instant,
    sample_index: u64,
    used: usize,
    raw: [f32; SAMPLES],
    output: [f32; SAMPLES],
    min_gain: f32,
    callback_gap_us: u128,
    callback_frames: usize,
    lock_wait_us: u128,
    min_buffer_samples: usize,
    starved_samples: u64,
    dropped_samples: u64,
}

impl Block {
    fn new() -> Self {
        Self {
            generation: 0,
            at: Instant::now(),
            sample_index: 0,
            used: 0,
            raw: [0.0; SAMPLES],
            output: [0.0; SAMPLES],
            min_gain: 1.0,
            callback_gap_us: 0,
            callback_frames: 0,
            lock_wait_us: 0,
            min_buffer_samples: 0,
            starved_samples: 0,
            dropped_samples: 0,
        }
    }
}

pub struct Playback {
    tx: Option<SyncSender<Block>>,
    block: Block,
    previous: Option<Instant>,
    sample_index: u64,
    dropped_samples: u64,
    rate: u32,
}

impl Playback {
    pub fn new(rate: u32) -> Self {
        let (mut recorder, rx) = Self::channel(rate);
        if let Err(error) = std::thread::Builder::new()
            .name("playback-record".into())
            .spawn(move || record_loop(rx, rate))
        {
            tracing::warn!(%error, "playback recording unavailable");
            recorder.tx = None;
        }
        recorder
    }

    fn channel(rate: u32) -> (Self, Receiver<Block>) {
        let (tx, rx) = std::sync::mpsc::sync_channel(BLOCKS);
        (
            Self {
                tx: Some(tx),
                block: Block::new(),
                previous: None,
                sample_index: 0,
                dropped_samples: 0,
                rate: rate.max(1),
            },
            rx,
        )
    }

    pub fn begin(
        &mut self,
        at: Instant,
        frames: usize,
        lock_wait: Duration,
        min_buffer_samples: usize,
        starved_samples: u64,
    ) {
        self.begin_generation(generation(), at);
        self.block.callback_frames = frames;
        self.block.lock_wait_us = lock_wait.as_micros();
        self.block.min_buffer_samples = min_buffer_samples;
        self.block.starved_samples = starved_samples;
    }

    fn begin_generation(&mut self, generation: u64, at: Instant) {
        self.block.generation = generation;
        self.block.at = at;
        self.block.sample_index = self.sample_index;
        self.block.used = 0;
        self.block.min_gain = 1.0;
        self.block.callback_gap_us = self.previous.map_or(0, |previous| {
            at.saturating_duration_since(previous).as_micros()
        });
        self.previous = Some(at);
    }

    pub fn push(&mut self, raw: f32, output: f32, gain: f32) {
        self.sample_index += 1;
        if self.block.generation == 0 || self.tx.is_none() {
            return;
        }
        let i = self.block.used;
        self.block.raw[i] = raw;
        self.block.output[i] = output;
        self.block.min_gain = self.block.min_gain.min(gain);
        self.block.used += 1;
        if self.block.used == SAMPLES {
            self.finish();
        }
    }

    pub fn finish(&mut self) {
        if self.block.used == 0 {
            return;
        }
        self.block.dropped_samples = self.dropped_samples;
        // Fixed blocks keep allocation and file/session locks off the device callback.
        if let Some(tx) = &self.tx
            && tx.try_send(self.block).is_err()
        {
            self.dropped_samples += self.block.used as u64;
        }
        self.block.at += Duration::from_secs_f64(self.block.used as f64 / f64::from(self.rate));
        self.block.sample_index = self.sample_index;
        self.block.used = 0;
        self.block.min_gain = 1.0;
    }
}

struct Files {
    session: Arc<Session>,
    before: u64,
    after: u64,
    csv: u64,
}

impl Files {
    fn open(session: Arc<Session>, rate: u32) -> Option<Self> {
        let before = session.next_id.fetch_add(1, Ordering::Relaxed);
        let after = session.next_id.fetch_add(1, Ordering::Relaxed);
        let csv = session.next_id.fetch_add(1, Ordering::Relaxed);
        for (id, name) in [(before, "playback-mix"), (after, "playback-out")] {
            session
                .tx
                .try_send(Op::FloatWav {
                    id,
                    path: session.path(name, "wav"),
                    rate,
                })
                .ok()?;
        }
        session.tx.try_send(Op::Csv {
            id: csv,
            path: session.path("playback", "csv"),
            header: "t_ms,sample_index,samples,rate,raw_dbfs,raw_peak,out_dbfs,out_peak,limiter_reduction_db,callback_gap_us,callback_frames,lock_wait_us,min_buffer_samples,starved_samples,record_dropped_samples".into(),
        }).ok()?;
        Some(Self {
            session,
            before,
            after,
            csv,
        })
    }

    fn write(&self, block: &Block, rate: u32) {
        let Some(elapsed) = block.at.checked_duration_since(self.session.start) else {
            return;
        };
        let raw = &block.raw[..block.used];
        let output = &block.output[..block.used];
        let peak = |samples: &[f32]| samples.iter().fold(0.0_f32, |p, s| p.max(s.abs()));
        self.session.send(Op::Playback {
            before: self.before,
            after: self.after,
            csv: self.csv,
            raw: raw.to_vec(),
            output: output.to_vec(),
            line: format!(
                "{},{},{},{},{:.1},{:.6},{:.1},{:.6},{:.2},{},{},{},{},{},{}",
                elapsed.as_millis(),
                block.sample_index,
                block.used,
                rate,
                rms_dbfs(raw),
                peak(raw),
                rms_dbfs(output),
                peak(output),
                -20.0 * block.min_gain.max(f32::MIN_POSITIVE).log10(),
                block.callback_gap_us,
                block.callback_frames,
                block.lock_wait_us,
                block.min_buffer_samples,
                block.starved_samples,
                block.dropped_samples,
            ),
        });
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        for id in [self.before, self.after, self.csv] {
            self.session.send(Op::Close { id });
        }
    }
}

fn record_loop(rx: Receiver<Block>, rate: u32) {
    let mut files: Option<Files> = None;
    loop {
        if files
            .as_ref()
            .is_some_and(|files| files.session.generation != generation())
        {
            files = None;
        }
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(block) => {
                let Some(session) = session().filter(|s| s.generation == block.generation) else {
                    continue;
                };
                if files
                    .as_ref()
                    .is_none_or(|files| files.session.generation != block.generation)
                {
                    files = Files::open(session, rate);
                }
                if let Some(files) = &files {
                    files.write(&block, rate);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorded_mix_preserves_overload_and_native_rate_beside_actual_output() {
        let dir = std::env::temp_dir().join(format!("playback-record-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (tx, rx) = std::sync::mpsc::sync_channel(16);
        let writer = std::thread::spawn(move || write_loop(rx));
        let session = Arc::new(Session {
            generation: 1,
            dir: dir.clone(),
            start: Instant::now(),
            tx,
            next_id: AtomicU64::new(1),
            names: Mutex::new(HashMap::new()),
            overflowed: AtomicBool::new(false),
        });
        let files = Files::open(session.clone(), 44_100).unwrap();
        let mut block = Block::new();
        block.at = session.start + Duration::from_millis(12);
        block.used = 2;
        block.raw[..2].copy_from_slice(&[1.74, -1.74]);
        block.output[..2].copy_from_slice(&[0.99, -0.99]);
        block.min_gain = 0.5;
        block.callback_gap_us = 64_000;
        block.starved_samples = 858;
        files.write(&block, 44_100);
        drop(files);
        drop(session);
        writer.join().unwrap();
        let raw = std::fs::read(dir.join("playback-mix.wav")).unwrap();
        let output = std::fs::read(dir.join("playback-out.wav")).unwrap();
        assert_eq!(&raw[..44], &audio_header(44_100, 8, true));
        assert_eq!(&output[..44], &audio_header(44_100, 8, true));
        assert_eq!(&raw[44..48], &1.74_f32.to_le_bytes());
        assert_eq!(&raw[48..52], &(-1.74_f32).to_le_bytes());
        assert_eq!(&output[44..48], &0.99_f32.to_le_bytes());
        let csv = std::fs::read_to_string(dir.join("playback.csv")).unwrap();
        let row: Vec<_> = csv.lines().nth(1).unwrap().split(',').collect();
        assert_eq!(&row[..4], &["12", "0", "2", "44100"]);
        assert_eq!(row[5], "1.740000");
        assert_eq!(row[8], "6.02");
        assert_eq!(row[9], "64000");
        assert_eq!(row[13], "858");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn overload_drops_diagnostics_and_recovers_without_blocking_playback() {
        let (mut recorder, rx) = Playback::channel(48_000);
        let at = Instant::now();
        for _ in 0..BLOCKS + 3 {
            recorder.begin_generation(1, at);
            for _ in 0..SAMPLES {
                recorder.push(1.5, 0.9, 0.6);
            }
        }
        assert_eq!(recorder.dropped_samples, 3 * SAMPLES as u64);
        for _ in 0..BLOCKS {
            let block = rx.try_recv().unwrap();
            assert_eq!(block.raw, [1.5; SAMPLES]);
            assert_eq!(block.output, [0.9; SAMPLES]);
        }
        recorder.begin_generation(1, at);
        recorder.push(0.2, 0.2, 1.0);
        recorder.finish();
        let recovered = rx.try_recv().unwrap();
        assert_eq!(recovered.dropped_samples, 3 * SAMPLES as u64);
        assert_eq!(recovered.used, 1);
        assert_eq!(recovered.sample_index, (BLOCKS + 3) as u64 * SAMPLES as u64);
    }

    #[test]
    fn callback_sizes_rates_and_recording_changes_preserve_sample_boundaries() {
        for rate in [44_100, 48_000, 96_000] {
            let (mut recorder, rx) = Playback::channel(rate);
            let at = Instant::now();
            recorder.begin_generation(0, at);
            recorder.push(9.0, 9.0, 1.0);
            recorder.finish();
            assert!(rx.try_recv().is_err());
            recorder.begin_generation(1, at);
            for _ in 0..SAMPLES + 17 {
                recorder.push(0.4, 0.3, 0.75);
            }
            recorder.finish();
            let first = rx.try_recv().unwrap();
            let tail = rx.try_recv().unwrap();
            assert_eq!((first.used, tail.used), (SAMPLES, 17));
            assert_eq!((first.sample_index, tail.sample_index), (1, 481));
            assert_eq!(
                tail.at - first.at,
                Duration::from_secs_f64(480.0 / f64::from(rate))
            );
            recorder.begin_generation(2, at + Duration::from_secs(1));
            recorder.push(-0.2, -0.2, 1.0);
            recorder.finish();
            let next = rx.try_recv().unwrap();
            assert_eq!(next.generation, 2);
            assert_eq!(next.used, 1);
            assert_eq!(next.raw[0], -0.2);
            assert_eq!(next.callback_gap_us, 1_000_000);
        }
    }
}
