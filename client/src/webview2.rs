use std::path::PathBuf;

const ENSURE_FLAG: &str = "--ensure-webview2";

const BOOTSTRAPPER: &str = "MicrosoftEdgeWebview2Setup.exe";

enum Outcome {
    AlreadyPresent(String),
    Installed,
}

pub fn gate() {
    let installer_mode = std::env::args().any(|arg| arg == ENSURE_FLAG);

    match ensure() {
        Ok(Outcome::AlreadyPresent(version)) => {
            if installer_mode {
                tracing::info!("WebView2 runtime {version} already present; nothing to do");
                std::process::exit(0);
            }
        }
        Ok(Outcome::Installed) => {
            if installer_mode {
                tracing::info!("WebView2 runtime installed");
                std::process::exit(0);
            }
        }
        Err(problem) => {
            tracing::error!("[webview2] {problem}");
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

    if installed_version().is_some() {
        return Ok(Outcome::Installed);
    }

    attempt?;
    Err(
        "The WebView2 installer finished but no runtime is registered. \
         Installing the runtime manually from Microsoft should fix this."
            .into(),
    )
}

fn installed_version() -> Option<String> {
    use webview2_com_sys::Microsoft::Web::WebView2::Win32::GetAvailableCoreWebView2BrowserVersionString;
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::core::{PCWSTR, PWSTR};

    let mut raw = PWSTR::null();
    unsafe {
        GetAvailableCoreWebView2BrowserVersionString(PCWSTR::null(), &mut raw).ok()?;
        if raw.is_null() {
            return None;
        }
        let version = raw.to_string().ok();
        CoTaskMemFree(Some(raw.as_ptr().cast()));
        version.filter(|v| !v.is_empty())
    }
}

fn bootstrapper_path() -> Option<PathBuf> {
    let path = std::env::current_exe().ok()?.parent()?.join(BOOTSTRAPPER);
    path.is_file().then_some(path)
}

fn run_bootstrapper(path: &PathBuf) -> Result<(), String> {
    let status = std::process::Command::new(path)
        .args(["/silent", "/install"])
        .status()
        .map_err(|e| {
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

    let code = status.code().unwrap_or(-1);
    Err(format!(
        "The WebView2 installer failed (exit code {code} / {:#010x}). It downloads the \
         runtime from Microsoft, so check the internet connection and try again.",
        code as u32
    ))
}

fn alert(text: &str) {
    use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    use windows::core::HSTRING;

    let body = HSTRING::from(text);
    let title = HSTRING::from("Discordia — WebView2 runtime required");
    unsafe {
        MessageBoxW(None, &body, &title, MB_OK | MB_ICONERROR);
    }
}
