use tokio::sync::mpsc::UnboundedSender;

pub const FRAME: usize = 480;

pub struct FrameCutter {
    tx: UnboundedSender<Vec<f32>>,
    pending: Vec<f32>,
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    emitted: u64,
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

    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub fn emitted(&self) -> u64 {
        self.emitted
    }

    pub fn push_mono(&mut self, samples: &[f32]) -> bool {
        self.pending.extend_from_slice(samples);
        self.drain()
    }

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
        self.pending.extend_from_slice(&self.scratch);
        self.drain()
    }

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
