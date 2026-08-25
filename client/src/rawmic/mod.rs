#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "windows")]
pub use self::windows::{Capture, Sink, SinkBuilder};

pub const fn supported() -> bool {
    cfg!(target_os = "windows")
}
