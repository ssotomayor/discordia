pub fn supported() -> bool {
    cfg!(target_os = "windows")
}

#[cfg(target_os = "windows")]
pub use windows::{Capture, devices, start};

#[cfg(target_os = "windows")]
mod windows {
    use std::sync::{Arc, Mutex, mpsc};
    use std::time::Duration;

    use livekit::webrtc::video_frame::{I420Buffer, VideoFrame, VideoRotation};
    use livekit::webrtc::video_source::native::NativeVideoSource;
    use webrtc_sys::camera_capture::{CameraSink, CameraSinkWrapper, ffi};
    use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};

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
        source: NativeVideoSource,
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
            self.source.capture_frame(&VideoFrame {
                rotation: VideoRotation::VideoRotation0,
                timestamp_us,
                frame_metadata: None,
                buffer,
            });
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
        fatal: tokio::sync::mpsc::UnboundedSender<String>,
    ) -> Result<Capture, String> {
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
                        1280,
                        720,
                        30,
                        Box::new(CameraSinkWrapper::new(Box::new(Sink {
                            source,
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
                start(Some("discordia-nonexistent-camera".into()), source, tx)
                    .await
                    .is_err()
            );
        }
    }
}
