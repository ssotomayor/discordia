pub fn supported() -> bool {
    cfg!(any(target_os = "android", target_os = "windows"))
}

#[cfg(target_os = "android")]
use android::start_preview;
#[cfg(target_os = "android")]
pub use android::{Capture, devices, start};

#[cfg(target_os = "windows")]
use windows::start_preview;
#[cfg(target_os = "windows")]
pub use windows::{Capture, devices, start};

#[cfg(any(target_os = "android", target_os = "windows"))]
#[path = "camera_preview.rs"]
mod preview;
#[cfg(any(target_os = "android", target_os = "windows"))]
pub use preview::{PreviewCommand, preview_controller};

#[cfg(target_os = "android")]
mod android {
    use std::sync::Arc;
    use std::time::Duration;

    use livekit::webrtc::video_frame::VideoFrame;
    use livekit::webrtc::video_source::native::NativeVideoSource;

    use crate::camera_quality::CameraQuality;
    use crate::state::CameraDevice;

    pub struct Capture {
        slot: u32,
        pub device: CameraDevice,
    }

    impl Capture {
        pub async fn stop(mut self) {
            crate::android::unregister_frame_sink(self.slot);
            if crate::android::stop_camera_capture_wait(self.slot)
                .await
                .is_ok()
            {
                self.slot = 0;
            }
        }
    }

    impl Drop for Capture {
        fn drop(&mut self) {
            if self.slot == 0 {
                return;
            }
            crate::android::unregister_frame_sink(self.slot);
            crate::android::stop_camera_capture(self.slot);
        }
    }

    pub fn devices() -> Result<Vec<CameraDevice>, String> {
        crate::android::camera_devices()
    }

    pub(super) async fn start_preview(
        device: Option<String>,
        quality: CameraQuality,
        preview: tokio::sync::mpsc::Sender<super::preview::PreviewBuffer>,
        fatal: tokio::sync::mpsc::UnboundedSender<String>,
    ) -> Result<Capture, String> {
        crate::android::prepare_camera().await?;
        crate::android::ensure_media_ready().await?;
        let quality = quality.normalized();
        let selected = devices()?
            .into_iter()
            .find(|camera| device.as_ref().is_none_or(|id| camera.id == *id))
            .ok_or("No available camera")?;
        let slot = crate::android::next_slot();
        let capture = Capture {
            slot,
            device: selected,
        };
        let queued = preview.clone();
        let pacer = parking_lot::Mutex::new(crate::sysvideo::pacing::FramePacer::new(5));
        crate::android::register_frame_sink(
            slot,
            Arc::new(move |frame| {
                // Backpressure bounds full-resolution frames while the preview encoder catches up.
                let _ = preview.try_send(super::preview::PreviewBuffer {
                    buffer: frame.buffer,
                    rotation: frame.rotation,
                });
            }),
            fatal,
        );
        crate::android::filter_capture_frames(
            slot,
            Arc::new(move || {
                !queued.is_closed()
                    && queued.capacity() > 0
                    && pacer.lock().accept(std::time::Instant::now())
            }),
        );
        crate::android::start_camera_capture(
            slot,
            Some(capture.device.id.clone()),
            quality.width(),
            quality.height,
            quality.fps,
        )
        .await?;
        Ok(capture)
    }

    pub async fn start(
        device: Option<String>,
        source: NativeVideoSource,
        quality: CameraQuality,
        fatal: tokio::sync::mpsc::UnboundedSender<String>,
    ) -> Result<Capture, String> {
        let quality = quality.normalized();
        let available = crate::android::camera_devices()?;
        let selected = match device {
            Some(id) => available.into_iter().find(|camera| camera.id == id),
            None => available.into_iter().next(),
        }
        .ok_or_else(|| {
            "No available camera. Connect one or check camera permissions.".to_string()
        })?;
        let slot = crate::android::next_slot();
        let (first_tx, first_rx) = tokio::sync::oneshot::channel();
        let first = parking_lot::Mutex::new(Some(first_tx));
        crate::android::register_frame_sink(
            slot,
            Arc::new(move |frame: crate::sysvideo::Frame| {
                // The track is published after this capture opens, so the
                // adapter drops frames until then; the camera is still live.
                source.capture_frame(&VideoFrame {
                    rotation: frame.rotation,
                    timestamp_us: frame.timestamp_us,
                    frame_metadata: None,
                    buffer: frame.buffer,
                });
                if let Some(tx) = first.lock().take() {
                    let _ = tx.send(());
                }
            }),
            fatal,
        );
        let capture = Capture {
            slot,
            device: selected,
        };
        crate::android::start_camera_capture(
            slot,
            Some(capture.device.id.clone()),
            quality.width(),
            quality.height,
            quality.fps,
        )
        .await?;
        match tokio::time::timeout(Duration::from_secs(12), first_rx).await {
            Ok(Ok(())) => Ok(capture),
            _ => Err("the camera produced no frames".into()),
        }
    }
}

#[cfg(target_os = "windows")]
mod windows {
    use std::sync::{Arc, Mutex, mpsc};
    use std::time::Duration;

    use livekit::webrtc::video_frame::{I420Buffer, VideoFrame, VideoRotation};
    use livekit::webrtc::video_source::native::NativeVideoSource;
    use webrtc_sys::camera_capture::{CameraSink, CameraSinkWrapper, ffi};
    use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};

    use crate::camera_quality::CameraQuality;
    use crate::state::CameraDevice;

    struct Apartment;

    impl Apartment {
        fn new() -> Result<Self, String> {
            // SAFETY: each dedicated capture worker balances its own COM initialization.
            unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
                .ok()
                .map_err(|e| format!("camera COM initialization failed: {e}"))?;
            Ok(Self)
        }
    }

    impl Drop for Apartment {
        fn drop(&mut self) {
            // SAFETY: this apartment never leaves the thread that initialized it.
            unsafe { CoUninitialize() };
        }
    }

    pub fn devices() -> Result<Vec<CameraDevice>, String> {
        std::thread::spawn(|| {
            let _apartment = Apartment::new()?;
            Ok(ffi::camera_devices()
                .into_iter()
                .map(|d| CameraDevice {
                    id: d.id,
                    label: d.label,
                })
                .collect())
        })
        .join()
        .map_err(|_| "camera enumeration worker failed".to_string())?
    }

    pub struct Capture {
        stop: mpsc::Sender<()>,
        worker: Option<std::thread::JoinHandle<()>>,
        pub device: CameraDevice,
    }

    impl Capture {
        pub async fn stop(mut self) {
            if self.stop.send(()).is_err() {
                tracing::debug!("camera worker already stopped");
            }
            if let Some(worker) = self.worker.take() {
                match tokio::task::spawn_blocking(move || worker.join()).await {
                    Ok(Ok(())) => {}
                    _ => tracing::error!("camera stop worker failed"),
                }
            }
        }
    }

    impl Drop for Capture {
        fn drop(&mut self) {
            if self.stop.send(()).is_err() {
                tracing::debug!("camera worker already stopped");
            }
        }
    }

    struct Sink {
        source: Option<NativeVideoSource>,
        preview: Option<tokio::sync::mpsc::Sender<super::preview::PreviewBuffer>>,
        first: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
        alive: Arc<Mutex<std::time::Instant>>,
    }

    impl CameraSink for Sink {
        fn on_frame(
            &self,
            width: u32,
            height: u32,
            strides: (u32, u32, u32),
            planes: (&[u8], &[u8], &[u8]),
            timestamp_us: i64,
        ) {
            let Some(buffer) = copy_frame(width, height, strides, planes) else {
                return;
            };
            if let Some(source) = &self.source {
                source.capture_frame(&VideoFrame {
                    rotation: VideoRotation::VideoRotation0,
                    timestamp_us,
                    frame_metadata: None,
                    buffer,
                });
            } else if let Some(preview) = &self.preview {
                // A slow webview must never accumulate full-resolution camera frames.
                let _ = preview.try_send(super::preview::PreviewBuffer {
                    buffer,
                    rotation: VideoRotation::VideoRotation0,
                });
            }
            if let Ok(mut alive) = self.alive.lock() {
                *alive = std::time::Instant::now();
            }
            if let Ok(mut first) = self.first.lock()
                && let Some(tx) = first.take()
                && tx.send(()).is_err()
            {
                tracing::debug!("camera start was cancelled");
            }
        }
    }

    fn copy_frame(
        width: u32,
        height: u32,
        strides: (u32, u32, u32),
        planes: (&[u8], &[u8], &[u8]),
    ) -> Option<I420Buffer> {
        if width == 0
            || height == 0
            || width > 8192
            || height > 8192
            || u64::from(width) * u64::from(height) > 16_777_216
        {
            return None;
        }
        let sizes = [
            (width, height),
            (width.div_ceil(2), height.div_ceil(2)),
            (width.div_ceil(2), height.div_ceil(2)),
        ];
        let input = [planes.0, planes.1, planes.2];
        let strides = [strides.0, strides.1, strides.2];
        for i in 0..3 {
            if strides[i] < sizes[i].0
                || input[i].len() < (strides[i] as usize).checked_mul(sizes[i].1 as usize)?
            {
                return None;
            }
        }
        let mut buffer = I420Buffer::new(width, height);
        let (sy, su, sv) = buffer.strides();
        let (y, u, v) = buffer.data_mut();
        for (i, (output, stride)) in [(y, sy), (u, su), (v, sv)].into_iter().enumerate() {
            for row in 0..sizes[i].1 as usize {
                let src = row * strides[i] as usize;
                let dst = row * stride as usize;
                let len = sizes[i].0 as usize;
                output[dst..dst + len].copy_from_slice(&input[i][src..src + len]);
            }
        }
        Some(buffer)
    }

    pub async fn start(
        device: Option<String>,
        source: NativeVideoSource,
        quality: CameraQuality,
        fatal: tokio::sync::mpsc::UnboundedSender<String>,
    ) -> Result<Capture, String> {
        start_capture(device, Some(source), quality, None, fatal).await
    }

    pub(super) async fn start_preview(
        device: Option<String>,
        quality: CameraQuality,
        preview: tokio::sync::mpsc::Sender<super::preview::PreviewBuffer>,
        fatal: tokio::sync::mpsc::UnboundedSender<String>,
    ) -> Result<Capture, String> {
        start_capture(device, None, quality, Some(preview), fatal).await
    }

    async fn start_capture(
        device: Option<String>,
        source: Option<NativeVideoSource>,
        quality: CameraQuality,
        preview: Option<tokio::sync::mpsc::Sender<super::preview::PreviewBuffer>>,
        fatal: tokio::sync::mpsc::UnboundedSender<String>,
    ) -> Result<Capture, String> {
        let quality = quality.normalized();
        let (stop, stopped) = mpsc::channel();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (first_tx, first_rx) = tokio::sync::oneshot::channel();
        let worker = std::thread::Builder::new()
            .name("camera-capture".into())
            .spawn(move || {
                let alive = Arc::new(Mutex::new(std::time::Instant::now()));
                let opened = (|| {
                    let apartment = Apartment::new()?;
                    let devices = ffi::camera_devices();
                    let selected = match device {
                        Some(id) => devices.into_iter().find(|d| d.id == id),
                        None => devices.into_iter().next(),
                    }
                    .ok_or_else(|| {
                        "No available camera. Connect one or check camera permissions.".to_string()
                    })?;
                    let capture = ffi::start_camera_capture(
                        &selected.id,
                        quality.width(),
                        quality.height,
                        quality.fps,
                        Box::new(CameraSinkWrapper::new(Box::new(Sink {
                            source,
                            preview,
                            first: Mutex::new(Some(first_tx)),
                            alive: alive.clone(),
                        }))),
                    )
                    .map_err(|e| format!("opening the camera failed: {e}"))?;
                    Ok::<_, String>((
                        apartment,
                        capture,
                        CameraDevice {
                            id: selected.id,
                            label: selected.label,
                        },
                    ))
                })();
                match opened {
                    Ok((apartment, capture, device)) => {
                        if ready_tx.send(Ok(device)).is_ok() {
                            loop {
                                match stopped.recv_timeout(Duration::from_millis(200)) {
                                    Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                                }
                                if alive
                                    .lock()
                                    .is_ok_and(|last| last.elapsed() > Duration::from_secs(10))
                                {
                                    if fatal
                                        .send("The camera stopped delivering frames.".into())
                                        .is_err()
                                    {
                                        tracing::debug!("camera failure after teardown");
                                    }
                                    break;
                                }
                            }
                        }
                        drop(capture);
                        drop(apartment);
                    }
                    Err(e) => {
                        if ready_tx.send(Err(e)).is_err() {
                            tracing::debug!("camera start was cancelled");
                        }
                    }
                }
            })
            .map_err(|e| format!("camera worker failed: {e}"))?;
        let mut capture = Capture {
            stop,
            worker: Some(worker),
            device: CameraDevice {
                id: String::new(),
                label: String::new(),
            },
        };
        let result = async {
            capture.device = ready_rx
                .await
                .map_err(|_| "camera worker stopped".to_string())??;
            first_rx
                .await
                .map_err(|_| "camera produced no frames".to_string())
        };
        match tokio::time::timeout(Duration::from_secs(12), result).await {
            Ok(Ok(())) => Ok(capture),
            result => {
                capture.stop().await;
                Err(match result {
                    Ok(Err(e)) => e,
                    _ => "camera startup timed out".into(),
                })
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::syscamera::preview::{PreviewCommand, preview_controller, preview_image};

        #[test]
        fn preview_scales_frames_and_bounds_the_capture_queue() {
            use base64::Engine;
            let (tx, mut rx) = tokio::sync::mpsc::channel(1);
            let sink = Sink {
                source: None,
                preview: Some(tx),
                first: Mutex::new(None),
                alive: Arc::new(Mutex::new(std::time::Instant::now())),
            };
            let y = vec![81; 1280 * 720];
            let u = vec![90; 640 * 360];
            let v = vec![240; 640 * 360];
            for _ in 0..3 {
                sink.on_frame(1280, 720, (1280, 640, 640), (&y, &u, &v), 0);
            }
            let frame = rx.try_recv().unwrap();
            assert!(rx.try_recv().is_err());
            let encoded = preview_image(frame).unwrap();
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded.strip_prefix("data:image/jpeg;base64,").unwrap())
                .unwrap();
            let image = image::load_from_memory(&bytes).unwrap().to_rgb8();
            assert_eq!(image.dimensions(), (640, 360));
            let pixel = image.get_pixel(320, 180).0;
            assert!(pixel[0] > 240 && pixel[1] < 15 && pixel[2] < 15);
        }

        #[tokio::test]
        async fn closing_idle_preview_acknowledges_release_and_exits() {
            let (commands, mut frames) = preview_controller();
            let (done, stopped) = tokio::sync::oneshot::channel();
            commands.send(PreviewCommand::Stop(done)).unwrap();
            stopped.await.unwrap();
            drop(commands);
            assert!(
                tokio::time::timeout(Duration::from_secs(2), frames.changed())
                    .await
                    .unwrap()
                    .is_err()
            );
        }

        #[test]
        fn padded_odd_camera_planes_are_copied_without_padding() {
            let frame = copy_frame(
                3,
                3,
                (5, 3, 3),
                (
                    &[1, 2, 3, 99, 99, 4, 5, 6, 99, 99, 7, 8, 9, 99, 99],
                    &[10, 11, 99, 12, 13, 99],
                    &[14, 15, 99, 16, 17, 99],
                ),
            )
            .unwrap();
            let (sy, su, sv) = frame.strides();
            let (y, u, v) = frame.data();
            assert_eq!(&y[sy as usize..sy as usize + 3], &[4, 5, 6]);
            assert_eq!(&u[su as usize..su as usize + 2], &[12, 13]);
            assert_eq!(&v[sv as usize..sv as usize + 2], &[16, 17]);
            assert!(copy_frame(3, 3, (2, 3, 3), (&[0; 15], &[0; 6], &[0; 6])).is_none());
            assert!(copy_frame(3, 3, (5, 3, 3), (&[0; 14], &[0; 6], &[0; 6])).is_none());
            assert!(copy_frame(8192, 8192, (8192, 4096, 4096), (&[], &[], &[])).is_none());
        }

        #[tokio::test]
        async fn missing_native_camera_fails_and_releases_worker() {
            let available = tokio::task::spawn_blocking(devices).await.unwrap().unwrap();
            eprintln!("Native cameras: {available:?}");
            let source = NativeVideoSource::new(
                livekit::webrtc::video_source::VideoResolution {
                    width: 1280,
                    height: 720,
                },
                false,
            );
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
            assert!(
                start(
                    Some("discordia-nonexistent-camera".into()),
                    source,
                    Default::default(),
                    tx
                )
                .await
                .is_err()
            );
        }
    }
}
