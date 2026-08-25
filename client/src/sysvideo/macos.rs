use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AllocAnyThread, DefinedClass, define_class, msg_send};
use objc2_core_media::{CMSampleBuffer, CMTime};
use objc2_core_video::kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange;
use objc2_foundation::{NSArray, NSError};
use objc2_screen_capture_kit::{
    SCContentFilter, SCShareableContent, SCStream, SCStreamConfiguration, SCStreamDelegate,
    SCStreamOutput, SCStreamOutputType,
};
use tokio::sync::mpsc::UnboundedSender;

use super::{Frame, FrameSink, Settings, Source, Target};

const CONTENT_TIMEOUT: Duration = Duration::from_secs(20);

struct TapState {
    sink: FrameSink,
    fatal: UnboundedSender<String>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "DxfScreenVideoTap"]
    #[ivars = TapState]
    struct Tap;

    unsafe impl NSObjectProtocol for Tap {}

    unsafe impl SCStreamOutput for Tap {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        unsafe fn stream_did_output(
            &self,
            _stream: &SCStream,
            sample: &CMSampleBuffer,
            kind: SCStreamOutputType,
        ) {
            if kind != SCStreamOutputType::Screen {
                return;
            }
            self.handle_frame(sample);
        }
    }

    unsafe impl SCStreamDelegate for Tap {
        #[unsafe(method(stream:didStopWithError:))]
        unsafe fn stream_did_stop(&self, _stream: &SCStream, error: &NSError) {
            let _ = self
                .ivars()
                .fatal
                .send(error.localizedDescription().to_string());
        }
    }
);

impl Tap {
    fn handle_frame(&self, sample: &CMSampleBuffer) {
        let Some(buffer) = (unsafe { sample.image_buffer() }) else {
            return;
        };

        if objc2_core_video::CVPixelBufferGetWidth(&buffer) == 0
            || objc2_core_video::CVPixelBufferGetHeight(&buffer) == 0
        {
            return;
        }

        (self.ivars().sink)(Frame { buffer });
        super::FRAMES_CAPTURED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

pub struct MacVideoCapture {
    stream: Retained<SCStream>,
}

impl MacVideoCapture {
    pub fn start(
        target: Target,
        settings: Settings,
        sink: FrameSink,
        fatal: UnboundedSender<String>,
    ) -> Result<Self, String> {
        let filter = content_filter(target)?;

        unsafe {
            let config = SCStreamConfiguration::new();
            config.setWidth(settings.width as usize);
            config.setHeight(settings.height as usize);
            config.setPixelFormat(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange);
            config.setMinimumFrameInterval(CMTime {
                value: 1,
                timescale: settings.fps.max(1) as i32,
                flags: objc2_core_media::CMTimeFlags::Valid,
                epoch: 0,
            });
            config.setShowsCursor(true);
            config.setQueueDepth(3);

            let tap = Tap::alloc().set_ivars(TapState { sink, fatal });
            let tap: Retained<Tap> = msg_send![super(tap), init];
            let output = ProtocolObject::from_ref(&*tap);
            let delegate = ProtocolObject::from_ref(&*tap);

            let stream: Retained<SCStream> = SCStream::initWithFilter_configuration_delegate(
                SCStream::alloc(),
                &filter,
                &config,
                Some(delegate),
            );

            let queue = dispatch2::DispatchQueue::new("fun.dioxus.sysvideo", None);
            stream
                .addStreamOutput_type_sampleHandlerQueue_error(
                    output,
                    SCStreamOutputType::Screen,
                    Some(&queue),
                )
                .map_err(|e| format!("add screen output: {e}"))?;

            std::mem::forget(tap);

            let (done_tx, done_rx) = std_mpsc::channel();
            let handler = RcBlock::new(move |err: *mut NSError| {
                let msg = if err.is_null() {
                    None
                } else {
                    Some((*err).localizedDescription().to_string())
                };
                let _ = done_tx.send(msg);
            });
            stream.startCaptureWithCompletionHandler(Some(&handler));
            match done_rx.recv_timeout(CONTENT_TIMEOUT) {
                Ok(None) => {}
                Ok(Some(e)) => return Err(format!("start capture: {e}")),
                Err(_) => return Err("timed out starting screen capture".into()),
            }

            Ok(Self { stream })
        }
    }
}

impl Drop for MacVideoCapture {
    fn drop(&mut self) {
        unsafe {
            self.stream.stopCaptureWithCompletionHandler(None);
        }
    }
}

fn shareable_content() -> Result<Retained<SCShareableContent>, String> {
    let (tx, rx) = std_mpsc::channel();
    let handler = RcBlock::new(move |content: *mut SCShareableContent, err: *mut NSError| {
        if !err.is_null() {
            let msg = unsafe { (*err).localizedDescription() }.to_string();
            let _ = tx.send(Err(msg));
            return;
        }
        if content.is_null() {
            let _ = tx.send(Err("no shareable content returned".into()));
            return;
        }
        let _ = tx.send(Ok(unsafe { Retained::retain(content) }.unwrap()));
    });
    unsafe {
        SCShareableContent::getShareableContentWithCompletionHandler(&handler);
    }
    match rx.recv_timeout(CONTENT_TIMEOUT) {
        Ok(r) => r,
        Err(_) => Err("timed out querying shareable content (screen recording permission?)".into()),
    }
}

pub(super) fn content_filter(target: Target) -> Result<Retained<SCContentFilter>, String> {
    let content = shareable_content()?;
    filter_for(&content, target)
}

fn is_pickable(w: &objc2_screen_capture_kit::SCWindow) -> bool {
    unsafe {
        if !w.isOnScreen() || w.windowLayer() != 0 {
            return false;
        }
        if w.title()
            .map(|t| t.to_string())
            .unwrap_or_default()
            .trim()
            .is_empty()
        {
            return false;
        }
        let f = w.frame();
        f.size.width >= 64.0 && f.size.height >= 64.0
    }
}

pub fn sources() -> Result<Vec<Source>, String> {
    let content = shareable_content()?;
    let mut out = Vec::new();

    unsafe {
        let displays = content.displays();
        let multi = displays.len() > 1;
        for (i, d) in displays.iter().enumerate() {
            out.push(Source {
                target: Target::Display(d.displayID()),
                title: if multi {
                    format!("Screen {}", i + 1)
                } else {
                    "Entire screen".to_string()
                },
                app: None,
                width: d.width().max(0) as u32,
                height: d.height().max(0) as u32,
            });
        }

        let windows: Vec<_> = content
            .windows()
            .iter()
            .filter(|w| is_pickable(w))
            .collect();

        let mut seen: std::collections::HashMap<i32, (String, usize)> =
            std::collections::HashMap::new();
        for w in &windows {
            if let Some(app) = w.owningApplication() {
                let e = seen
                    .entry(app.processID())
                    .or_insert_with(|| (app.applicationName().to_string(), 0));
                e.1 += 1;
            }
        }
        let mut apps: Vec<_> = seen
            .into_iter()
            .filter(|(_, (_, n))| *n > 1)
            .map(|(pid, (name, n))| (pid, name, n))
            .collect();
        apps.sort_by_key(|(_, name, _)| name.to_lowercase());
        for (pid, name, n) in apps {
            out.push(Source {
                target: Target::Application(pid),
                title: format!("{name} — all {n} windows"),
                app: None,
                width: 0,
                height: 0,
            });
        }

        let mut wins: Vec<Source> = windows
            .iter()
            .map(|w| {
                let f = w.frame();
                Source {
                    target: Target::Window(w.windowID()),
                    title: w.title().map(|t| t.to_string()).unwrap_or_default(),
                    app: w
                        .owningApplication()
                        .map(|a| a.applicationName().to_string()),
                    width: f.size.width.max(0.0) as u32,
                    height: f.size.height.max(0.0) as u32,
                }
            })
            .collect();
        wins.sort_by(|a, b| {
            let ka = a.app.as_deref().unwrap_or("").to_lowercase();
            let kb = b.app.as_deref().unwrap_or("").to_lowercase();
            ka.cmp(&kb)
                .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
        });
        out.extend(wins);
    }

    Ok(out)
}

fn filter_for(
    content: &SCShareableContent,
    target: Target,
) -> Result<Retained<SCContentFilter>, String> {
    unsafe {
        let empty: Retained<NSArray<objc2_screen_capture_kit::SCWindow>> = NSArray::new();
        match target {
            Target::Display(id) => {
                let display = content
                    .displays()
                    .iter()
                    .find(|d| d.displayID() == id)
                    .ok_or("that screen is no longer connected")?;
                Ok(SCContentFilter::initWithDisplay_excludingWindows(
                    SCContentFilter::alloc(),
                    &display,
                    &empty,
                ))
            }
            Target::Window(id) => {
                let window = content
                    .windows()
                    .iter()
                    .find(|w| w.windowID() == id)
                    .ok_or("that window has closed")?;
                Ok(SCContentFilter::initWithDesktopIndependentWindow(
                    SCContentFilter::alloc(),
                    &window,
                ))
            }
            Target::Application(pid) => {
                let window = content
                    .windows()
                    .iter()
                    .find(|w| {
                        is_pickable(w)
                            && w.owningApplication().is_some_and(|a| a.processID() == pid)
                    })
                    .ok_or("that app has no shareable window open any more")?;
                let app = window
                    .owningApplication()
                    .ok_or("that app is no longer running")?;
                let wf = window.frame();
                let display = content
                    .displays()
                    .iter()
                    .find(|d| {
                        let f = d.frame();
                        wf.origin.x >= f.origin.x
                            && wf.origin.x < f.origin.x + f.size.width
                            && wf.origin.y >= f.origin.y
                            && wf.origin.y < f.origin.y + f.size.height
                    })
                    .or_else(|| content.displays().firstObject())
                    .ok_or("no displays available")?;
                let apps = NSArray::from_retained_slice(&[app]);
                Ok(
                    SCContentFilter::initWithDisplay_includingApplications_exceptingWindows(
                        SCContentFilter::alloc(),
                        &display,
                        &apps,
                        &empty,
                    ),
                )
            }
        }
    }
}
