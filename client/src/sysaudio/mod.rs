use tokio::sync::mpsc::UnboundedSender;

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod frames;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

pub struct Capture {
    #[cfg(target_os = "macos")]
    _inner: macos::MacCapture,
    #[cfg(target_os = "windows")]
    _inner: self::windows::WinCapture,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)]
pub enum NativeScope {
    Never,
    Always,
    MonitorOnly,
}

#[cfg(target_os = "macos")]
pub fn scope() -> NativeScope {
    NativeScope::Always
}

#[cfg(target_os = "windows")]
pub fn scope() -> NativeScope {
    const MIN_BUILD: u32 = 20348;
    static SCOPE: std::sync::OnceLock<NativeScope> = std::sync::OnceLock::new();
    *SCOPE.get_or_init(|| {
        if os_build() >= MIN_BUILD {
            NativeScope::MonitorOnly
        } else {
            NativeScope::Never
        }
    })
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn scope() -> NativeScope {
    NativeScope::Never
}

#[cfg(target_os = "windows")]
fn os_build() -> u32 {
    use ::windows::Wdk::System::SystemServices::RtlGetVersion;
    use ::windows::Win32::System::SystemInformation::OSVERSIONINFOW;

    let mut info = OSVERSIONINFOW {
        dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32,
        ..Default::default()
    };
    if unsafe { RtlGetVersion(&mut info) }.is_ok() {
        info.dwBuildNumber
    } else {
        0
    }
}

#[cfg(target_os = "windows")]
pub(crate) fn os_build_label() -> String {
    match os_build() {
        0 => "unknown build".into(),
        b => format!("build {b}"),
    }
}

pub fn supported() -> bool {
    scope() != NativeScope::Never
}

pub fn start(
    tx: UnboundedSender<Vec<f32>>,
    fatal: UnboundedSender<String>,
    target: Option<crate::sysvideo::Target>,
) -> Result<Capture, String> {
    #[cfg(target_os = "macos")]
    {
        let target = target.ok_or_else(|| "no screen-share target was selected".to_string())?;
        Ok(Capture {
            _inner: macos::MacCapture::start(target, tx, fatal)?,
        })
    }
    #[cfg(target_os = "windows")]
    {
        let _ = target;
        Ok(Capture {
            _inner: self::windows::WinCapture::start(tx, fatal)?,
        })
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = (tx, fatal, target);
        Err("system audio capture isn't implemented on this platform yet".into())
    }
}
