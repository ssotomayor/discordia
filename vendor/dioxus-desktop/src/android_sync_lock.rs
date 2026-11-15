//! This is a hack to get around the fact that wry is currently not thread safe on android
//!
//! We want to acquire this mutex before doing anything with the virtualdom directly

#[cfg(target_os = "android")]
pub use watched::android_runtime_lock;

#[cfg(target_os = "android")]
mod watched {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Mutex, MutexGuard, Once};
    use std::time::{Duration, Instant};

    const STALL: Duration = Duration::from_secs(5);
    const REPEAT: Duration = Duration::from_secs(30);

    static RUNTIME_LOCK: Mutex<()> = Mutex::new(());
    static HOLDER: Mutex<Option<Holder>> = Mutex::new(None);
    static WAITING: AtomicUsize = AtomicUsize::new(0);
    static WATCHDOG: Once = Once::new();

    struct Holder {
        site: &'static str,
        thread: std::thread::Thread,
        since: Instant,
        next_report: Instant,
        reported: bool,
    }

    pub struct RuntimeGuard {
        _guard: MutexGuard<'static, ()>,
    }

    impl Drop for RuntimeGuard {
        fn drop(&mut self) {
            let holder = HOLDER.lock().unwrap_or_else(|e| e.into_inner()).take();
            if let Some(holder) = holder.filter(|h| h.reported) {
                tracing::warn!(
                    site = holder.site,
                    held_ms = holder.since.elapsed().as_millis() as u64,
                    "Android UI lock released after a stall"
                );
            }
        }
    }

    pub fn android_runtime_lock(site: &'static str) -> RuntimeGuard {
        WATCHDOG.call_once(spawn_watchdog);
        WAITING.fetch_add(1, Ordering::Relaxed);
        let guard = RUNTIME_LOCK.lock().unwrap();
        WAITING.fetch_sub(1, Ordering::Relaxed);
        let since = Instant::now();
        *HOLDER.lock().unwrap_or_else(|e| e.into_inner()) = Some(Holder {
            site,
            thread: std::thread::current(),
            since,
            next_report: since + STALL,
            reported: false,
        });
        RuntimeGuard { _guard: guard }
    }

    // Taps reach Rust as synchronous requests that wait for this lock, so a stuck holder
    // freezes the page without an ANR: the Java UI thread never blocks.
    fn spawn_watchdog() {
        let spawned = std::thread::Builder::new()
            .name("dx-ui-lock-watch".into())
            .spawn(|| loop {
                std::thread::sleep(Duration::from_secs(1));
                let mut holder = HOLDER.lock().unwrap_or_else(|e| e.into_inner());
                let Some(holder) = holder.as_mut() else {
                    continue;
                };
                let now = Instant::now();
                if now < holder.next_report {
                    continue;
                }
                holder.reported = true;
                holder.next_report = now + REPEAT;
                tracing::warn!(
                    site = holder.site,
                    thread = holder.thread.name().unwrap_or("unnamed"),
                    thread_id = ?holder.thread.id(),
                    held_ms = holder.since.elapsed().as_millis() as u64,
                    waiting = WAITING.load(Ordering::Relaxed),
                    "Android UI lock held too long: taps and rendering wait for it"
                );
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "Could not watch the Android UI lock");
        }
    }
}
