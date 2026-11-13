//! What the gateway knows about the device sleeping and the app coming back.

use std::time::Duration;

/// `Instant` stops while Android is suspended, so a deadline measured with it
/// outlives a server that hung up on its own clock. This one keeps counting.
#[cfg(target_os = "android")]
pub(super) fn now() -> Duration {
    // SAFETY: a timespec is plain integers, and it outlives the call that fills it.
    let mut time: libc::timespec = unsafe { std::mem::zeroed() };
    if unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut time) } != 0 {
        return awake();
    }
    Duration::new(time.tv_sec as u64, time.tv_nsec as u32)
}

#[cfg(not(target_os = "android"))]
pub(super) fn now() -> Duration {
    awake()
}

fn awake() -> Duration {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed()
}

pub(super) type Clock = fn() -> Duration;

/// The app returning to the foreground. Only Android reports it.
pub(super) struct Resumes(Option<tokio::sync::watch::Receiver<u64>>);

impl Resumes {
    pub(super) fn subscribe() -> Self {
        #[cfg(target_os = "android")]
        {
            Self(Some(crate::android::resumes()))
        }
        #[cfg(not(target_os = "android"))]
        {
            Self(None)
        }
    }

    #[cfg(test)]
    pub(super) fn from(receiver: tokio::sync::watch::Receiver<u64>) -> Self {
        Self(Some(receiver))
    }

    pub(super) async fn next(&mut self) {
        if let Some(receiver) = &mut self.0
            && receiver.changed().await.is_ok()
        {
            return;
        }
        self.0 = None;
        std::future::pending().await
    }
}

/// Whether something started at `began` has gone `limit` without finishing,
/// counting the time the device slept through.
pub(super) fn overdue(clock: Clock, began: Duration, limit: Duration) -> bool {
    clock().saturating_sub(began) >= limit
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn a_resume_is_seen_once_and_a_closed_source_never_fires() {
        let (resume, receiver) = tokio::sync::watch::channel(0);
        let mut resumes = Resumes::from(receiver);
        resume.send_replace(1);
        resumes.next().await;
        let wait = Duration::from_secs(3600);
        assert!(tokio::time::timeout(wait, resumes.next()).await.is_err());
        // A dropped sender reports an error at once and for ever: it must not spin a `select!`.
        drop(resume);
        assert!(tokio::time::timeout(wait, resumes.next()).await.is_err());
    }

    #[test]
    fn overdue_counts_from_the_start_it_is_given() {
        fn at_thirty() -> Duration {
            Duration::from_secs(30)
        }
        let limit = Duration::from_secs(20);
        assert!(overdue(at_thirty, Duration::from_secs(10), limit));
        assert!(!overdue(at_thirty, Duration::from_secs(11), limit));
        assert!(!overdue(at_thirty, Duration::from_secs(40), limit));
    }
}
