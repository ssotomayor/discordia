#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
pub struct Frame {
    buffer: objc2_core_foundation::CFRetained<objc2_core_video::CVPixelBuffer>,
}

#[cfg(target_os = "macos")]
impl Frame {
    #[allow(clippy::wrong_self_convention)]
    pub fn into_consumable_pixel_buffer(&self) -> *mut std::ffi::c_void {
        objc2_core_foundation::CFRetained::into_raw(self.buffer.clone())
            .as_ptr()
            .cast()
    }
}

#[cfg(target_os = "macos")]
pub type FrameSink = Box<dyn Fn(Frame) + Send + Sync>;

#[cfg(target_os = "macos")]
pub struct Capture {
    _inner: macos::MacVideoCapture,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub max_bitrate: u64,
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Display(u32),
    Window(u32),
    Application(i32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    pub target: Target,
    pub title: String,
    pub app: Option<String>,
    pub width: u32,
    pub height: u32,
}

#[cfg(target_os = "macos")]
pub fn sources() -> Result<Vec<Source>, String> {
    macos::sources()
}

#[cfg(target_os = "macos")]
pub(crate) fn content_filter(
    target: Target,
) -> Result<objc2::rc::Retained<objc2_screen_capture_kit::SCContentFilter>, String> {
    macos::content_filter(target)
}

#[cfg(not(target_os = "macos"))]
pub fn sources() -> Result<Vec<Source>, String> {
    Err("native screen capture isn't implemented on this platform".into())
}

pub fn supported() -> bool {
    cfg!(target_os = "macos")
}

pub(crate) static FRAMES_CAPTURED: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

pub fn frames_captured() -> u64 {
    FRAMES_CAPTURED.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(target_os = "macos")]
pub fn start(
    target: Target,
    settings: Settings,
    sink: FrameSink,
    fatal: tokio::sync::mpsc::UnboundedSender<String>,
) -> Result<Capture, String> {
    FRAMES_CAPTURED.store(0, std::sync::atomic::Ordering::Relaxed);
    Ok(Capture {
        _inner: macos::MacVideoCapture::start(target, settings, sink, fatal)?,
    })
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    #[ignore = "needs Screen Recording permission and a display"]
    #[tokio::test(flavor = "multi_thread")]
    async fn frames_survive_the_encoder_handoff() {
        use livekit::webrtc::video_frame::native::NativeBuffer;
        use livekit::webrtc::video_frame::{VideoFrame, VideoRotation};
        use livekit::webrtc::video_source::VideoResolution;
        use livekit::webrtc::video_source::native::NativeVideoSource;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let target = match super::sources() {
            Ok(v) => v.into_iter().next().expect("a display").target,
            Err(e) => {
                eprintln!("SKIP (no screen recording permission?): {e}");
                return;
            }
        };
        let source = NativeVideoSource::new(
            VideoResolution {
                width: 1280,
                height: 720,
            },
            true,
        );
        let count = Arc::new(AtomicUsize::new(0));
        let (fatal_tx, _fatal_rx) = tokio::sync::mpsc::unbounded_channel();
        let c = count.clone();
        let cap = super::start(
            target,
            super::Settings {
                width: 1280,
                height: 720,
                fps: 30,
                max_bitrate: 4_000_000,
            },
            Box::new(move |frame: super::Frame| {
                let buffer = unsafe {
                    NativeBuffer::from_cv_pixel_buffer(frame.into_consumable_pixel_buffer())
                };
                source.capture_frame(&VideoFrame {
                    rotation: VideoRotation::VideoRotation0,
                    timestamp_us: 0,
                    frame_metadata: None,
                    buffer,
                });
                c.fetch_add(1, Ordering::Relaxed);
            }),
            fatal_tx,
        )
        .expect("capture starts");

        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        drop(cap);
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let n = count.load(Ordering::Relaxed);
        eprintln!("--- frames through the encoder handoff: {n} ---");
        assert!(n > 0, "no frames captured — the test proved nothing");
    }
}
