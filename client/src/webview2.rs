//! The WebView2 runtime gate (Windows only).
//!
//! wry draws the entire UI with Microsoft's Edge WebView2 runtime. Windows 11
//! and current Windows 10 ship it, but on a machine without it the window never
//! paints — which reads as a broken app rather than a missing dependency. This
//! settles the question before any window exists.
//!
//! **One implementation serves both distributions.** The portable zip calls
//! `gate()` on normal startup; the NSIS installer runs the very same binary as
//! `Discordia.exe --ensure-webview2` (see `client/nsis/webview2.nsh`) instead of
//! reimplementing the check in NSIS script, where it could drift.
//!
//! Both layouts keep the Evergreen bootstrapper beside the executable, so
//! `bootstrapper_path()` is the same lookup in either case.

use std::path::PathBuf;

/// Flag the installer passes to ask for the check without starting the UI.
const ENSURE_FLAG: &str = "--ensure-webview2";

/// The Evergreen bootstrapper, shipped next to the executable by both the
/// portable zip and the installer.
const BOOTSTRAPPER: &str = "MicrosoftEdgeWebview2Setup.exe";

enum Outcome {
    /// A runtime was already registered; nothing was run or downloaded.
    AlreadyPresent(String),
    /// The bootstrapper ran and a runtime is now registered.
    Installed,
}

/// Settle WebView2 before anything can try to use it.
///
/// Call this as the FIRST statement of `main`. It either returns — meaning a
/// runtime is available — or exits the process. It never returns having failed,
/// because the only thing left to do afterwards is create a window that cannot
/// work.
pub fn gate() {
    let installer_mode = std::env::args().any(|arg| arg == ENSURE_FLAG);

    match ensure() {
        Ok(Outcome::AlreadyPresent(version)) => {
            if installer_mode {
                println!("WebView2 runtime {version} already present; nothing to do");
                std::process::exit(0);
            }
        }
        Ok(Outcome::Installed) => {
            if installer_mode {
                println!("WebView2 runtime installed");
                std::process::exit(0);
            }
        }
        Err(problem) => {
            // Installer mode has no UI to alert; a normal launch has no
            // working window to paint in. Exit immediately in both cases.
            eprintln!("[webview2] {problem}");
            if !installer_mode {
                alert(&format!(
                    "Discordia needs the Microsoft Edge WebView2 runtime, and it \
                     could not be installed.\n\n{problem}"
                ));
            }
            std::process::exit(1);
        }
    }
}

/// Query first, install only if we must.
fn ensure() -> Result<Outcome, String> {
    if let Some(version) = installed_version() {
        return Ok(Outcome::AlreadyPresent(version));
    }

    let bootstrapper = bootstrapper_path().ok_or_else(|| {
        format!(
            "No WebView2 runtime is installed, and {BOOTSTRAPPER} is not next to the \
             application. Install the runtime from Microsoft, then start Discordia again."
        )
    })?;

    let attempt = run_bootstrapper(&bootstrapper);

    // Registry is the authority, not exit code: bootstrapper exits 0x80040828
    // on healthy installs, so non-zero does not mean failure.
    if installed_version().is_some() {
        return Ok(Outcome::Installed);
    }

    // No runtime, so the exit code is now the only thing that can explain why.
    attempt?;
    Err(
        "The WebView2 installer finished but no runtime is registered. \
         Installing the runtime manually from Microsoft should fix this."
            .into(),
    )
}

/// The version of the registered runtime, if there is one.
///
/// This is Microsoft's own detection entry point, which is why it is used in
/// preference to reading the registry directly: it already accounts for
/// per-machine and per-user installs, the Edge Dev/Beta channels it can fall
/// back to, and the `WEBVIEW2_BROWSER_EXECUTABLE_FOLDER` override. It is a pure
/// query — no WebView2 is created or initialised by calling it.
fn installed_version() -> Option<String> {
    use webview2_com_sys::Microsoft::Web::WebView2::Win32::GetAvailableCoreWebView2BrowserVersionString;
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::core::{PCWSTR, PWSTR};

    let mut raw = PWSTR::null();
    // SAFETY: `raw` is a live out-param. On success the callee hands back a
    // CoTaskMemAlloc'd string, which is released below; on failure it is left
    // null and nothing is freed.
    unsafe {
        GetAvailableCoreWebView2BrowserVersionString(PCWSTR::null(), &mut raw).ok()?;
        if raw.is_null() {
            return None;
        }
        let version = raw.to_string().ok();
        CoTaskMemFree(Some(raw.as_ptr().cast()));
        // An empty string would mean "registered, but no version" — treat that
        // as absent rather than pretending we know something.
        version.filter(|v| !v.is_empty())
    }
}

/// The bootstrapper shipped beside the executable, in either layout.
fn bootstrapper_path() -> Option<PathBuf> {
    let path = std::env::current_exe().ok()?.parent()?.join(BOOTSTRAPPER);
    path.is_file().then_some(path)
}

fn run_bootstrapper(path: &PathBuf) -> Result<(), String> {
    let status = std::process::Command::new(path)
        .args(["/silent", "/install"])
        .status()
        .map_err(|e| {
            // 1223 is ERROR_CANCELLED — the elevation prompt was dismissed.
            // Worth separating, because it is a decision rather than a fault and
            // the advice differs.
            if e.raw_os_error() == Some(1223) {
                "The WebView2 installation was cancelled. Start Discordia again to retry."
                    .to_string()
            } else {
                format!("Could not start the WebView2 installer: {e}")
            }
        })?;

    if status.success() {
        return Ok(());
    }

    // Hex included because HRESULTs are searchable in hex, not decimal. No
    // translation: bootstrapper codes are unstable, and a wrong guess reads
    // worse than a number.
    let code = status.code().unwrap_or(-1);
    Err(format!(
        "The WebView2 installer failed (exit code {code} / {:#010x}). It downloads the \
         runtime from Microsoft, so check the internet connection and try again.",
        code as u32
    ))
}

/// A native message box: at this point there is no webview to render UI with,
/// which is the whole problem.
fn alert(text: &str) {
    use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    use windows::core::HSTRING;

    let body = HSTRING::from(text);
    let title = HSTRING::from("Discordia — WebView2 runtime required");
    // SAFETY: both strings outlive the call, and a null owner window is valid
    // for a process that has not created one.
    unsafe {
        MessageBoxW(None, &body, &title, MB_OK | MB_ICONERROR);
    }
}
