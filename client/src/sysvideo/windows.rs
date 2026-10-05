#[cfg(test)]
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use livekit::webrtc::video_frame::{I420Buffer, VideoBuffer};
use windows_capture::capture::{CaptureControl, Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame as CapturedFrame;
use windows_capture::graphics_capture_api::{GraphicsCaptureApi, InternalCaptureControl};
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
    metrics: std::sync::Arc<super::metrics::Metrics>,
}

struct Handler {
    flags: Flags,
    pacer: FramePacer,
    converter: BgraConverter,
    previous_callback: Option<Instant>,
}

#[derive(Default)]
struct BgraConverter {
    scratch: Option<I420Buffer>,
}

impl BgraConverter {
    fn convert(
        &mut self,
        pixels: &mut [u8],
        stride: u32,
        width: u32,
        height: u32,
        settings: Settings,
    ) -> Result<I420Buffer, String> {
        if width == 0
            || height == 0
            || stride < width.saturating_mul(4)
            || !stride.is_multiple_of(4)
            || pixels.len() < stride as usize * height as usize
        {
            return Err("Invalid captured pixel buffer".into());
        }
        let (out_width, out_height) = super::fit_resolution(width, height, settings);
        let resize = (width, height) != (out_width, out_height);
        let mut direct = (!resize).then(|| I420Buffer::new(width, height));
        let buffer = if resize {
            if self
                .scratch
                .as_ref()
                .is_none_or(|buffer| buffer.width() != width || buffer.height() != height)
            {
                self.scratch = Some(I420Buffer::new(width, height));
            }
            let Some(scratch) = self.scratch.as_mut() else {
                return Err("Missing screen conversion buffer".into());
            };
            scratch
        } else {
            self.scratch = None;
            let Some(buffer) = direct.as_mut() else {
                return Err("Missing screen output buffer".into());
            };
            buffer
        };
        let (sy, su, sv) = buffer.strides();
        let (y, u, v) = buffer.data_mut();
        livekit::webrtc::native::yuv_helper::argb_to_i420(
            pixels,
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
        if resize {
            // Only the scaled buffer reaches WebRTC; the full-size scratch remains exclusively ours.
            Ok(buffer.scale(out_width as i32, out_height as i32))
        } else {
            direct.ok_or_else(|| "Missing screen output buffer".into())
        }
    }
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
            converter: BgraConverter::default(),
            previous_callback: None,
            flags: ctx.flags,
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut CapturedFrame<'_>,
        control: InternalCaptureControl,
    ) -> Result<(), String> {
        let now = Instant::now();
        let accepted = self.pacer.accept(now);
        if self.flags.count_frames {
            self.flags.metrics.callback(
                self.previous_callback
                    .map_or(Duration::ZERO, |previous| now.duration_since(previous)),
                !accepted,
            );
            self.previous_callback = Some(now);
        }
        if !accepted {
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
    fn deliver(&mut self, frame: &mut CapturedFrame<'_>) -> Result<(), String> {
        let started = Instant::now();
        let width = frame.width();
        let height = frame.height();
        if width == 0 || height == 0 {
            return Ok(());
        }
        let mut mapped = frame.buffer().map_err(|e| e.to_string())?;
        let mapped_at = Instant::now();
        let stride = mapped.row_pitch();
        let buffer = self.converter.convert(
            mapped.as_raw_buffer(),
            stride,
            width,
            height,
            self.flags.settings,
        )?;
        let converted_at = Instant::now();
        // WebRTC must not retain the mapped staging texture while accepting the converted frame.
        drop(mapped);
        let (out_width, out_height) = super::fit_resolution(width, height, self.flags.settings);
        (self.flags.sink)(Frame { buffer });
        if self.flags.count_frames {
            self.flags.metrics.record_windows(
                out_width,
                out_height,
                started.elapsed(),
                mapped_at.duration_since(started),
                converted_at.duration_since(mapped_at),
            );
        }
        Ok(())
    }
}

pub struct WinVideoCapture {
    control: Option<CaptureControl<Handler, String>>,
    pub(super) metrics: std::sync::Arc<super::metrics::Metrics>,
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
        let metrics = std::sync::Arc::new(super::metrics::Metrics::default());
        let flags = Flags {
            sink,
            fatal,
            settings,
            count_frames,
            metrics: metrics.clone(),
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
            metrics,
        })
    }
}

fn capture_settings<T: TryInto<windows_capture::settings::GraphicsCaptureItemType>>(
    item: T,
    flags: Flags,
) -> CaptureSettings<Flags, T> {
    let border = if GraphicsCaptureApi::is_border_settings_supported().unwrap_or(false) {
        DrawBorderSettings::WithoutBorder
    } else {
        DrawBorderSettings::Default
    };
    CaptureSettings::new(
        item,
        CursorCaptureSettings::Default,
        border,
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
    fn resizing_bgra_ignores_row_padding_and_preserves_colors() {
        use livekit::webrtc::video_frame::VideoFormatType;
        use livekit::webrtc::video_frame::native::VideoFrameBufferExt;
        let mut converter = BgraConverter::default();
        let settings = Settings {
            width: 4,
            height: 2,
            fps: 60,
            max_bitrate: 16_000_000,
            adaptive_quality: false,
            priority: super::super::Priority::Motion,
            codec: super::super::Codec::H264,
            encoder: super::super::Encoder::Auto,
        };
        for width in [8, 12, 4] {
            let stride = width * 4 + 16;
            let mut pixels = vec![0; stride as usize * 4];
            for row in pixels.chunks_exact_mut(stride as usize) {
                for pixel in row.as_chunks_mut::<4>().0 {
                    pixel.copy_from_slice(&[0, 255, 0, 255]);
                }
                for pixel in row[..width as usize * 4].as_chunks_mut::<4>().0 {
                    pixel.copy_from_slice(&[0, 0, 255, 255]);
                }
            }
            let buffer = converter
                .convert(&mut pixels, stride, width, 4, settings)
                .expect("convert padded image");
            let mut rgb = vec![0; (buffer.width() * buffer.height() * 4) as usize];
            buffer.to_argb(
                VideoFormatType::ABGR,
                &mut rgb,
                buffer.width() * 4,
                buffer.width() as i32,
                buffer.height() as i32,
            );
            for pixel in rgb.as_chunks::<4>().0 {
                assert!(
                    pixel[0] > 240 && pixel[1] < 15 && pixel[2] < 15,
                    "{pixel:?}"
                );
            }
            assert!(buffer.width() <= 4 && buffer.height() <= 2);
        }
        assert!(converter.convert(&mut [0; 4], 4, 8, 4, settings).is_err());
    }

    #[test]
    #[ignore = "manual screen conversion performance comparison"]
    fn compare_screen_conversion_cost() {
        let (width, height) = (3840, 2160);
        let mut pixels = [40, 100, 200, 255].repeat((width * height) as usize);
        let settings = Settings {
            width: 1920,
            height: 1080,
            fps: 60,
            max_bitrate: 16_000_000,
            adaptive_quality: false,
            priority: super::super::Priority::Motion,
            codec: super::super::Codec::H264,
            encoder: super::super::Encoder::Auto,
        };
        let mut converter = BgraConverter::default();
        for _ in 0..3 {
            std::hint::black_box(
                converter
                    .convert(&mut pixels, width * 4, width, height, settings)
                    .expect("convert"),
            );
        }
        let started = Instant::now();
        for _ in 0..30 {
            let mut full = I420Buffer::new(width, height);
            let (sy, su, sv) = full.strides();
            let (y, u, v) = full.data_mut();
            livekit::webrtc::native::yuv_helper::argb_to_i420(
                &pixels,
                width * 4,
                y,
                sy,
                u,
                su,
                v,
                sv,
                width as i32,
                height as i32,
            );
            std::hint::black_box(full.scale(1920, 1080));
        }
        let before = started.elapsed().as_secs_f64() * 1000.0 / 30.0;
        let started = Instant::now();
        for _ in 0..30 {
            std::hint::black_box(
                converter
                    .convert(&mut pixels, width * 4, width, height, settings)
                    .expect("convert"),
            );
        }
        let after = started.elapsed().as_secs_f64() * 1000.0 / 30.0;
        eprintln!(
            "4K to 1080p: allocate I420 then scale {before:.2} ms/frame; reuse I420 then scale {after:.2} ms/frame"
        );
    }

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
                    adaptive_quality: false,
                    priority: super::super::Priority::Balanced,
                    codec: super::super::Codec::H264,
                    encoder: super::super::Encoder::Auto,
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
            let metrics = capture.metrics.clone();
            drop(capture);
            tokio::time::sleep(Duration::from_millis(500)).await;
            let stopped = count.load(Ordering::Relaxed);
            tokio::time::sleep(Duration::from_millis(300)).await;
            assert_eq!(
                count.load(Ordering::Relaxed),
                stopped,
                "capture continued after drop"
            );
            let before = metrics.snapshot().frames;
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
                metrics.snapshot().frames,
                before,
                "previews polluted stream diagnostics"
            );
        }
    }
}
