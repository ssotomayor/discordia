use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use livekit::webrtc::video_frame::I420Buffer;
use windows_capture::capture::{CaptureControl, Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame as CapturedFrame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::monitor::Monitor;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings as CaptureSettings,
};
use windows_capture::window::Window;

use super::{Frame, FrameSink, Settings, Source, Target};

pub fn sources() -> Result<Vec<Source>, String> {
    let mut sources = Vec::new();
    for monitor in Monitor::enumerate().map_err(|e| e.to_string())? {
        sources.push(Source {
            target: Target::WindowsMonitor(monitor.as_raw_hmonitor() as isize),
            title: monitor.name().map_err(|e| e.to_string())?,
            app: None,
            width: monitor.width().map_err(|e| e.to_string())?,
            height: monitor.height().map_err(|e| e.to_string())?,
        });
    }
    for window in Window::enumerate().map_err(|e| e.to_string())? {
        if !window.is_valid() || window.process_id().ok() == Some(std::process::id()) {
            continue;
        }
        let title = window.title().map_err(|e| e.to_string())?;
        if title.trim().is_empty() {
            continue;
        }
        sources.push(Source {
            target: Target::WindowsWindow(window.as_raw_hwnd() as isize),
            title,
            app: Some(
                window
                    .process_name()
                    .unwrap_or_else(|_| "Application".into()),
            ),
            width: window.width().unwrap_or(0).max(0) as u32,
            height: window.height().unwrap_or(0).max(0) as u32,
        });
    }
    Ok(sources)
}

struct Flags {
    sink: FrameSink,
    fatal: tokio::sync::mpsc::UnboundedSender<String>,
    settings: Settings,
    count_frames: bool,
}

struct Handler {
    flags: Flags,
    pacer: FramePacer,
}

struct FramePacer {
    interval: Duration,
    next: Option<Instant>,
}

impl FramePacer {
    fn new(fps: u32) -> Self {
        Self {
            interval: Duration::from_secs_f64(1.0 / fps.max(1) as f64),
            next: None,
        }
    }

    fn accept(&mut self, now: Instant) -> bool {
        let Some(next) = self.next else {
            self.next = Some(now + self.interval);
            return true;
        };
        // Capture callbacks jitter around the display cadence; resetting to now drops alternate frames.
        if now + Duration::from_millis(1) < next {
            return false;
        }
        self.next = Some(if now.saturating_duration_since(next) >= self.interval {
            now + self.interval
        } else {
            next + self.interval
        });
        true
    }
}

impl GraphicsCaptureApiHandler for Handler {
    type Flags = Flags;
    type Error = String;

    fn new(ctx: Context<Flags>) -> Result<Self, String> {
        Ok(Self {
            pacer: FramePacer::new(ctx.flags.settings.fps),
            flags: ctx.flags,
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut CapturedFrame<'_>,
        control: InternalCaptureControl,
    ) -> Result<(), String> {
        let now = Instant::now();
        if !self.pacer.accept(now) {
            return Ok(());
        }
        if let Err(e) = self.deliver(frame) {
            if self.flags.fatal.send(e).is_err() {
                eprintln!("[screen] capture failure after session teardown");
            }
            control.stop();
        }
        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), String> {
        if self
            .flags
            .fatal
            .send("The shared window or display was closed".into())
            .is_err()
        {
            eprintln!("[screen] source closed after session teardown");
        }
        Ok(())
    }
}

impl Handler {
    fn deliver(&self, frame: &mut CapturedFrame<'_>) -> Result<(), String> {
        let width = frame.width();
        let height = frame.height();
        if width == 0 || height == 0 {
            return Ok(());
        }
        let mut mapped = frame.buffer().map_err(|e| e.to_string())?;
        let stride = mapped.row_pitch();
        let mut buffer = I420Buffer::new(width, height);
        let (sy, su, sv) = buffer.strides();
        let (y, u, v) = buffer.data_mut();
        livekit::webrtc::native::yuv_helper::argb_to_i420(
            mapped.as_raw_buffer(),
            stride,
            y,
            sy,
            u,
            su,
            v,
            sv,
            width as i32,
            height as i32,
        );
        let (out_width, out_height) = super::fit_resolution(width, height, self.flags.settings);
        if (width, height) != (out_width, out_height) {
            buffer = buffer.scale(out_width as i32, out_height as i32);
        }
        (self.flags.sink)(Frame { buffer });
        if self.flags.count_frames {
            super::FRAMES_CAPTURED.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }
}

pub struct WinVideoCapture {
    control: Option<CaptureControl<Handler, String>>,
}

impl WinVideoCapture {
    pub fn stop(mut self) -> Result<(), String> {
        match self.control.take() {
            Some(control) => control.stop().map_err(|e| e.to_string()),
            None => Ok(()),
        }
    }
    pub fn start(
        target: Target,
        settings: Settings,
        sink: FrameSink,
        fatal: tokio::sync::mpsc::UnboundedSender<String>,
        count_frames: bool,
    ) -> Result<Self, String> {
        let flags = Flags {
            sink,
            fatal,
            settings,
            count_frames,
        };
        let control = match target {
            Target::WindowsMonitor(handle) => Handler::start_free_threaded(capture_settings(
                Monitor::from_raw_hmonitor(handle as *mut std::ffi::c_void),
                flags,
            )),
            Target::WindowsWindow(handle) => Handler::start_free_threaded(capture_settings(
                Window::from_raw_hwnd(handle as *mut std::ffi::c_void),
                flags,
            )),
            _ => return Err("The selected source is not a Windows capture target".into()),
        }
        .map_err(|e| e.to_string())?;
        Ok(Self {
            control: Some(control),
        })
    }
}

fn capture_settings<T: TryInto<windows_capture::settings::GraphicsCaptureItemType>>(
    item: T,
    flags: Flags,
) -> CaptureSettings<Flags, T> {
    CaptureSettings::new(
        item,
        CursorCaptureSettings::Default,
        DrawBorderSettings::Default,
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Default,
        DirtyRegionSettings::Default,
        ColorFormat::Bgra8,
        flags,
    )
}

impl Drop for WinVideoCapture {
    fn drop(&mut self) {
        if let Some(control) = self.control.take() {
            // Joining the capture worker must not block the app's event loop.
            std::thread::spawn(move || {
                if let Err(e) = control.stop() {
                    eprintln!("[screen] capture shutdown: {e}");
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    use livekit::webrtc::video_frame::{VideoBuffer, VideoFrame, VideoRotation};
    use livekit::webrtc::video_source::VideoResolution;
    use livekit::webrtc::video_source::native::NativeVideoSource;

    use super::*;

    #[test]
    fn frame_pacer_preserves_display_cadence_with_jitter() {
        let start = Instant::now();
        let mut pacer = FramePacer::new(60);
        for i in 0..120 {
            let jitter = if i % 2 == 0 { 0 } else { 400 };
            assert!(pacer.accept(start + Duration::from_micros(i * 16_667 + jitter)));
        }
    }

    #[test]
    fn frame_pacer_downsamples_without_catching_up_after_stalls() {
        let start = Instant::now();
        let mut pacer = FramePacer::new(30);
        let accepted = (0..120)
            .filter(|i| pacer.accept(start + Duration::from_micros(i * 16_667)))
            .count();
        assert_eq!(accepted, 60);
        assert!(pacer.accept(start + Duration::from_secs(10)));
        assert!(!pacer.accept(start + Duration::from_secs(10) + Duration::from_millis(2)));
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs an interactive Windows desktop and a visible window"]
    async fn selected_windows_and_monitors_feed_livekit_and_stop() {
        let sources = sources().expect("enumerate sources");
        for window in [false, true] {
            let target = sources
                .iter()
                .find(|source| source.app.is_some() == window)
                .expect("a monitor and visible window")
                .target;
            let source = NativeVideoSource::new(
                VideoResolution {
                    width: 640,
                    height: 360,
                },
                true,
            );
            let count = Arc::new(AtomicUsize::new(0));
            let frame_count = count.clone();
            let (fatal, mut failures) = tokio::sync::mpsc::unbounded_channel();
            let capture = WinVideoCapture::start(
                target,
                Settings {
                    width: 640,
                    height: 360,
                    fps: 15,
                    max_bitrate: 1_000_000,
                },
                Box::new(move |frame| {
                    assert!(frame.buffer.width() <= 640 && frame.buffer.height() <= 360);
                    source.capture_frame(&VideoFrame {
                        rotation: VideoRotation::VideoRotation0,
                        timestamp_us: 0,
                        frame_metadata: None,
                        buffer: frame.buffer,
                    });
                    frame_count.fetch_add(1, Ordering::Relaxed);
                }),
                fatal,
                true,
            )
            .expect("start capture");
            tokio::time::sleep(Duration::from_secs(3)).await;
            assert!(failures.try_recv().is_err(), "capture failed");
            assert!(
                count.load(Ordering::Relaxed) > 0,
                "no video reached LiveKit"
            );
            drop(capture);
            tokio::time::sleep(Duration::from_millis(500)).await;
            let stopped = count.load(Ordering::Relaxed);
            tokio::time::sleep(Duration::from_millis(300)).await;
            assert_eq!(
                count.load(Ordering::Relaxed),
                stopped,
                "capture continued after drop"
            );
            let before = super::super::frames_captured();
            let preview = super::super::thumbnail(target)
                .await
                .expect("native thumbnail");
            use base64::Engine;
            let png = base64::engine::general_purpose::STANDARD
                .decode(
                    preview
                        .strip_prefix("data:image/png;base64,")
                        .expect("PNG data URL"),
                )
                .expect("decode preview");
            let image = image::load_from_memory(&png).expect("valid PNG");
            assert!(image.width() > 0 && image.width() <= 320);
            assert!(image.height() > 0 && image.height() <= 180);
            assert_eq!(
                super::super::frames_captured(),
                before,
                "previews polluted stream diagnostics"
            );
        }
    }
}
