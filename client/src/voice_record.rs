//! Records each stage of a call's audio, with the figures beside it, so a level
//! swing can be placed before or after the network. Off until Diagnostics starts it.

use parking_lot::{Mutex, RwLock};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::time::{Duration, Instant};

mod playback;
pub use playback::Playback;

const QUEUE: usize = 8192;
/// A header is patched this often, so a crash or a killed app still leaves files that play.
const PATCH_EVERY: Duration = Duration::from_secs(1);

enum Op {
    Wav {
        id: u64,
        path: PathBuf,
        rate: u32,
    },
    PlaybackStart {
        ids: [u64; 3],
        paths: [PathBuf; 3],
        rate: u32,
        header: String,
    },
    Playback {
        before: u64,
        after: u64,
        csv: u64,
        raw: Vec<f32>,
        output: Vec<f32>,
        line: String,
    },
    Csv {
        id: u64,
        path: PathBuf,
        header: String,
    },
    Samples {
        id: u64,
        data: Vec<i16>,
    },
    Line {
        id: u64,
        line: String,
    },
    Close {
        id: u64,
    },
}

struct Session {
    generation: u64,
    dir: PathBuf,
    start: Instant,
    tx: SyncSender<Op>,
    next_id: AtomicU64,
    names: Mutex<HashMap<String, u32>>,
    overflowed: AtomicBool,
}

static ON: AtomicBool = AtomicBool::new(false);
static GENERATION: AtomicU64 = AtomicU64::new(0);
static ACTIVE_GENERATION: AtomicU64 = AtomicU64::new(0);
static SESSION: RwLock<Option<Arc<Session>>> = RwLock::new(None);

fn session() -> Option<Arc<Session>> {
    if !ON.load(Ordering::Relaxed) {
        return None;
    }
    SESSION.read().clone()
}

/// Starts a fresh folder under `root`, or under the config dir when `root` is `None`.
pub fn start(root: Option<PathBuf>) -> Result<PathBuf, String> {
    stop();
    let root = root.unwrap_or_else(|| crate::identity::config_dir().join("voice-recordings"));
    let dir = root.join(chrono::Local::now().format("%Y%m%d-%H%M%S").to_string());
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let (tx, rx) = std::sync::mpsc::sync_channel(QUEUE);
    std::thread::Builder::new()
        .name("voice-record".into())
        .spawn(move || write_loop(rx))
        .map_err(|e| e.to_string())?;
    tracing::info!(dir = %dir.display(), "voice recording on");
    let generation = GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
    *SESSION.write() = Some(Arc::new(Session {
        generation,
        dir: dir.clone(),
        start: Instant::now(),
        tx,
        next_id: AtomicU64::new(1),
        names: Mutex::new(HashMap::new()),
        overflowed: AtomicBool::new(false),
    }));
    ON.store(true, Ordering::Relaxed);
    ACTIVE_GENERATION.store(generation, Ordering::Release);
    Ok(dir)
}

/// The writer finishes every file once the last sender is gone.
pub fn stop() {
    ACTIVE_GENERATION.store(0, Ordering::Release);
    ON.store(false, Ordering::Relaxed);
    if SESSION.write().take().is_some() {
        tracing::info!("voice recording off");
    }
}

pub fn generation() -> u64 {
    ACTIVE_GENERATION.load(Ordering::Acquire)
}

pub fn folder() -> Option<PathBuf> {
    session().map(|s| s.dir.clone())
}

pub fn start_from_env() {
    if let Some(root) = std::env::var_os("DISCORDIA_VOICE_RECORD").filter(|v| !v.is_empty())
        && let Err(error) = start(Some(root.into()))
    {
        tracing::warn!(%error, "voice recording unavailable");
    }
}

impl Session {
    fn path(&self, name: &str, ext: &str) -> PathBuf {
        let clean: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let mut names = self.names.lock();
        let n = names.entry(format!("{clean}.{ext}")).or_insert(0);
        *n += 1;
        let file = if *n == 1 {
            format!("{clean}.{ext}")
        } else {
            format!("{clean}-{n}.{ext}")
        };
        self.dir.join(file)
    }

    fn send(&self, op: Op) {
        if let Err(TrySendError::Full(_)) = self.tx.try_send(op)
            && !self.overflowed.swap(true, Ordering::Relaxed)
        {
            tracing::warn!("voice recording fell behind the disk; some audio is missing from it");
        }
    }
}

/// A file that opens itself in whichever session is running when it is next written,
/// so a stream already playing when recording starts is still recorded.
struct Sink {
    name: String,
    open: Option<(u64, u64)>,
}

impl Sink {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            open: None,
        }
    }

    fn target(
        &mut self,
        ext: &str,
        op: impl FnOnce(u64, PathBuf) -> Op,
    ) -> Option<(Arc<Session>, u64)> {
        let session = session()?;
        if let Some((generation, id)) = self.open
            && generation == session.generation
        {
            return Some((session, id));
        }
        let id = session.next_id.fetch_add(1, Ordering::Relaxed);
        let path = session.path(&self.name, ext);
        // A dropped open would send every later write nowhere, so try again next time.
        session.tx.try_send(op(id, path)).ok()?;
        self.open = Some((session.generation, id));
        Some((session, id))
    }
}

impl Drop for Sink {
    fn drop(&mut self) {
        if let (Some((generation, id)), Some(session)) = (self.open, session())
            && generation == session.generation
        {
            let _ = session.tx.try_send(Op::Close { id });
        }
    }
}

/// Mono 16-bit PCM.
pub struct Wav {
    sink: Sink,
    rate: u32,
}

impl Wav {
    pub fn new(name: &str, rate: u32) -> Self {
        Self {
            sink: Sink::new(name),
            rate,
        }
    }

    pub fn write(&mut self, samples: &[f32]) {
        if let Some((session, id)) = self.target() {
            let data = samples
                .iter()
                .map(|s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
                .collect();
            session.send(Op::Samples { id, data });
        }
    }

    pub fn write_i16(&mut self, samples: &[i16]) {
        if let Some((session, id)) = self.target() {
            session.send(Op::Samples {
                id,
                data: samples.to_vec(),
            });
        }
    }

    fn target(&mut self) -> Option<(Arc<Session>, u64)> {
        let rate = self.rate;
        self.sink
            .target("wav", |id, path| Op::Wav { id, path, rate })
    }
}

/// Each row is prefixed with `t_ms`, one clock shared by every file of a session.
pub struct Csv {
    sink: Sink,
    header: String,
}

impl Csv {
    pub fn new(name: &str, header: &str) -> Self {
        Self {
            sink: Sink::new(name),
            header: format!("t_ms,{header}"),
        }
    }

    pub fn is_on(&self) -> bool {
        ON.load(Ordering::Relaxed)
    }

    pub fn row(&mut self, line: impl FnOnce() -> String) {
        let header = &self.header;
        let target = self.sink.target("csv", |id, path| Op::Csv {
            id,
            path,
            header: header.clone(),
        });
        if let Some((session, id)) = target {
            let line = format!("{},{}", session.start.elapsed().as_millis(), line());
            session.send(Op::Line { id, line });
        }
    }
}

pub fn rms_dbfs(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return -120.0;
    }
    let mean = samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32;
    (10.0 * mean.max(1e-12).log10()).max(-120.0)
}

enum Out {
    Wav {
        file: BufWriter<File>,
        bytes: u32,
        rate: u32,
        float: bool,
    },
    Csv(BufWriter<File>),
}

impl Out {
    fn patch(&mut self) -> std::io::Result<()> {
        match self {
            Out::Wav {
                file,
                bytes,
                rate,
                float,
            } => {
                file.flush()?;
                let inner = file.get_mut();
                inner.seek(SeekFrom::Start(0))?;
                inner.write_all(&audio_header(*rate, *bytes, *float))?;
                inner.seek(SeekFrom::End(0))?;
                Ok(())
            }
            Out::Csv(file) => file.flush(),
        }
    }
}

fn wav_header(rate: u32, bytes: u32) -> [u8; 44] {
    let mut h = [0u8; 44];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&(36 + bytes).to_le_bytes());
    h[8..16].copy_from_slice(b"WAVEfmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes());
    h[22..24].copy_from_slice(&1u16.to_le_bytes());
    h[24..28].copy_from_slice(&rate.to_le_bytes());
    h[28..32].copy_from_slice(&(rate * 2).to_le_bytes());
    h[32..34].copy_from_slice(&2u16.to_le_bytes());
    h[34..36].copy_from_slice(&16u16.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&bytes.to_le_bytes());
    h
}

fn audio_header(rate: u32, bytes: u32, float: bool) -> [u8; 44] {
    let mut header = wav_header(rate, bytes);
    if float {
        header[20..22].copy_from_slice(&3u16.to_le_bytes());
        header[28..32].copy_from_slice(&(rate * 4).to_le_bytes());
        header[32..34].copy_from_slice(&4u16.to_le_bytes());
        header[34..36].copy_from_slice(&32u16.to_le_bytes());
    }
    header
}

fn write_float(outs: &mut HashMap<u64, Out>, id: u64, samples: &[f32]) {
    if let Some(Out::Wav { file, bytes, .. }) = outs.get_mut(&id) {
        for sample in samples {
            let _ = file.write_all(&sample.to_le_bytes());
        }
        *bytes = bytes.saturating_add(4 * samples.len() as u32);
    }
}

fn create(path: &Path, first: &[u8]) -> Option<BufWriter<File>> {
    let mut file = match File::create(path) {
        Ok(file) => BufWriter::new(file),
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "voice recording file unavailable");
            return None;
        }
    };
    file.write_all(first).ok()?;
    Some(file)
}

fn write_loop(rx: Receiver<Op>) {
    let mut outs = HashMap::<u64, Out>::new();
    let mut patched = Instant::now();
    loop {
        match rx.recv_timeout(PATCH_EVERY) {
            Ok(Op::Wav { id, path, rate }) => {
                if let Some(file) = create(&path, &wav_header(rate, 0)) {
                    outs.insert(
                        id,
                        Out::Wav {
                            file,
                            bytes: 0,
                            rate,
                            float: false,
                        },
                    );
                }
            }
            Ok(Op::PlaybackStart {
                ids: [before, after, csv],
                paths: [before_path, after_path, csv_path],
                rate,
                header,
            }) => {
                for (id, path) in [(before, before_path), (after, after_path)] {
                    if let Some(file) = create(&path, &audio_header(rate, 0, true)) {
                        outs.insert(
                            id,
                            Out::Wav {
                                file,
                                bytes: 0,
                                rate,
                                float: true,
                            },
                        );
                    }
                }
                if let Some(file) = create(&csv_path, format!("{header}\n").as_bytes()) {
                    outs.insert(csv, Out::Csv(file));
                }
            }
            Ok(Op::Playback {
                before,
                after,
                csv,
                raw,
                output,
                line,
            }) => {
                write_float(&mut outs, before, &raw);
                write_float(&mut outs, after, &output);
                if let Some(Out::Csv(file)) = outs.get_mut(&csv) {
                    let _ = writeln!(file, "{line}");
                }
            }
            Ok(Op::Csv { id, path, header }) => {
                if let Some(file) = create(&path, format!("{header}\n").as_bytes()) {
                    outs.insert(id, Out::Csv(file));
                }
            }
            Ok(Op::Samples { id, data }) => {
                if let Some(Out::Wav { file, bytes, .. }) = outs.get_mut(&id) {
                    for s in &data {
                        let _ = file.write_all(&s.to_le_bytes());
                    }
                    *bytes = bytes.saturating_add(2 * data.len() as u32);
                }
            }
            Ok(Op::Line { id, line }) => {
                if let Some(Out::Csv(file)) = outs.get_mut(&id) {
                    let _ = writeln!(file, "{line}");
                }
            }
            Ok(Op::Close { id }) => {
                if let Some(mut out) = outs.remove(&id) {
                    let _ = out.patch();
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                for out in outs.values_mut() {
                    let _ = out.patch();
                }
                break;
            }
        }
        if patched.elapsed() >= PATCH_EVERY {
            for out in outs.values_mut() {
                let _ = out.patch();
            }
            patched = Instant::now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_closed_wav_states_its_own_length_and_a_csv_keeps_its_rows() {
        let dir = std::env::temp_dir().join(format!("voice-record-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (tx, rx) = std::sync::mpsc::sync_channel(16);
        let writer = std::thread::spawn(move || write_loop(rx));
        let wav = dir.join("a.wav");
        let csv = dir.join("a.csv");
        tx.send(Op::Wav {
            id: 1,
            path: wav.clone(),
            rate: 48_000,
        })
        .unwrap();
        tx.send(Op::Samples {
            id: 1,
            data: vec![1, -1, 2],
        })
        .unwrap();
        tx.send(Op::Samples {
            id: 1,
            data: vec![3],
        })
        .unwrap();
        tx.send(Op::Csv {
            id: 2,
            path: csv.clone(),
            header: "t_ms,x".into(),
        })
        .unwrap();
        tx.send(Op::Line {
            id: 2,
            line: "5,1".into(),
        })
        .unwrap();
        tx.send(Op::Close { id: 1 }).unwrap();
        tx.send(Op::Close { id: 2 }).unwrap();
        drop(tx);
        writer.join().unwrap();
        let bytes = std::fs::read(&wav).unwrap();
        assert_eq!(bytes.len(), 44 + 8);
        assert_eq!(bytes[..44], wav_header(48_000, 8));
        assert_eq!(&bytes[44..46], &1i16.to_le_bytes());
        assert_eq!(std::fs::read_to_string(&csv).unwrap(), "t_ms,x\n5,1\n");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_stream_already_playing_is_recorded_from_when_recording_starts() {
        let root = std::env::temp_dir().join(format!("voice-session-{}", std::process::id()));
        let mut wav = Wav::new("recv-alice", 48_000);
        let mut csv = Csv::new("recv-alice", "x");
        wav.write(&[0.5; 10]);
        csv.row(|| "before".into());
        let dir = start(Some(root.clone())).unwrap();
        assert_eq!(folder(), Some(dir.clone()));
        wav.write(&[0.5; 10]);
        csv.row(|| "during".into());
        stop();
        wav.write(&[0.5; 10]);
        csv.row(|| "after".into());
        let path = dir.join("recv-alice.wav");
        let deadline = Instant::now() + Duration::from_secs(5);
        while std::fs::read(&path).map_or(true, |b| b.get(40..44) != Some(&20u32.to_le_bytes()[..]))
        {
            assert!(Instant::now() < deadline, "header never patched");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(std::fs::read(&path).unwrap().len(), 44 + 20);
        let rows = std::fs::read_to_string(dir.join("recv-alice.csv")).unwrap();
        assert!(
            rows.starts_with("t_ms,x\n") && rows.ends_with(",during\n"),
            "{rows}"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
