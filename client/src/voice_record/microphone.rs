use super::*;
use crate::audio_queue::CapturePosition;

const HEADER: &str = "capture_id,first_sample,samples,age_ms,event,event_count,wav_first_sample,record_dropped_samples";

pub struct MicrophoneTrace {
    csv: Csv,
    wav: Wav,
    generation: u64,
    counts: HashMap<&'static str, u64>,
    wav_samples: u64,
    record_dropped_samples: u64,
    writer_dropped_samples: u64,
}

impl MicrophoneTrace {
    pub fn new(stage: &str, wav_name: &str) -> Self {
        Self {
            csv: Csv::new(stage, HEADER),
            wav: Wav::new(wav_name, 48_000),
            generation: 0,
            counts: HashMap::new(),
            wav_samples: 0,
            record_dropped_samples: 0,
            writer_dropped_samples: 0,
        }
    }

    pub fn record(
        &mut self,
        position: Option<CapturePosition>,
        captured: Instant,
        event: &'static str,
        samples: Option<&[i16]>,
    ) {
        self.record_generation(generation(), position, captured, event, samples);
    }

    fn record_generation(
        &mut self,
        expected_generation: u64,
        position: Option<CapturePosition>,
        captured: Instant,
        event: &'static str,
        samples: Option<&[i16]>,
    ) {
        let Some((session, csv)) = self.csv.sink.target("csv", |id, path| Op::Csv {
            id,
            path,
            header: format!("t_ms,{HEADER}"),
        }) else {
            return;
        };
        if session.generation != expected_generation {
            return;
        }
        let wav = if let Some(samples) = samples {
            let Some((wav_session, id)) = self.wav.target() else {
                return;
            };
            if wav_session.generation != session.generation {
                return;
            }
            Some((id, samples.to_vec()))
        } else {
            None
        };
        self.record_to(&session, csv, wav, (position, captured, event));
    }

    fn record_to(
        &mut self,
        session: &Session,
        csv: u64,
        wav: Option<(u64, Vec<i16>)>,
        (position, captured, event): (Option<CapturePosition>, Instant, &'static str),
    ) {
        let written = wav.is_some();
        let line = self.line(session.generation, position, captured, event, written);
        let line = format!("{},{}", session.start.elapsed().as_millis(), line);
        if !session.send(Op::Microphone { wav, csv, line }) {
            if written {
                self.wav_samples -= crate::audio_queue::SAMPLES as u64;
            }
            self.writer_dropped_samples += crate::audio_queue::SAMPLES as u64;
        }
    }

    pub fn record_float(
        &mut self,
        position: Option<CapturePosition>,
        captured: Instant,
        event: &'static str,
        samples: &[f32; crate::audio_queue::SAMPLES],
    ) {
        self.record_float_generation(generation(), position, captured, event, samples);
    }

    fn record_float_generation(
        &mut self,
        expected_generation: u64,
        position: Option<CapturePosition>,
        captured: Instant,
        event: &'static str,
        samples: &[f32; crate::audio_queue::SAMPLES],
    ) {
        if expected_generation == 0 {
            return;
        }
        let samples = samples.map(|s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16);
        self.record_generation(
            expected_generation,
            position,
            captured,
            event,
            Some(&samples),
        );
    }

    fn line(
        &mut self,
        generation: u64,
        position: Option<CapturePosition>,
        captured: Instant,
        event: &'static str,
        written: bool,
    ) -> String {
        if self.generation != generation {
            self.generation = generation;
            self.counts.clear();
            self.wav_samples = 0;
            self.writer_dropped_samples = 0;
        }
        let count = self.counts.entry(event).or_default();
        *count += 1;
        let wav_first = if written {
            let first = self.wav_samples;
            self.wav_samples += crate::audio_queue::SAMPLES as u64;
            first.to_string()
        } else {
            String::new()
        };
        let (capture_id, first_sample) = position.map_or_else(
            || (String::new(), String::new()),
            |p| (p.capture_id.to_string(), p.first_sample.to_string()),
        );
        format!(
            "{capture_id},{first_sample},{},{},{event},{count},{wav_first},{}",
            crate::audio_queue::SAMPLES,
            captured.elapsed().as_millis(),
            self.record_dropped_samples + self.writer_dropped_samples,
        )
    }
}

struct CaptureBlock {
    generation: u64,
    position: Option<CapturePosition>,
    captured: Instant,
    samples: [f32; crate::audio_queue::SAMPLES],
    outcome: &'static str,
    dropped_samples: u64,
}

pub struct CaptureRecorder {
    tx: Option<SyncSender<CaptureBlock>>,
    dropped_samples: u64,
    generation: u64,
}

impl CaptureRecorder {
    pub fn new() -> Self {
        let (mut recorder, rx) = Self::channel();
        if let Err(error) = std::thread::Builder::new()
            .name("mic-record".into())
            .spawn(move || {
                let mut trace = MicrophoneTrace::new("mic-capture", "mic-raw");
                while let Ok(block) = rx.recv() {
                    if generation() != block.generation {
                        continue;
                    }
                    trace.record_dropped_samples = block.dropped_samples;
                    trace.record_float_generation(
                        block.generation,
                        block.position,
                        block.captured,
                        block.outcome,
                        &block.samples,
                    );
                }
            })
        {
            tracing::warn!(%error, "Microphone recording unavailable");
            recorder.tx = None;
        }
        recorder
    }

    fn channel() -> (Self, Receiver<CaptureBlock>) {
        let (tx, rx) = std::sync::mpsc::sync_channel(64);
        (
            Self {
                tx: Some(tx),
                dropped_samples: 0,
                generation: 0,
            },
            rx,
        )
    }

    pub fn record(
        &mut self,
        samples: [f32; crate::audio_queue::SAMPLES],
        position: Option<CapturePosition>,
        captured: Instant,
        outcome: &'static str,
    ) {
        let generation = generation();
        if generation == 0 {
            return;
        }
        if generation != self.generation {
            self.generation = generation;
            self.dropped_samples = 0;
        }
        self.offer(CaptureBlock {
            generation,
            samples,
            position,
            captured,
            outcome,
            dropped_samples: self.dropped_samples,
        });
    }

    fn offer(&mut self, block: CaptureBlock) {
        // Fixed blocks keep allocation and recording/session locks off the device callback.
        if let Some(tx) = &self.tx
            && tx.try_send(block).is_err()
        {
            self.dropped_samples += crate::audio_queue::SAMPLES as u64;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_session(tx: SyncSender<Op>, dir: PathBuf) -> Session {
        Session {
            generation: 1,
            dir,
            start: Instant::now(),
            tx,
            next_id: AtomicU64::new(1),
            names: Mutex::new(HashMap::new()),
            overflowed: AtomicBool::new(false),
        }
    }

    #[test]
    fn disk_queue_overload_drops_the_audio_and_its_row_together_without_advancing_file_offsets() {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let session = test_session(tx, PathBuf::new());
        let mut trace = MicrophoneTrace::new("test", "test");
        session
            .tx
            .try_send(Op::Line {
                id: 99,
                line: "busy".into(),
            })
            .unwrap();
        let position = Some(CapturePosition {
            capture_id: 7,
            first_sample: 0,
        });
        trace.record_to(
            &session,
            2,
            Some((1, vec![123; 480])),
            (position, Instant::now(), "accepted"),
        );
        assert_eq!(trace.wav_samples, 0);
        assert_eq!(trace.writer_dropped_samples, 480);
        assert!(matches!(rx.try_recv(), Ok(Op::Line { id: 99, .. })));
        assert!(rx.try_recv().is_err());
        trace.record_to(
            &session,
            2,
            Some((1, vec![456; 480])),
            (position, Instant::now(), "accepted"),
        );
        match rx.try_recv().unwrap() {
            Op::Microphone {
                wav: Some((1, data)),
                csv: 2,
                line,
            } => {
                assert_eq!(data, vec![456; 480]);
                assert!(line.ends_with(",accepted,2,0,480"));
            }
            _ => panic!("audio and metadata must share a queue operation"),
        }
    }

    #[test]
    fn only_accepted_publications_appear_in_the_waveform_and_rejections_still_have_csv_rows() {
        let dir = std::env::temp_dir().join(format!("mic-publication-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (tx, rx) = std::sync::mpsc::sync_channel(16);
        let writer = std::thread::spawn(move || write_loop(rx));
        let session = test_session(tx, dir.clone());
        session
            .tx
            .send(Op::Wav {
                id: 1,
                path: dir.join("accepted.wav"),
                rate: 48_000,
            })
            .unwrap();
        session
            .tx
            .send(Op::Csv {
                id: 2,
                path: dir.join("publication.csv"),
                header: format!("t_ms,{HEADER}"),
            })
            .unwrap();
        let mut trace = MicrophoneTrace::new("test", "test");
        for (first_sample, event, samples) in [
            (0, "accepted", Some(vec![123; 480])),
            (480, "source_error", None),
            (960, "accepted", Some(vec![456; 480])),
        ] {
            let position = Some(CapturePosition {
                capture_id: 7,
                first_sample,
            });
            trace.record_to(
                &session,
                2,
                samples.map(|s| (1, s)),
                (position, Instant::now(), event),
            );
        }
        drop(session);
        writer.join().unwrap();
        let wav = std::fs::read(dir.join("accepted.wav")).unwrap();
        assert_eq!(wav.len(), 44 + 2 * 480 * 2);
        assert_eq!(&wav[44..46], &123_i16.to_le_bytes());
        assert_eq!(&wav[1004..1006], &456_i16.to_le_bytes());
        let csv = std::fs::read_to_string(dir.join("publication.csv")).unwrap();
        let rows: Vec<_> = csv.lines().collect();
        assert_eq!(rows.len(), 4);
        assert!(rows[2].contains(",7,480,480,"));
        assert!(rows[2].ends_with(",source_error,1,,0"));
        assert!(rows[3].ends_with(",accepted,2,480,0"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_blocks_and_rejected_publications_keep_positions_without_claiming_wav_audio() {
        let mut trace = MicrophoneTrace::new("test", "test");
        let now = Instant::now();
        let position = |first_sample| {
            Some(CapturePosition {
                capture_id: 7,
                first_sample,
            })
        };
        let accepted = trace.line(1, position(0), now, "accepted", true);
        let rejected = trace.line(1, position(480), now, "source_error", false);
        let resumed = trace.line(1, position(1440), now, "accepted", true);
        assert_eq!(accepted.split(',').count(), HEADER.split(',').count());
        assert!(accepted.ends_with(",accepted,1,0,0"));
        assert!(rejected.starts_with("7,480,480,"));
        assert!(rejected.ends_with(",source_error,1,,0"));
        assert!(resumed.starts_with("7,1440,480,"));
        assert!(resumed.ends_with(",accepted,2,480,0"));
        assert!(
            trace
                .line(2, position(1920), now, "accepted", true)
                .ends_with(",accepted,1,0,0")
        );
    }

    #[test]
    fn a_slow_recording_worker_cannot_block_capture_and_reports_missing_blocks() {
        let (mut recorder, rx) = CaptureRecorder::channel();
        for i in 0..100 {
            recorder.offer(CaptureBlock {
                generation: 1,
                samples: [0.0; 480],
                position: None,
                captured: Instant::now(),
                outcome: "queued",
                dropped_samples: recorder.dropped_samples,
            });
            if i < 64 {
                assert_eq!(recorder.dropped_samples, 0);
            }
        }
        assert_eq!(recorder.dropped_samples, 36 * 480);
        for _ in 0..64 {
            rx.try_recv().unwrap();
        }
        recorder.offer(CaptureBlock {
            generation: 1,
            samples: [0.0; 480],
            position: None,
            captured: Instant::now(),
            outcome: "queued",
            dropped_samples: recorder.dropped_samples,
        });
        assert_eq!(rx.try_recv().unwrap().dropped_samples, 36 * 480);
    }
}
