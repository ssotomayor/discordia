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
}

pub struct AndroidVideoCapture {
    slot: u32,
    pub(super) metrics: Arc<Metrics>,
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
        let slot = crate::android::next_slot();
        let metrics = Arc::new(Metrics::default());
        let recorded = metrics.clone();
        let (first_tx, first_rx) = tokio::sync::oneshot::channel();
        let state = parking_lot::Mutex::new(Pacing {
            first: Some(first_tx),
            pacer: FramePacer::new(settings.fps),
            previous: None,
        });
        let sink: FrameSink = Box::new(move |frame: Frame| {
            let now = Instant::now();
            let (accepted, gap, first) = {
                let mut state = state.lock();
                let accepted = state.pacer.accept(now);
                let gap = state
                    .previous
                    .map_or(Duration::ZERO, |previous| now.duration_since(previous));
                state.previous = Some(now);
                let first = accepted.then(|| state.first.take()).flatten();
                (accepted, gap, first)
            };
            recorded.callback(gap, !accepted);
            if let Some(tx) = first {
                let _ = tx.send(());
            }
            if accepted {
                recorded.record(frame.buffer.width(), frame.buffer.height(), Duration::ZERO);
                sink(frame);
            }
        });
        crate::android::register_frame_sink(slot, Arc::from(sink), fatal);
        let result =
            crate::android::start_screen_share(slot, settings.width, settings.height, settings.fps)
                .await;
        if let Err(error) = result {
            crate::android::unregister_frame_sink(slot);
            return Err(error);
        }
        match tokio::time::timeout(Duration::from_secs(20), first_rx).await {
            Ok(Ok(())) => Ok(Self { slot, metrics }),
            _ => {
                crate::android::unregister_frame_sink(slot);
                crate::android::stop_screen_share();
                Err("the screen capture produced no frames".into())
            }
        }
    }
}

impl Drop for AndroidVideoCapture {
    fn drop(&mut self) {
        crate::android::unregister_frame_sink(self.slot);
        crate::android::stop_screen_share();
    }
}
