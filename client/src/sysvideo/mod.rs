//! Native screen-*video* capture, for platforms where the webview has no
//! capture API at all.
//!
//! The webview path in `features::screenshare` is the original and still the
//! only one on Windows, where WebView2 is Chromium and `getDisplayMedia` works.
//! macOS is the reason this module exists: WKWebView does not expose
//! `navigator.mediaDevices` — not restricted, *absent* — so the JS controller's
//! capture branch can never run there, and a macOS build could not share a
//! screen at all. (Verified from inside a bundled app: the origin is a secure
//! context and `typeof navigator.mediaDevices` is still `"undefined"`. wry sets
//! several `WKPreferences` keys and never the media-devices one, so its
//! auto-granting permission delegate has nothing to grant.)
//!
//! Capturing natively sidesteps the engine the same way `sysaudio` does for
//! sound: frames become an ordinary LiveKit video track published from Rust.
//!
//! Frames are handed to a sink as owned `Frame`s wrapping the platform's own
//! image buffer, so nothing is copied or converted on the way — see `Frame`.

#[cfg(target_os = "macos")]
mod macos;

/// One captured frame, holding a reference to the platform image buffer that
/// backs it.
///
/// Deliberately opaque and deliberately *owned*: the point is that no pixels
/// are copied between the OS capture callback and the encoder. `libwebrtc` can
/// wrap a `CVPixelBuffer` directly (`NativeBuffer::from_cv_pixel_buffer`), and
/// on Apple silicon that lets the frame reach the hardware encoder without a
/// colour conversion in our process at all. Dropping this releases the buffer.
///
/// Carries no dimensions on purpose: libwebrtc reads them from the pixel buffer
/// itself, so a second copy here could only ever disagree with it — which is
/// exactly what happens when the display mode changes mid-share.
#[cfg(target_os = "macos")]
pub struct Frame {
    /// Retained for as long as this `Frame` lives; released on drop by
    /// `CFRetained`.
    buffer: objc2_core_foundation::CFRetained<objc2_core_video::CVPixelBuffer>,
}

#[cfg(target_os = "macos")]
impl Frame {
    /// A **+1** reference to the underlying `CVPixelBuffer`, for handing to code
    /// that consumes one — specifically `NativeBuffer::from_cv_pixel_buffer`.
    ///
    /// The extra retain is not defensive. `from_cv_pixel_buffer` is documented
    /// as "does not bump the reference count", which reads like a borrow and is
    /// the opposite: the ObjC bridge behind it
    /// (`new_native_buffer_from_platform_image_buffer`) ends with
    /// `CVPixelBufferRelease(pixelBuffer)`, so it *takes over* the reference it
    /// is given. Handing it this `Frame`'s own reference meant every frame was
    /// released twice — once by libwebrtc and once when the `Frame` dropped —
    /// and CoreFoundation traps on an over-release: `EXC_BREAKPOINT` inside
    /// `CFRelease`, on the capture queue, the instant a share started.
    ///
    /// So the caller gets a reference of its own to give away, and the `Frame`
    /// keeps releasing exactly the one it owns.
    pub fn into_consumable_pixel_buffer(&self) -> *mut std::ffi::c_void {
        // `clone` retains; `into_raw` hands the pointer over without releasing.
        objc2_core_foundation::CFRetained::into_raw(self.buffer.clone())
            .as_ptr()
            .cast()
    }
}

/// Where captured frames go. Called on the OS capture thread, once per frame.
///
/// A callback rather than a channel, and that is a correctness choice, not a
/// style one. A channel would have to either copy each frame (defeating the
/// whole point) or send the buffer reference to another thread, where it would
/// outlive the callback and pile up behind any consumer slower than the
/// capture. Invoking the sink inline means the frame is alive exactly as long
/// as the encoder needs it and the queue depth is the OS's to manage.
#[cfg(target_os = "macos")]
pub type FrameSink = Box<dyn Fn(Frame) + Send + Sync>;

/// A running capture. Dropping it stops the capture and releases the OS
/// resources behind it.
#[cfg(target_os = "macos")]
pub struct Capture {
    _inner: macos::MacVideoCapture,
}

/// What to capture and how.
///
/// Mirrors the webview path's quality presets (see
/// `features::screenshare::quality_preset`) so the same setting means the same
/// thing on both paths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Encoder ceiling in bits per second. Not a target: congestion control
    /// still decides what a given uplink actually sends, so a high cap only
    /// helps links that can use it.
    pub max_bitrate: u64,
}

/// Which surface a share is pointed at.
///
/// Plain ids rather than retained OS objects, deliberately: this is stored in
/// `AppState` and re-sent by the effect that survives a voice-session restart,
/// so it has to be `Copy`, comparable, and free of anything thread-bound. The
/// backend re-resolves the id against a fresh `SCShareableContent` at capture
/// time — which is also the only honest way to do it, since the window may have
/// closed between the pick and the start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// A whole display, wallpaper and dock included.
    Display(u32),
    /// One window, wherever it sits and whatever is in front of it.
    Window(u32),
    /// Every window belonging to one application, addressed by the pid of the
    /// app that owns it. Windows opened *after* the share starts are included,
    /// which is the behaviour that makes "share an app" useful.
    Application(i32),
}

/// A pickable surface, as the picker UI needs it.
///
/// Flattened for the UI rather than mirroring the OS types: `title` is what to
/// show, `app` is what to group by, and `target` is the only part the capture
/// path cares about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    pub target: Target,
    /// What to show in the list: a window's title, "Entire screen" / "Screen N"
    /// for a display, or "<App> — all N windows" for an application.
    pub title: String,
    /// Owning application, for grouping windows under a heading. `None` for
    /// displays and for the application entries themselves.
    pub app: Option<String>,
    pub width: u32,
    pub height: u32,
}

/// Everything shareable right now: displays first, then applications, then
/// individual windows.
///
/// Blocks on the OS query (it is asynchronous and can sit behind the permission
/// dialog for as long as the user takes), so call it off the UI thread.
#[cfg(target_os = "macos")]
pub fn sources() -> Result<Vec<Source>, String> {
    macos::sources()
}

#[cfg(not(target_os = "macos"))]
pub fn sources() -> Result<Vec<Source>, String> {
    Err("native screen capture isn't implemented on this platform".into())
}

/// Whether this build can capture the screen without help from the webview.
///
/// `false` does not mean sharing is impossible — it means the webview path is
/// the one in charge, which on Windows is where it works.
pub fn supported() -> bool {
    cfg!(target_os = "macos")
}

/// Frames handed to the sink since the current capture started.
///
/// A process-global counter rather than plumbed state, which is honest about the
/// shape of the thing: there is at most one screen capture per process. It is
/// written from the OS capture thread and read by the UI, so it is an atomic and
/// nothing more — no allocation, no lock, nothing that could stall a capture
/// callback.
///
/// It exists because the sharer otherwise has no way to tell a working share
/// from a dead one. There is no self-preview on this path (LiveKit does not loop
/// a publication back to its publisher, and the webview holds no local track),
/// so "frames are leaving this machine" is the only confidence signal available
/// — and a number that climbs is a real one.
pub(crate) static FRAMES_CAPTURED: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// How many frames the running capture has produced. 0 when nothing is running.
pub fn frames_captured() -> u64 {
    FRAMES_CAPTURED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Begin capturing the screen, handing each frame to `sink`.
///
/// `fatal` carries the reason a *running* capture died. Starting successfully is
/// only half of it: ScreenCaptureKit reports a stream that stops on its own
/// through its delegate, and without that the track stays published and simply
/// freezes — the one failure a sharer cannot see for themselves.
#[cfg(target_os = "macos")]
pub fn start(
    target: Target,
    settings: Settings,
    sink: FrameSink,
    fatal: tokio::sync::mpsc::UnboundedSender<String>,
) -> Result<Capture, String> {
    // Zeroed here rather than on stop, so a UI reading it during teardown sees
    // the count from the share that just ended instead of a stale one from the
    // share before it.
    FRAMES_CAPTURED.store(0, std::sync::atomic::Ordering::Relaxed);
    Ok(Capture {
        _inner: macos::MacVideoCapture::start(target, settings, sink, fatal)?,
    })
}

#[cfg(test)]
mod tests {
    /// Regression test for the over-release that crashed every share the moment
    /// it started: `EXC_BREAKPOINT` inside `CFRelease` on the capture queue,
    /// because `from_cv_pixel_buffer` consumes the reference it is handed and
    /// `Frame` released the same one again on drop. See
    /// `Frame::into_consumable_pixel_buffer`.
    ///
    /// Exercises the real path — an actual ScreenCaptureKit capture feeding real
    /// `NativeVideoSource::capture_frame` calls — because that is the only thing
    /// that reproduces it; nothing about the types is wrong, only the ownership.
    ///
    /// `#[ignore]`d because it needs the Screen Recording (TCC) grant and a
    /// display, so it cannot run headlessly in CI. Run it on a Mac with:
    ///
    /// ```sh
    /// cargo test -p dioxusfun --bin Discordia -- --ignored --nocapture
    /// ```
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
