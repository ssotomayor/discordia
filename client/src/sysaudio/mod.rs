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

#[cfg(target_os = "macos")]
mod macos;

/// A running capture. Dropping it stops the capture and releases the OS
/// resources behind it.
pub struct Capture {
    #[cfg(target_os = "macos")]
    _inner: macos::MacCapture,
}

/// Whether this build can capture system audio without help from the webview.
///
/// The share flow asks first: when this is true it requests video only from
/// `getDisplayMedia`, so the machine's audio is captured once, here, rather
/// than twice — once natively and once by the engine — and heard doubled.
pub fn supported() -> bool {
    cfg!(target_os = "macos")
}

/// Begin capturing system audio, pushing 10 ms mono frames to `tx`.
///
/// Errors are for the caller to report and carry on from: a share with no
/// sound is worth far more than no share at all.
pub fn start(tx: UnboundedSender<Vec<f32>>) -> Result<Capture, String> {
    #[cfg(target_os = "macos")]
    {
        Ok(Capture {
            _inner: macos::MacCapture::start(tx)?,
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = tx;
        Err("system audio capture isn't implemented on this platform yet".into())
    }
}
