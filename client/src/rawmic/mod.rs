//! Microphone capture that bypasses the operating system's own input
//! processing.
//!
//! Windows runs a capture endpoint through the driver's effects chain — the
//! vendor's noise suppression and gain control (Realtek, Nahimic, Dolby) plus
//! Windows 11's own voice isolation — before any application sees a sample.
//! Those effects are written for a client that has none of its own. Ours has
//! three (libwebrtc's echo canceller and AGC, DeepFilterNet), and two
//! suppressors in series is how a voice ends up underwater with its quiet
//! consonants chewed off — and how the transmit gate ends up judging a signal
//! something else already decided was noise. WASAPI's *raw* mode is the
//! documented way out: the same shared-mode stream, with the endpoint's
//! processing skipped.
//!
//! cpal cannot ask for it. Raw mode is set through
//! `IAudioClient2::SetClientProperties`, which has to happen before
//! `Initialize`, and cpal owns that call — so this module opens the device
//! itself. That is deliberately the *only* difference: samples come out
//! interleaved in the device's own format and go straight into the same
//! downmix/resample/frame path `features::voice` already runs for cpal.
//!
//! Nowhere else has anything to bypass. macOS applies its microphone modes
//! (Voice Isolation, Wide Spectrum) only to clients of the voice-processing
//! audio unit, and cpal opens a plain HAL input unit, so a macOS capture is
//! already raw. `supported()` says so and the setting is hidden where it is
//! false, rather than offered as a switch that cannot change anything.

#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "windows")]
pub use self::windows::{Capture, Sink, SinkBuilder};

/// Whether this platform puts processing on the microphone that we can turn
/// off.
pub const fn supported() -> bool {
    cfg!(target_os = "windows")
}
