//! System-audio capture, for sharing the machine's sound alongside a screen.
//!
//! The webview cannot do this everywhere. `getDisplayMedia({audio:true})` gets
//! tab/system audio on Chromium, but WebKit — which is what wry embeds on macOS
//! — implements the video half only, so a macOS share is silent no matter what
//! constraints are asked for. Capturing natively sidesteps the engine entirely:
//! the audio becomes an ordinary LiveKit track published from Rust, on the same
//! path the microphone already uses.
//!
//! Backends are per-platform and only macOS exists so far. Everywhere else this
//! reports unsupported and the webview path stays in charge, which is fine
//! because that is where the webview path actually works.
//!
//! Frames are mono `f32` in [-1, 1] at 48 kHz — the format
//! `features::voice`'s publish path already speaks.

use tokio::sync::mpsc::UnboundedSender;

mod frames;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

/// A running capture. Dropping it stops the capture and releases the OS
/// resources behind it.
pub struct Capture {
    #[cfg(target_os = "macos")]
    _inner: macos::MacCapture,
    #[cfg(target_os = "windows")]
    _inner: self::windows::WinCapture,
}

/// Which picks native capture takes charge of.
///
/// Not a plain yes/no, because on Windows the answer depends on *what* the user
/// chose to share — and that is only knowable after the picker has closed. See
/// `features::screenshare` for where the decision is made.
///
/// Every platform constructs exactly one of these, so the other two always look
/// dead to the compiler on any given build.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)]
pub enum NativeScope {
    /// Native capture never applies; the webview keeps whatever audio it can get.
    Never,
    /// Native capture always applies. macOS: WebKit implements the video half of
    /// `getDisplayMedia` only, so there is nothing to take charge *from*.
    Always,
    /// Only a whole-screen pick. A single-window pick stays with the engine,
    /// whose `windowAudio: 'window'` is scoped to that one window — narrower
    /// than any loopback can be, since loopback is machine-wide by construction.
    /// Sending the whole machine's sound for a share the user scoped to one
    /// window would leak every other app that happens to be making noise.
    MonitorOnly,
}

/// Where native capture applies on this platform.
#[cfg(target_os = "macos")]
pub fn scope() -> NativeScope {
    NativeScope::Always
}

/// Windows: only if the OS is new enough for *process* loopback.
///
/// Claiming `MonitorOnly` on an older build would be a downgrade, not a
/// feature — every whole-screen share would lose the audio the webview was
/// perfectly able to provide, in exchange for a capture path that isn't there.
#[cfg(target_os = "windows")]
pub fn scope() -> NativeScope {
    /// Windows 10 build 20348: the first with `AUDIOCLIENT_ACTIVATION_PARAMS`.
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

/// The real OS build number.
///
/// `RtlGetVersion` rather than `GetVersionEx`: the latter lies about anything
/// past Windows 8 unless the executable carries a compatibility manifest, and
/// a false "too old" here silently costs a working feature.
#[cfg(target_os = "windows")]
fn os_build() -> u32 {
    use ::windows::Wdk::System::SystemServices::RtlGetVersion;
    use ::windows::Win32::System::SystemInformation::OSVERSIONINFOW;

    let mut info = OSVERSIONINFOW {
        dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32,
        ..Default::default()
    };
    // SAFETY: `info` is sized as the API requires.
    if unsafe { RtlGetVersion(&mut info) }.is_ok() {
        info.dwBuildNumber
    } else {
        0
    }
}

/// The build number as it should appear in a message to the user.
#[cfg(target_os = "windows")]
pub(crate) fn os_build_label() -> String {
    match os_build() {
        0 => "unknown build".into(),
        b => format!("build {b}"),
    }
}

/// Whether this build can capture system audio without help from the webview.
pub fn supported() -> bool {
    scope() != NativeScope::Never
}

/// Begin capturing system audio, pushing 10 ms mono frames to `tx`.
///
/// Errors are for the caller to report and carry on from: a share with no
/// sound is worth far more than no share at all.
///
/// `fatal` carries the reason a *running* capture died. Returning `Ok` here
/// only means the capture started; if it later breaks, the track stays
/// published and would simply go quiet, which is the one outcome a sharer has
/// no way to notice on their own. Backends that cannot detect such a failure
/// (macOS, where ScreenCaptureKit reports through a delegate we don't install)
/// just never send on it.
pub fn start(
    tx: UnboundedSender<Vec<f32>>,
    fatal: UnboundedSender<String>,
) -> Result<Capture, String> {
    #[cfg(target_os = "macos")]
    {
        let _ = fatal;
        Ok(Capture {
            _inner: macos::MacCapture::start(tx)?,
        })
    }
    #[cfg(target_os = "windows")]
    {
        Ok(Capture {
            _inner: self::windows::WinCapture::start(tx, fatal)?,
        })
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = (tx, fatal);
        Err("system audio capture isn't implemented on this platform yet".into())
    }
}
