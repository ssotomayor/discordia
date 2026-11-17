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

use super::pacing::FramePacer;
use super::{Frame, FrameSink, Settings, Source, Target};

#[path = "windows_gpu.rs"]
mod gpu;

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
        if !window.is_valid() {
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
    gpu: Option<gpu::Processor>,
    gpu_disabled: bool,
    gpu_choice: gpu::Choice,
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

impl GraphicsCaptureApiHandler for Handler {
    type Flags = Flags;
    type Error = String;

    fn new(ctx: Context<Flags>) -> Result<Self, String> {
        let gpu_disabled = ctx.flags.settings.encoder == super::Encoder::Cpu;
        Ok(Self {
            pacer: FramePacer::new(ctx.flags.settings.fps),
            converter: BgraConverter::default(),
            previous_callback: None,
            gpu: None,
            gpu_disabled,
            gpu_choice: gpu::Choice::default(),
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
        // WebRTC aligns this monotonic capture clock; conversion delays must not retime the image.
        let timestamp_us = frame.timestamp().map_err(|e| e.to_string())?.Duration / 10;
        let width = frame.width();
        let height = frame.height();
        if width == 0 || height == 0 {
            return Ok(());
        }
        let (out_width, out_height) = super::fit_resolution(width, height, self.flags.settings);
        let mut prepared = None;
        if !self.gpu_disabled {
            let cpu_probe = if self.gpu_choice.compare_cpu_first() {
                let began = Instant::now();
                Some((self.cpu_frame(frame)?, began.elapsed()))
            } else {
                None
            };
            let gpu_started = Instant::now();
            let geometry = (width, height, out_width, out_height);
            let result = (|| {
                if self.gpu.as_ref().is_none_or(|gpu| gpu.geometry != geometry) {
                    self.gpu_choice = gpu::Choice::default();
                    self.gpu = Some(gpu::Processor::new(
                        frame.device(),
                        frame.device_context(),
                        geometry,
                        self.flags.settings.fps,
                    )?);
                    tracing::info!(
                        ?geometry,
                        capture_path = "D3D11-NV12",
                        "evaluating GPU screen preprocessing"
                    );
                }
                self.gpu
                    .as_ref()
                    .ok_or_else(|| {
                        windows_capture_api::core::Error::from(
                            windows_capture_api::Win32::Foundation::E_FAIL,
                        )
                    })?
                    .convert(frame.as_raw_texture())
            })();
            match result {
                Ok(gpu_frame) => {
                    let gpu_cost = gpu_started.elapsed();
                    if self.gpu_choice.selected.is_none() {
                        let (cpu_frame, cpu_cost) = match cpu_probe {
                            Some(probe) => probe,
                            None => {
                                let began = Instant::now();
                                (self.cpu_frame(frame)?, began.elapsed())
                            }
                        };
                        let decision =
                            self.gpu_choice
                                .observe(cpu_cost, gpu_cost, self.flags.settings.fps);
                        if let Some(use_gpu) = decision {
                            let (cpu, gpu) = self.gpu_choice.costs();
                            tracing::info!(
                                use_gpu,
                                cpu_ms = cpu.as_secs_f64() * 1000.0,
                                gpu_ms = gpu.as_secs_f64() * 1000.0,
                                "screen preprocessing cost comparison completed"
                            );
                            if !use_gpu {
                                self.gpu = None;
                                self.gpu_disabled = true;
                            }
                        }
                        prepared = Some(if decision == Some(true) {
                            gpu_frame
                        } else {
                            cpu_frame
                        });
                    } else if self
                        .gpu_choice
                        .over_budget(gpu_cost, self.flags.settings.fps)
                    {
                        tracing::warn!(
                            capture_path = "CPU-I420",
                            "GPU preprocessing exceeded the frame budget; restoring CPU conversion"
                        );
                        self.gpu = None;
                        self.gpu_disabled = true;
                    } else {
                        prepared = Some(gpu_frame);
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, capture_path = "CPU-I420", "GPU preprocessing unavailable; keeping CPU capture conversion");
                    self.gpu = None;
                    self.gpu_disabled = true;
                    prepared = cpu_probe.map(|(frame, _)| frame);
                }
            }
        }
        let (buffer, readback, conversion) = match prepared {
            Some(frame) => frame,
            None => self.cpu_frame(frame)?,
        };
        (self.flags.sink)(Frame {
            buffer,
            timestamp_us,
        });
        if self.flags.count_frames {
            self.flags.metrics.record_windows(
                out_width,
                out_height,
                started.elapsed(),
                readback,
                conversion,
            );
        }
        Ok(())
    }

    fn cpu_frame(
        &mut self,
        frame: &mut CapturedFrame<'_>,
    ) -> Result<(I420Buffer, Duration, Duration), String> {
        let started = Instant::now();
        let width = frame.width();
        let height = frame.height();
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
        Ok((
            buffer,
            mapped_at.duration_since(started),
            converted_at.duration_since(mapped_at),
        ))
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

    #[tokio::test]
    #[ignore = "needs an interactive Windows desktop"]
    async fn own_window_can_share_video_without_discordia_audio() {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, WINDOW_EX_STYLE, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
        };
        use windows::core::w;

        struct TestWindow(HWND);
        impl Drop for TestWindow {
            fn drop(&mut self) {
                unsafe { DestroyWindow(self.0).expect("destroy test window") };
            }
        }
        let window = TestWindow(unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("STATIC"),
                w!("Discordia capture regression"),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                20,
                20,
                320,
                240,
                None,
                None,
                None,
                None,
            )
            .expect("create own window")
        });
        let target = Target::WindowsWindow(window.0.0 as isize);
        assert!(
            sources()
                .unwrap()
                .iter()
                .any(|source| source.target == target)
        );
        assert!(!crate::sysaudio::supported_for(Some(target)));
        let (audio, _) = crate::audio_queue::channel();
        let (fatal, _) = tokio::sync::mpsc::unbounded_channel();
        assert!(crate::sysaudio::start(audio, fatal, Some(target)).is_err());

        let frames = Arc::new(AtomicUsize::new(0));
        let count = frames.clone();
        let (fatal, mut failures) = tokio::sync::mpsc::unbounded_channel();
        let capture = WinVideoCapture::start(
            target,
            Settings {
                width: 320,
                height: 240,
                fps: 15,
                max_bitrate: 1_000_000,
                adaptive_quality: false,
                priority: super::super::Priority::Balanced,
                codec: super::super::Codec::H264,
                encoder: super::super::Encoder::Auto,
            },
            Box::new(move |_| {
                count.fetch_add(1, Ordering::Relaxed);
            }),
            fatal,
            true,
        )
        .expect("capture own window");
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert!(failures.try_recv().is_err());
        assert!(frames.load(Ordering::Relaxed) > 0);
        drop(capture);
    }

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
        assert!((60..=61).contains(&accepted));
        assert!(pacer.accept(start + Duration::from_secs(10)));
        assert!(!pacer.accept(start + Duration::from_secs(10) + Duration::from_millis(2)));
    }

    #[test]
    fn frame_pacer_keeps_fractional_source_frames_with_callback_delays() {
        for seed in [42_u64, 7, 123456] {
            let start = Instant::now();
            let mut pacer = FramePacer::new(60);
            let mut random = seed;
            for i in 0..7193_u64 {
                random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
                let source_us = (i as f64 * 1_000_000.0 / 59.94).round() as u64;
                let delay_us = (random >> 32) % 4001;
                assert!(pacer.accept(start + Duration::from_micros(source_us + delay_us)));
            }
        }
    }

    #[test]
    fn frame_pacer_rejects_bursts_and_recovers_after_source_stalls() {
        let start = Instant::now();
        let mut pacer = FramePacer::new(60);
        assert!(pacer.accept(start));
        for ms in 0..8 {
            assert!(!pacer.accept(start + Duration::from_millis(ms)));
        }
        let resumed = start + Duration::from_secs(1);
        assert!(pacer.accept(resumed));
        assert!(!pacer.accept(resumed));
        assert!(!pacer.accept(resumed + Duration::from_millis(2)));
        assert!(pacer.accept(resumed + Duration::from_micros(16_683)));
        let mut short_stall = FramePacer::new(60);
        assert!(short_stall.accept(start));
        assert!(short_stall.accept(start + Duration::from_millis(25)));
        assert!(!short_stall.accept(start + Duration::from_micros(25_100)));
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs an interactive Windows desktop and a visible window"]
    async fn selected_windows_and_monitors_feed_livekit_and_stop() {
        let sources = sources().expect("enumerate sources");
        let load = std::env::var_os("DISCORDIA_TEST_CAPTURE_1440P60").is_some();
        let (max_width, max_height, fps) = if load {
            (2560, 1440, 60)
        } else {
            (640, 360, 15)
        };
        for window in [false, true] {
            let target = sources
                .iter()
                .find(|source| source.app.is_some() == window)
                .expect("a monitor and visible window")
                .target;
            let source = NativeVideoSource::new(
                VideoResolution {
                    width: max_width,
                    height: max_height,
                },
                true,
            );
            let count = Arc::new(AtomicUsize::new(0));
            let frame_count = count.clone();
            let (fatal, mut failures) = tokio::sync::mpsc::unbounded_channel();
            let capture = WinVideoCapture::start(
                target,
                Settings {
                    width: max_width,
                    height: max_height,
                    fps,
                    max_bitrate: 1_000_000,
                    adaptive_quality: false,
                    priority: super::super::Priority::Balanced,
                    codec: super::super::Codec::H264,
                    encoder: super::super::Encoder::Auto,
                },
                Box::new(move |frame| {
                    assert!(
                        frame.buffer.width() <= max_width && frame.buffer.height() <= max_height
                    );
                    assert!(frame.timestamp_us > 0);
                    source.capture_frame(&VideoFrame {
                        rotation: VideoRotation::VideoRotation0,
                        timestamp_us: frame.timestamp_us,
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
            let sample = metrics.snapshot();
            eprintln!(
                "Capture under load: window={window} {}x{} target_fps={fps} frames={} readback_ms={:?} conversion_ms={:?} largest_gap_ms={}",
                sample.width,
                sample.height,
                sample.frames,
                sample.readback_ms_since(Default::default()),
                sample.conversion_ms_since(Default::default()),
                sample.largest_gap_us as f64 / 1000.0
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
