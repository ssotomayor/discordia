use std::sync::Arc;
use std::time::{Duration, Instant};

use livekit::webrtc::video_frame::VideoBuffer;

use super::metrics::Metrics;
use super::pacing::FramePacer;
use super::{Frame, FrameSink, Settings, Source, Target};

pub fn sources() -> Result<Vec<Source>, String> {
    let (width, height) = crate::android::display_size()?;
    Ok(vec![Source {
        target: Target::AndroidScreen,
        title: "Entire screen".into(),
        app: None,
        width,
        height,
    }])
}

/// A MediaProjection virtual display refreshes at the screen's rate, so the
/// capturer hands over every one of those frames whatever was asked for; the
/// pacer is what makes the requested rate true.
struct Pacing {
    first: Option<tokio::sync::oneshot::Sender<()>>,
    pacer: FramePacer,
    previous: Option<Instant>,
    accepted_at: Option<Instant>,
}

#[derive(Clone)]
pub(crate) struct CaptureControl {
    slot: u32,
    pacing: Arc<parking_lot::Mutex<Pacing>>,
}

impl CaptureControl {
    pub async fn apply(&self, settings: Settings) -> Result<(), String> {
        crate::android::resize_screen_share(self.slot, settings).await?;
        self.pacing.lock().pacer.set_fps(settings.fps);
        Ok(())
    }
}

pub struct AndroidVideoCapture {
    slot: u32,
    pub(super) metrics: Arc<Metrics>,
    pub(super) control: CaptureControl,
}

impl AndroidVideoCapture {
    pub async fn start(
        target: Target,
        settings: Settings,
        sink: FrameSink,
        fatal: tokio::sync::mpsc::UnboundedSender<String>,
    ) -> Result<Self, String> {
        if target != Target::AndroidScreen {
            return Err("screen capture isn't implemented for this source".into());
        }
        let settings =
            super::mobile_budget::MobileBudget::warm_start(settings, Instant::now()).current();
        let slot = crate::android::next_slot();
        let metrics = Arc::new(Metrics::default());
        let recorded = metrics.clone();
        let (first_tx, first_rx) = tokio::sync::oneshot::channel();
        let state = Arc::new(parking_lot::Mutex::new(Pacing {
            first: Some(first_tx),
            pacer: FramePacer::new(settings.fps),
            previous: None,
            accepted_at: None,
        }));
        let admission = state.clone();
        let callbacks = metrics.clone();
        let filter = Arc::new(move || {
            let now = Instant::now();
            let mut state = admission.lock();
            let accepted = state.pacer.accept(now);
            let gap = state
                .previous
                .map_or(Duration::ZERO, |previous| now.duration_since(previous));
            state.previous = Some(now);
            if accepted {
                state.accepted_at = Some(now);
            }
            callbacks.callback(gap, !accepted);
            accepted
        });
        let control = CaptureControl {
            slot,
            pacing: state.clone(),
        };
        let sink: FrameSink = Box::new(move |frame: Frame| {
            let (elapsed, first) = {
                let mut state = state.lock();
                (
                    state
                        .accepted_at
                        .take()
                        .map_or(Duration::ZERO, |at| at.elapsed()),
                    state.first.take(),
                )
            };
            if let Some(tx) = first {
                let _ = tx.send(());
            }
            recorded.record_android(
                frame.buffer.width(),
                frame.buffer.height(),
                elapsed,
                frame.conversion_time,
                frame.copy_time,
            );
            sink(frame);
        });
        crate::android::register_frame_sink(slot, Arc::from(sink), fatal);
        crate::android::filter_capture_frames(slot, filter);
        let capture = Self {
            slot,
            metrics,
            control,
        };
        crate::android::start_screen_share(slot, settings.width, settings.height, settings.fps)
            .await?;
        match tokio::time::timeout(Duration::from_secs(20), first_rx).await {
            Ok(Ok(())) => Ok(capture),
            _ => Err("the screen capture produced no frames".into()),
        }
    }
}

impl Drop for AndroidVideoCapture {
    fn drop(&mut self) {
        crate::android::unregister_frame_sink(self.slot);
        crate::android::stop_screen_share(self.slot);
    }
}
