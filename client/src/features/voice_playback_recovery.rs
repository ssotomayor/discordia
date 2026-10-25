/// What a track that ran dry mid-stream rebuilds before playing again. The
/// drift controller keeps 40 ms in steady state; this is only the floor after
/// a starvation, and holding more here was itself a stutter.
pub(super) const RESERVE_MS: u32 = 25;
/// Silence this long means the utterance ended; the next one starts at once.
pub(super) const IDLE_MS: u32 = 80;
const RESUME_FADE_MS: u32 = 5;

pub(super) struct Recovery {
    reserve: usize,
    idle: usize,
    fade_len: usize,
    fade: usize,
    started: bool,
    recovering: bool,
    waited: usize,
    idle_samples: usize,
    pub held_samples: u64,
}

fn samples(rate: u32, ms: u32) -> usize {
    (u64::from(rate) * u64::from(ms) / 1000).max(1) as usize
}

impl Recovery {
    pub fn new(rate: u32) -> Self {
        let fade_len = samples(rate, RESUME_FADE_MS);
        Self {
            reserve: samples(rate, RESERVE_MS),
            idle: samples(rate, IDLE_MS),
            fade_len,
            fade: fade_len,
            started: false,
            recovering: false,
            waited: 0,
            idle_samples: 0,
            held_samples: 0,
        }
    }

    pub fn set_rate(&mut self, rate: u32) {
        let reserve = samples(rate, RESERVE_MS);
        if self.reserve != reserve {
            self.reserve = reserve;
            self.idle = samples(rate, IDLE_MS);
            self.fade_len = samples(rate, RESUME_FADE_MS);
            self.fade = self.fade_len;
            self.recovering = false;
            self.waited = 0;
            self.started = false;
            self.idle_samples = 0;
        }
    }

    pub fn ready(&mut self, queued: usize) -> bool {
        if queued == 0 {
            self.idle_samples = self.idle_samples.saturating_add(1);
            if self.idle_samples >= self.idle {
                self.started = false;
                self.recovering = false;
                self.waited = 0;
                return true;
            }
        } else {
            self.idle_samples = 0;
        }
        if queued == 0 && self.started {
            self.recovering = true;
        }
        if self.recovering {
            // Releasing each late hop immediately leaves no reserve for the next delivery stall.
            if queued >= self.reserve || self.waited >= self.reserve {
                self.recovering = false;
                self.waited = 0;
                self.fade = 0;
            } else {
                if queued > 0 {
                    self.waited += 1;
                    self.held_samples += 1;
                }
                return false;
            }
        }
        self.started |= queued > 0;
        true
    }

    /// Ramps the first samples after a recovery in from silence; unity otherwise.
    pub fn resume_gain(&mut self) -> f32 {
        if self.fade >= self.fade_len {
            return 1.0;
        }
        self.fade += 1;
        self.fade as f32 / self.fade_len as f32
    }
}
