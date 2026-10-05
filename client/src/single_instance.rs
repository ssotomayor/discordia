//! One Discordia per Windows session: a named mutex marks the primary, and a
//! second launch restores its window. The mutex is abandoned at process exit,
//! which is how the updater's replacement takes over instead of racing it.

use std::time::Duration;

use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, WAIT_ABANDONED, WAIT_OBJECT_0,
};
use windows::Win32::System::Threading::{CreateMutexW, WaitForSingleObject};
use windows::Win32::UI::WindowsAndMessaging::{
    ASFW_ANY, AllowSetForegroundWindow, FindWindowW, SW_RESTORE, SetForegroundWindow, ShowWindow,
};
use windows::core::PCWSTR;

use crate::{MAIN_WINDOW_TITLE, RESTART_FLAG};

const MUTEX_NAME: &str = "Local\\com.discordia.app";

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
        // Title only: the Social and Streams windows carry a suffix, so
        // "Discordia" alone names the main window without a custom class.
        // SAFETY: the name is NUL-terminated and outlives the call.
        if let Ok(hwnd) = unsafe { FindWindowW(PCWSTR::null(), PCWSTR(title.as_ptr())) }
            && !hwnd.is_invalid()
        {
            // SAFETY: hwnd came from FindWindowW; a denied foreground is
            // accepted because restoring the window is the point.
            unsafe {
                let _ = AllowSetForegroundWindow(ASFW_ANY);
                let _ = ShowWindow(hwnd, SW_RESTORE);
                let _ = SetForegroundWindow(hwnd);
            }
            return true;
        }
        std::thread::sleep(FOCUS_INTERVAL);
    }
    false
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
}
