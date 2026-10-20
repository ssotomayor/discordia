const RESERVE_MS: u32 = 80;

pub(super) struct Recovery {
    reserve: usize,
    started: bool,
    recovering: bool,
    waited: usize,
    idle_samples: usize,
    pub held_samples: u64,
}

impl Recovery {
    pub fn new(rate: u32) -> Self {
        Self {
            reserve: (u64::from(rate) * u64::from(RESERVE_MS) / 1000).max(1) as usize,
            started: false,
            recovering: false,
            waited: 0,
            idle_samples: 0,
            held_samples: 0,
        }
    }

    pub fn set_rate(&mut self, rate: u32) {
        let reserve = (u64::from(rate) * u64::from(RESERVE_MS) / 1000).max(1) as usize;
        if self.reserve != reserve {
            self.reserve = reserve;
            self.recovering = false;
            self.waited = 0;
            self.started = false;
            self.idle_samples = 0;
        }
    }

    pub fn ready(&mut self, queued: usize) -> bool {
        if queued == 0 {
            self.idle_samples = self.idle_samples.saturating_add(1);
            if self.idle_samples >= self.reserve {
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
}
