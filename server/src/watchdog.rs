//! Names the step a socket loop is on, so a stall says where it stalled
//! instead of going quiet: a loop is one `select!`, and a step that does not
//! return holds every other branch with it.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// A step past this is reported when it ends.
pub const SLOW: Duration = Duration::from_millis(250);
/// A step past this is reported while it is still running, and again every
/// tick until it ends.
pub const STUCK: Duration = Duration::from_secs(2);

#[derive(Default)]
pub struct ArmWatch {
    current: Mutex<Option<(String, Instant)>>,
}

impl ArmWatch {
    pub fn begin(&self, arm: impl Into<String>) {
        *self.lock() = Some((arm.into(), Instant::now()));
    }

    /// Closes the step begun last. Called at the top of the loop, not at the
    /// end of a branch, because a `continue` inside an arm skips the end.
    pub fn finish(&self, who: &str) {
        if let Some((arm, since)) = self.lock().take() {
            let took = since.elapsed();
            if took > SLOW {
                tracing::warn!(who, arm, ms = took.as_millis() as u64, "slow loop step");
            } else {
                tracing::trace!(who, arm, us = took.as_micros() as u64, "loop step");
            }
        }
    }

    pub fn stuck_for(&self) -> Option<(String, Duration)> {
        self.lock()
            .as_ref()
            .map(|(arm, since)| (arm.clone(), since.elapsed()))
            .filter(|(_, for_)| *for_ > STUCK)
    }

    fn lock(&self) -> MutexGuard<'_, Option<(String, Instant)>> {
        self.current.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Barks every tick while a step is still running. Ends on its own once the
/// loop dropped its watch, so an early return does not leave it shouting.
pub fn watchdog(watch: Arc<ArmWatch>, who: String) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(STUCK);
        loop {
            tick.tick().await;
            if Arc::strong_count(&watch) == 1 {
                break;
            }
            if let Some((arm, for_)) = watch.stuck_for() {
                tracing::warn!(
                    %who,
                    arm,
                    secs = for_.as_secs(),
                    "loop step still running — this socket handles nothing else meanwhile"
                );
            }
        }
    })
}

/// The `op` of one of our own frames, read off the front of the JSON without
/// parsing the payload: the tag is always serialised first.
pub fn op_of(json: &str) -> &str {
    json.strip_prefix("{\"op\":\"")
        .and_then(|rest| rest.split('"').next())
        .unwrap_or("?")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_is_read_off_the_front() {
        assert_eq!(
            op_of(r#"{"op":"send_message","d":{"x":1}}"#),
            "send_message"
        );
        assert_eq!(op_of(r#"{"op":"leave_voice"}"#), "leave_voice");
        assert_eq!(op_of(r#"{"d":{},"op":"x"}"#), "?");
        assert_eq!(op_of("garbage"), "?");
    }

    #[test]
    fn a_finished_step_is_not_stuck() {
        let w = ArmWatch::default();
        assert!(w.stuck_for().is_none());
        w.begin("recv x");
        assert!(w.stuck_for().is_none(), "brand new step is not stuck yet");
        w.finish("test");
        assert!(w.stuck_for().is_none());
    }
}
