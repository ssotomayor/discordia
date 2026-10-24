use tokio::sync::mpsc::UnboundedSender;

#[cfg(target_os = "android")]
mod android;
#[cfg(any(target_os = "android", test))]
mod pcm;

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod frames;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

pub struct Capture {
    #[cfg(target_os = "android")]
    _inner: android::AndroidCapture,
    #[cfg(target_os = "macos")]
    _inner: macos::MacCapture,
    #[cfg(target_os = "windows")]
    _inner: self::windows::WinCapture,
}

#[cfg(target_os = "android")]
impl Capture {
    pub async fn is_active(&self) -> Result<bool, String> {
        self._inner.is_active().await
    }
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

#[cfg(target_os = "android")]
pub fn scope() -> NativeScope {
    if crate::android::playback_capture_supported() {
        NativeScope::Always
    } else {
        NativeScope::Never
    }
}

#[cfg(not(any(target_os = "android", target_os = "macos", target_os = "windows")))]
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

pub fn supported_for(target: Option<crate::sysvideo::Target>) -> bool {
    #[cfg(target_os = "windows")]
    if let Some(crate::sysvideo::Target::WindowsWindow(handle)) = target {
        let window =
            windows_capture::window::Window::from_raw_hwnd(handle as *mut std::ffi::c_void);
        return supported()
            && window.is_valid()
            && window
                .process_id()
                .is_ok_and(|pid| pid != std::process::id());
    }
    let _ = target;
    supported()
}

#[cfg(not(target_os = "android"))]
pub fn start(
    tx: crate::audio_queue::AudioSender<f32>,
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
        let target = target.ok_or_else(|| "no screen-share target was selected".to_string())?;
        Ok(Capture {
            _inner: self::windows::WinCapture::start(target, tx, fatal)?,
        })
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = (tx, fatal, target);
        Err("system audio capture isn't implemented on this platform yet".into())
    }
}

#[cfg(target_os = "android")]
pub async fn start_android(
    tx: crate::audio_queue::AudioSender<f32>,
    fatal: UnboundedSender<String>,
    target: Option<crate::sysvideo::Target>,
) -> Result<Capture, String> {
    if target != Some(crate::sysvideo::Target::AndroidScreen) {
        return Err("Android audio requires an active screen share".into());
    }
    Ok(Capture {
        _inner: android::AndroidCapture::start(tx, fatal).await?,
    })
}

pub fn captures_application(target: Option<crate::sysvideo::Target>) -> bool {
    use crate::sysvideo::Target;
    match target {
        Some(Target::Window(_) | Target::Application(_)) => true,
        #[cfg(target_os = "windows")]
        Some(Target::WindowsWindow(_)) => true,
        _ => false,
    }
}
