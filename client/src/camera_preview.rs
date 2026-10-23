use crate::camera_quality::CameraQuality;
use livekit::webrtc::video_frame::native::VideoFrameBufferExt;
use livekit::webrtc::video_frame::{I420Buffer, VideoBuffer, VideoRotation};
use std::time::Duration;

pub(super) struct PreviewBuffer {
    pub buffer: I420Buffer,
    pub rotation: VideoRotation,
}

impl PreviewBuffer {
    fn dimensions(&self) -> (u32, u32) {
        let (width, height) = (self.buffer.width(), self.buffer.height());
        match self.rotation {
            VideoRotation::VideoRotation90 | VideoRotation::VideoRotation270 => (height, width),
            _ => (width, height),
        }
    }
}

pub enum PreviewCommand {
    Configure {
        revision: u64,
        device: Option<String>,
        quality: CameraQuality,
    },
    Stop(tokio::sync::oneshot::Sender<()>),
}

#[derive(Clone, Default)]
pub struct PreviewFrame {
    pub revision: u64,
    pub image: Option<String>,
    pub error: Option<String>,
    pub dimensions: Option<(u32, u32)>,
}

pub fn preview_controller() -> (
    tokio::sync::mpsc::UnboundedSender<PreviewCommand>,
    tokio::sync::watch::Receiver<PreviewFrame>,
) {
    let (commands, mut requests) = tokio::sync::mpsc::unbounded_channel();
    let (frames, receiver) = tokio::sync::watch::channel(PreviewFrame::default());
    tokio::spawn(async move {
        let mut next = requests.recv().await;
        while let Some(mut command) = next {
            while let Ok(newer) = requests.try_recv() {
                if let PreviewCommand::Stop(done) = command {
                    let _ = done.send(());
                }
                command = newer;
            }
            if requests.is_closed() {
                break;
            }
            let PreviewCommand::Configure {
                revision,
                device,
                quality,
            } = command
            else {
                if let PreviewCommand::Stop(done) = command {
                    let _ = done.send(());
                }
                next = requests.recv().await;
                continue;
            };
            let quality = quality.normalized();
            frames.send_replace(PreviewFrame {
                revision,
                ..Default::default()
            });
            let (raw_frames, mut stream) = tokio::sync::mpsc::channel(1);
            let (fatal_tx, mut fatal_rx) = tokio::sync::mpsc::unbounded_channel();
            match super::start_preview(device, quality, raw_frames, fatal_tx).await {
                Err(error) => {
                    frames.send_replace(PreviewFrame {
                        revision,
                        error: Some(error),
                        ..Default::default()
                    });
                    next = requests.recv().await;
                }
                Ok(capture) => {
                    let mut last = std::time::Instant::now() - Duration::from_secs(1);
                    let mut failed = false;
                    next = loop {
                        tokio::select! {
                            biased;
                            command = requests.recv() => break command,
                            Some(error) = fatal_rx.recv() => {
                                frames.send_replace(PreviewFrame { revision, error: Some(error), ..Default::default() });
                                failed = true;
                                break None;
                            }
                            frame = tokio::time::timeout(Duration::from_secs(12), stream.recv()) => {
                                let frame = frame.ok().flatten();
                                let Some(frame) = frame else {
                                    frames.send_replace(PreviewFrame { revision, error: Some("Camera preview stopped".into()), ..Default::default() });
                                    failed = true;
                                    break None;
                                };
                                if last.elapsed() < Duration::from_millis(if cfg!(target_os = "android") { 200 } else { 125 }) { continue; }
                                last = std::time::Instant::now();
                                let dimensions = frame.dimensions();
                                let encoded = tokio::task::spawn_blocking(move || preview_image(frame)).await;
                                match encoded {
                                    Ok(Ok(image)) => { frames.send_replace(PreviewFrame { revision, image: Some(image), dimensions: Some(dimensions), error: None }); }
                                    result => { frames.send_replace(PreviewFrame { revision, error: Some(format!("Camera preview failed: {result:?}")), ..Default::default() }); }
                                }
                            }
                        }
                    };
                    // Release the device before opening another mode or publishing it.
                    capture.stop().await;
                    if failed {
                        next = requests.recv().await;
                    }
                }
            }
        }
    });
    (commands, receiver)
}

pub(super) fn preview_image(frame: PreviewBuffer) -> Result<String, String> {
    use base64::Engine;
    use livekit::webrtc::video_frame::VideoFormatType;
    let mut buffer = frame.buffer;
    let (max_width, max_height) = if cfg!(target_os = "android") {
        (480.0, 270.0)
    } else {
        (640.0, 360.0)
    };
    let scale = (max_width / f64::from(buffer.width()))
        .min(max_height / f64::from(buffer.height()))
        .min(1.0);
    let width = (f64::from(buffer.width()) * scale).max(1.0) as u32;
    let height = (f64::from(buffer.height()) * scale).max(1.0) as u32;
    let buffer = buffer.scale(width as i32, height as i32);
    let mut pixels = vec![0; (width * height * 4) as usize];
    buffer.to_argb(
        VideoFormatType::ABGR,
        &mut pixels,
        width * 4,
        width as i32,
        height as i32,
    );
    let image =
        image::RgbaImage::from_raw(width, height, pixels).ok_or("Invalid camera preview")?;
    let image = match frame.rotation {
        VideoRotation::VideoRotation90 => image::imageops::rotate90(&image),
        VideoRotation::VideoRotation180 => image::imageops::rotate180(&image),
        VideoRotation::VideoRotation270 => image::imageops::rotate270(&image),
        _ => image,
    };
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 75)
        .encode_image(&image::DynamicImage::ImageRgba8(image))
        .map_err(|e| e.to_string())?;
    Ok(format!(
        "data:image/jpeg;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(jpeg)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    #[test]
    fn portrait_preview_rotates_after_bounded_conversion() {
        let (wide, short) = if cfg!(target_os = "android") {
            (480, 270)
        } else {
            (640, 360)
        };
        for (rotation, expected) in [
            (VideoRotation::VideoRotation90, (short, wide)),
            (VideoRotation::VideoRotation270, (short, wide)),
            (VideoRotation::VideoRotation180, (wide, short)),
        ] {
            let frame = PreviewBuffer {
                buffer: I420Buffer::new(1280, 720),
                rotation,
            };
            assert_eq!(
                frame.dimensions(),
                if expected.0 < expected.1 {
                    (720, 1280)
                } else {
                    (1280, 720)
                }
            );
            let encoded = preview_image(frame).expect("encode preview");
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(
                    encoded
                        .strip_prefix("data:image/jpeg;base64,")
                        .expect("JPEG URL"),
                )
                .expect("base64");
            let image = image::load_from_memory(&bytes).expect("decode JPEG");
            assert_eq!((image.width(), image.height()), expected);
        }
    }
}
