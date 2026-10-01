#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(target_os = "macos", target_os = "windows", test))]
pub(crate) mod metrics;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "windows")]
pub struct Frame {
    pub buffer: livekit::webrtc::video_frame::I420Buffer,
}

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

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub type FrameSink = Box<dyn Fn(Frame) + Send + Sync>;

#[cfg(target_os = "macos")]
pub struct Capture {
    _inner: macos::MacVideoCapture,
}

#[cfg(target_os = "windows")]
pub struct Capture {
    _inner: windows::WinVideoCapture,
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
impl Capture {
    pub(crate) fn metrics(&self) -> std::sync::Arc<metrics::Metrics> {
        self._inner.metrics.clone()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub max_bitrate: u64,
    pub priority: Priority,
    pub codec: Codec,
    pub encoder: Encoder,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Encoder {
    #[default]
    Auto,
    Gpu,
    Cpu,
}

pub fn hardware_encoder_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "Hardware"
    } else {
        "GPU"
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Codec {
    #[default]
    H264,
    Vp8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Priority {
    Motion,
    Detail,
    Balanced,
}

#[cfg(any(target_os = "windows", test))]
fn fit_resolution(width: u32, height: u32, settings: Settings) -> (u32, u32) {
    let ratio = (settings.width as f64 / width.max(1) as f64)
        .min(settings.height as f64 / height.max(1) as f64)
        .min(1.0);
    let even = |value: u32| (value / 2 * 2).max(2);
    (
        even((width as f64 * ratio) as u32),
        even((height as f64 * ratio) as u32),
    )
}

#[cfg(test)]
mod resolution_tests {
    #[test]
    fn capture_fits_portrait_and_ultrawide_without_stretching_or_upscaling() {
        let settings = super::Settings {
            width: 1920,
            height: 1080,
            fps: 60,
            max_bitrate: 16_000_000,
            priority: super::Priority::Motion,
            codec: super::Codec::H264,
            encoder: crate::sysvideo::Encoder::Auto,
        };
        assert_eq!(super::fit_resolution(3440, 1440, settings), (1920, 802));
        assert_eq!(super::fit_resolution(1080, 1920, settings), (606, 1080));
        assert_eq!(super::fit_resolution(801, 601, settings), (800, 600));
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Display(u32),
    Window(u32),
    Application(i32),
    #[cfg(target_os = "windows")]
    WindowsMonitor(isize),
    #[cfg(target_os = "windows")]
    WindowsWindow(isize),
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

#[cfg(target_os = "windows")]
pub fn sources() -> Result<Vec<Source>, String> {
    windows::sources()
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn sources() -> Result<Vec<Source>, String> {
    Err("native screen capture isn't implemented on this platform".into())
}

pub fn supported() -> bool {
    cfg!(any(target_os = "macos", target_os = "windows"))
}

pub async fn thumbnail(target: Target) -> Result<String, String> {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        static SLOTS: std::sync::OnceLock<std::sync::Arc<tokio::sync::Semaphore>> =
            std::sync::OnceLock::new();
        let permit = SLOTS
            .get_or_init(|| std::sync::Arc::new(tokio::sync::Semaphore::new(2)))
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| e.to_string())?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            capture_thumbnail(target)
        })
        .await
        .map_err(|e| format!("preview worker failed: {e}"))?
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = target;
        Err("Native previews aren't available on this platform".into())
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn capture_thumbnail(target: Target) -> Result<String, String> {
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    let first = parking_lot::Mutex::new(Some(send));
    let sink: FrameSink = Box::new(move |frame| {
        if let Some(send) = first.lock().take()
            && send.send(frame_png(frame)).is_err()
        {
            tracing::debug!("preview receiver closed");
        }
    });
    let settings = Settings {
        width: 320,
        height: 180,
        fps: 1,
        max_bitrate: 0,
        priority: Priority::Detail,
        codec: Codec::H264,
        encoder: crate::sysvideo::Encoder::Auto,
    };
    let (fatal, _failures) = tokio::sync::mpsc::unbounded_channel();
    #[cfg(target_os = "windows")]
    let capture = windows::WinVideoCapture::start(target, settings, sink, fatal, false)?;
    #[cfg(target_os = "macos")]
    let capture = macos::MacVideoCapture::start(target, settings, sink, fatal, false)?;
    let result = receive
        .recv_timeout(std::time::Duration::from_secs(2))
        .map_err(|e| format!("No preview frame: {e}"));
    #[cfg(target_os = "windows")]
    capture.stop()?;
    #[cfg(target_os = "macos")]
    drop(capture);
    result?
}

#[cfg(all(test, target_os = "windows"))]
mod preview_tests {
    #[test]
    fn thumbnail_png_preserves_color_channels() {
        use base64::Engine;
        let mut buffer = livekit::webrtc::video_frame::I420Buffer::new(2, 2);
        let (sy, su, sv) = buffer.strides();
        let (y, u, v) = buffer.data_mut();
        let red = [0, 0, 255, 255].repeat(4);
        livekit::webrtc::native::yuv_helper::argb_to_i420(&red, 8, y, sy, u, su, v, sv, 2, 2);
        let data = super::frame_png(super::Frame { buffer }).expect("encode preview");
        let png = base64::engine::general_purpose::STANDARD
            .decode(data.split(',').nth(1).expect("data URL"))
            .expect("base64");
        let image = image::load_from_memory(&png).expect("PNG").to_rgba8();
        let pixel = image.get_pixel(0, 0).0;
        assert!(pixel[0] > 240 && pixel[1] < 15 && pixel[2] < 15);
        assert_eq!(pixel[3], 255);
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn frame_png(frame: Frame) -> Result<String, String> {
    use base64::Engine;
    use image::ImageEncoder;
    use livekit::webrtc::video_frame::native::VideoFrameBufferExt;
    use livekit::webrtc::video_frame::{VideoBuffer, VideoFormatType};
    #[cfg(target_os = "windows")]
    let buffer = frame.buffer;
    #[cfg(target_os = "macos")]
    // SAFETY: the retained CVPixelBuffer is handed to LiveKit with its own reference.
    let buffer = unsafe {
        livekit::webrtc::video_frame::native::NativeBuffer::from_cv_pixel_buffer(
            frame.into_consumable_pixel_buffer(),
        )
    };
    let (width, height) = (buffer.width(), buffer.height());
    if width == 0 || height == 0 || width > 320 || height > 180 {
        return Err("Invalid preview size".into());
    }
    let mut pixels = vec![0; (width * height * 4) as usize];
    buffer.to_argb(
        VideoFormatType::ABGR,
        &mut pixels,
        width * 4,
        width as i32,
        height as i32,
    );
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(&pixels, width, height, image::ExtendedColorType::Rgba8)
        .map_err(|e| e.to_string())?;
    Ok(format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(png)
    ))
}

#[cfg(target_os = "windows")]
pub fn start(
    target: Target,
    settings: Settings,
    sink: FrameSink,
    fatal: tokio::sync::mpsc::UnboundedSender<String>,
) -> Result<Capture, String> {
    Ok(Capture {
        _inner: windows::WinVideoCapture::start(target, settings, sink, fatal, true)?,
    })
}

#[cfg(target_os = "macos")]
pub fn start(
    target: Target,
    settings: Settings,
    sink: FrameSink,
    fatal: tokio::sync::mpsc::UnboundedSender<String>,
) -> Result<Capture, String> {
    Ok(Capture {
        _inner: macos::MacVideoCapture::start(target, settings, sink, fatal, true)?,
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
                priority: super::Priority::Balanced,
                codec: super::Codec::H264,
                encoder: crate::sysvideo::Encoder::Auto,
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
