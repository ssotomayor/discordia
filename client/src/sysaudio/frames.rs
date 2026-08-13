//! Turning whatever the OS hands a backend into what the publish path wants:
//! mono `f32` frames of exactly 480 samples (10 ms at 48 kHz).
//!
//! Every backend ends up needing the same two things — fold the channels
//! together, then cut the result into whole frames while carrying the remainder
//! to the next callback — so they live here rather than once per platform.

use tokio::sync::mpsc::UnboundedSender;

/// Samples per frame: 10 ms at 48 kHz, matching the voice pipeline.
pub const FRAME: usize = 480;

/// Accumulates samples and emits whole frames.
///
/// One `Vec` is allocated per emitted frame because that is the channel's
/// element type; the partial-frame remainder is carried in a buffer that is
/// reused for the life of the capture.
///
/// **Only part of this is live on any one platform**, which is why several items
/// below carry a `cfg_attr` allow rather than being deleted. macOS calls exactly
/// `push_mono` — ScreenCaptureKit hands us planar audio we fold ourselves — while
/// Windows uses `emitted`, `push_silence` and `push_interleaved` (and through it
/// `scratch`), because WASAPI loopback delivers interleaved frames and goes
/// silent rather than continuing to deliver when nothing is playing. The allow is
/// scoped to *not Windows* rather than blanket, so the day a third backend stops
/// using one of these, the lint still says so.
pub struct FrameCutter {
    tx: UnboundedSender<Vec<f32>>,
    pending: Vec<f32>,
    /// Frames handed on so far. The Windows backend uses this to notice that
    /// the machine has gone quiet and top the stream up with silence.
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    emitted: u64,
    /// Reusable downmix scratch, so an interleaved push doesn't allocate.
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    scratch: Vec<f32>,
}

impl FrameCutter {
    pub fn new(tx: UnboundedSender<Vec<f32>>) -> Self {
        Self {
            tx,
            pending: Vec::with_capacity(FRAME * 4),
            emitted: 0,
            scratch: Vec::with_capacity(FRAME * 4),
        }
    }

    /// Frames emitted since the capture started.
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub fn emitted(&self) -> u64 {
        self.emitted
    }

    /// Push already-mono samples. Returns false once the receiver is gone,
    /// which is the signal to stop capturing.
    pub fn push_mono(&mut self, samples: &[f32]) -> bool {
        self.pending.extend_from_slice(samples);
        self.drain()
    }

    /// Push interleaved samples, averaging the channels down to mono. Averaging
    /// rather than taking one channel: a desktop mix is routinely hard-panned,
    /// and dropping a channel would silence whatever sits on the other side.
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub fn push_interleaved(&mut self, samples: &[f32], channels: usize) -> bool {
        if channels <= 1 {
            return self.push_mono(samples);
        }
        self.scratch.clear();
        let scale = 1.0 / channels as f32;
        for chunk in samples.chunks_exact(channels) {
            self.scratch.push(chunk.iter().sum::<f32>() * scale);
        }
        // `pending` and `scratch` are separate fields, so this is not the
        // double borrow it looks like — but the borrow checker only sees the
        // method call, hence the manual extend.
        self.pending.extend_from_slice(&self.scratch);
        self.drain()
    }

    /// Push `count` mono samples of silence.
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub fn push_silence(&mut self, count: usize) -> bool {
        self.pending.resize(self.pending.len() + count, 0.0);
        self.drain()
    }

    fn drain(&mut self) -> bool {
        while self.pending.len() >= FRAME {
            let frame: Vec<f32> = self.pending.drain(..FRAME).collect();
            self.emitted += 1;
            if self.tx.send(frame).is_err() {
                return false;
            }
        }
        true
    }
}
