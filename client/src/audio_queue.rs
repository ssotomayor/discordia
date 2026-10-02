use std::time::{Duration, Instant};

use tokio::sync::mpsc::{Receiver, Sender, error::TrySendError};

pub const SAMPLES: usize = 480;
const CAPACITY: usize = 8;
const MAX_AGE: Duration = Duration::from_millis(100);

pub struct Frame<T> {
    pub samples: [T; SAMPLES],
    pub captured: Instant,
}

impl<T> Frame<T> {
    pub fn new(samples: [T; SAMPLES]) -> Self {
        Self {
            samples,
            captured: Instant::now(),
        }
    }

    pub fn is_fresh(&self) -> bool {
        self.captured.elapsed() <= MAX_AGE
    }
}

pub type AudioSender<T> = Sender<Frame<T>>;
pub type AudioReceiver<T> = Receiver<Frame<T>>;

pub fn channel<T>() -> (AudioSender<T>, AudioReceiver<T>) {
    tokio::sync::mpsc::channel(CAPACITY)
}

// A stalled publisher must not block the device callback or grow a delayed audio backlog.
pub fn offer<T>(tx: &AudioSender<T>, frame: Frame<T>) -> Result<bool, ()> {
    match tx.try_send(frame) {
        Ok(()) => Ok(true),
        Err(TrySendError::Full(_)) => Ok(false),
        Err(TrySendError::Closed(_)) => Err(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overload_is_bounded_and_recovers_without_closing_capture() {
        let (tx, mut rx) = channel();
        for i in 0..CAPACITY {
            assert_eq!(offer(&tx, Frame::new([i; SAMPLES])), Ok(true));
        }
        for _ in 0..1000 {
            assert_eq!(offer(&tx, Frame::new([99; SAMPLES])), Ok(false));
        }
        assert_eq!(rx.len(), CAPACITY);
        for i in 0..CAPACITY {
            assert_eq!(rx.try_recv().unwrap().samples, [i; SAMPLES]);
        }
        assert_eq!(offer(&tx, Frame::new([42; SAMPLES])), Ok(true));
        assert_eq!(rx.try_recv().unwrap().samples, [42; SAMPLES]);
        drop(rx);
        assert_eq!(offer(&tx, Frame::new([0; SAMPLES])), Err(()));
    }

    #[test]
    fn freshness_survives_processing_stages() {
        let input = Frame {
            samples: [0.0; SAMPLES],
            captured: Instant::now() - Duration::from_secs(1),
        };
        let output = Frame {
            samples: [0_i16; SAMPLES],
            captured: input.captured,
        };
        assert!(!input.is_fresh());
        assert!(!output.is_fresh());
        assert!(Frame::new([0.0; SAMPLES]).is_fresh());
    }
}
