//! The mutex survives until process exit so updater replacements wait through teardown.
//! Restoration runs in the primary because native visibility alone leaves its WebView hidden.

use std::time::Duration;

use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, WAIT_ABANDONED, WAIT_OBJECT_0,
};
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, EVENT_MODIFY_STATE, INFINITE, OpenEventW, SetEvent,
    WaitForSingleObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, FindWindowW, GetWindowThreadProcessId,
};
use windows::core::PCWSTR;

use crate::{MAIN_WINDOW_TITLE, RESTART_FLAG};

const MUTEX_NAME: &str = "Local\\com.discordia.app";
const RESTORE_EVENT_NAME: &str = "Local\\com.discordia.app.restore";

/// How long an updater-launched replacement waits for the mutex the old process
/// still holds through its teardown. Bounded so a hung primary cannot wedge it.
const RESTART_WAIT_MS: u32 = 30_000;

/// The primary may not have created its window yet when a second launch lands.
const FOCUS_ATTEMPTS: u32 = 30;
const FOCUS_INTERVAL: Duration = Duration::from_millis(100);

enum Claim {
    Primary,
    AlreadyRunning,
}

/// True when this process should keep running. When false, another instance is
/// alive and has already been asked to show itself; the caller must exit.
pub fn acquire() -> bool {
    match try_claim_named(MUTEX_NAME, restart_requested()) {
        Claim::Primary => true,
        Claim::AlreadyRunning => {
            focus_primary_window();
            false
        }
    }
}

/// Set by the updater on the process it spawns to replace this one.
pub fn restart_requested() -> bool {
    std::env::args().any(|arg| arg == RESTART_FLAG)
}

fn try_claim_named(name: &str, restart: bool) -> Claim {
    let name = wide(name);
    // SAFETY: the name is NUL-terminated and outlives the call.
    let handle = match unsafe { CreateMutexW(None, true, PCWSTR(name.as_ptr())) } {
        Ok(handle) => handle,
        Err(error) => {
            tracing::warn!(%error, "could not create the single-instance mutex; continuing");
            return Claim::Primary;
        }
    };

    // SAFETY: read immediately after CreateMutexW, the only moment the
    // ERROR_ALREADY_EXISTS signal is meaningful.
    if unsafe { GetLastError() } != ERROR_ALREADY_EXISTS {
        return Claim::Primary;
    }

    if restart {
        // SAFETY: handle is a live mutex handle.
        let wait = unsafe { WaitForSingleObject(handle, RESTART_WAIT_MS) };
        if wait == WAIT_OBJECT_0 || wait == WAIT_ABANDONED {
            return Claim::Primary;
        }
    }

    // SAFETY: this process does not own the mutex and keeps no other reference.
    let _ = unsafe { CloseHandle(handle) };
    Claim::AlreadyRunning
}

fn focus_primary_window() -> bool {
    let title = wide(MAIN_WINDOW_TITLE);
    for _ in 0..FOCUS_ATTEMPTS {
        // SAFETY: the name is NUL-terminated and outlives the call.
        if let Ok(hwnd) = unsafe { FindWindowW(PCWSTR::null(), PCWSTR(title.as_ptr())) }
            && !hwnd.is_invalid()
        {
            let mut pid = 0;
            // SAFETY: hwnd is live and pid is writable for the duration of the call.
            unsafe {
                GetWindowThreadProcessId(hwnd, Some(&mut pid));
                if pid != 0
                    && let Err(error) = AllowSetForegroundWindow(pid)
                {
                    tracing::debug!(%error, "Windows denied foreground permission for the primary");
                }
            }
        }
        if signal_restore_named(RESTORE_EVENT_NAME) {
            return true;
        }
        std::thread::sleep(FOCUS_INTERVAL);
    }
    tracing::warn!("The running instance did not expose its restore bridge");
    false
}

pub fn init_restore_bridge(sender: tokio::sync::mpsc::UnboundedSender<()>) {
    if let Err(error) = std::thread::Builder::new()
        .name("instance-restore".into())
        .spawn(move || listen_for_restore(RESTORE_EVENT_NAME, sender))
    {
        tracing::warn!(%error, "Could not start the instance restore bridge");
    }
}

fn listen_for_restore(name: &str, sender: tokio::sync::mpsc::UnboundedSender<()>) {
    let name = wide(name);
    // SAFETY: the auto-reset event name is NUL-terminated and lives through the call.
    let event = match unsafe { CreateEventW(None, false, false, PCWSTR(name.as_ptr())) } {
        Ok(event) => event,
        Err(error) => {
            tracing::warn!(%error, "Could not create the instance restore event");
            return;
        }
    };
    loop {
        // SAFETY: this thread owns the event handle until after the loop.
        if unsafe { WaitForSingleObject(event, INFINITE) } != WAIT_OBJECT_0 {
            tracing::warn!("The instance restore event wait failed");
            break;
        }
        // Showing just the HWND leaves Dioxus's hidden WebView2 controller invisible.
        if sender.send(()).is_err() {
            break;
        }
    }
    // SAFETY: no pending wait or sender uses this thread's event handle.
    if let Err(error) = unsafe { CloseHandle(event) } {
        tracing::warn!(%error, "Could not close the instance restore event");
    }
}

fn signal_restore_named(name: &str) -> bool {
    let name = wide(name);
    // SAFETY: the name is NUL-terminated and lives through the call.
    let Ok(event) = (unsafe { OpenEventW(EVENT_MODIFY_STATE, false, PCWSTR(name.as_ptr())) })
    else {
        return false;
    };
    // SAFETY: this process owns the opened event handle until CloseHandle below.
    let signaled = unsafe { SetEvent(event) };
    // SAFETY: SetEvent has completed and this handle will not be used again.
    if let Err(error) = unsafe { CloseHandle(event) } {
        tracing::warn!(%error, "Could not close the secondary restore event handle");
    }
    if let Err(error) = signaled {
        tracing::warn!(%error, "Could not notify the primary to restore");
        return false;
    }
    true
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_named_mutex_is_claimed_once() {
        let name = format!("Local\\discordia-test-{}", std::process::id());
        assert!(matches!(try_claim_named(&name, false), Claim::Primary));
        assert!(matches!(
            try_claim_named(&name, false),
            Claim::AlreadyRunning
        ));
    }

    #[test]
    fn the_replaced_flag_is_recognised() {
        assert_eq!(RESTART_FLAG, "--replaced");
    }

    #[test]
    fn restore_requests_reach_the_primary_repeatedly() {
        let name = format!("Local\\discordia-restore-test-{}", std::process::id());
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let worker_name = name.clone();
        let worker = std::thread::spawn(move || listen_for_restore(&worker_name, sender));
        for _ in 0..3 {
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            while !signal_restore_named(&name) {
                assert!(
                    std::time::Instant::now() < deadline,
                    "restore bridge did not start"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            while receiver.try_recv().is_err() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "restore request was lost"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(matches!(
                receiver.try_recv(),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty)
            ));
        }
        drop(receiver);
        assert!(signal_restore_named(&name));
        worker.join().unwrap();
        assert!(!signal_restore_named(&name));
    }
}
