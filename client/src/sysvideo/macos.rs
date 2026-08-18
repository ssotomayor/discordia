//! macOS screen capture via ScreenCaptureKit (macOS 13+).
//!
//! Same binding choice as `sysaudio::macos` — `objc2` rather than the
//! `screencapturekit` crate, so nothing Swift has to be shipped alongside the
//! binary — and the same permission story: this needs the Screen Recording
//! (TCC) grant, and a missing grant surfaces as a failed shareable-content
//! query rather than a hang.
//!
//! This runs a SCStream of its own rather than adding a video output to the one
//! `sysaudio` already has. Two reasons: the lifecycles genuinely differ (system
//! audio is a per-share setting the user can turn off, video is the share
//! itself), and `sysaudio`'s stream is configured 2x2 with
//! `excludesCurrentProcessAudio` for a job this one must not inherit. The cost
//! is one extra capture pipeline whose video is two pixels wide.

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

/// How long to wait for the shareable-content query. It is asynchronous and,
/// when the permission dialog is showing, can take as long as the user does.
const CONTENT_TIMEOUT: Duration = Duration::from_secs(20);

/// Per-callback state for the stream output delegate.
struct TapState {
    sink: FrameSink,
    /// Reports a stream that stopped on its own. `UnboundedSender` so the
    /// callback never blocks the capture thread.
    fatal: UnboundedSender<String>,
}

define_class!(
    // ScreenCaptureKit calls this back on its own dispatch queue.
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
    /// Hand one sample buffer's image buffer to the sink.
    fn handle_frame(&self, sample: &CMSampleBuffer) {
        // A sample buffer with no image buffer is normal, not an error:
        // ScreenCaptureKit emits frames whose only payload is a status change
        // (e.g. `.idle` when nothing on screen moved). There is nothing to
        // publish for those, and publishing the previous frame again would just
        // spend bitrate saying the same thing.
        //
        // SAFETY: reading the sample buffer's image buffer, which
        // `CFRetained::retain` takes its own reference to — so it stays alive
        // for as long as the `Frame` below does, independent of the callback.
        let Some(buffer) = (unsafe { sample.image_buffer() }) else {
            return;
        };

        // Dimensions come from the buffer, not config, because display-mode
        // changes (resolution switch, unplug) move them under us.
        if objc2_core_video::CVPixelBufferGetWidth(&buffer) == 0
            || objc2_core_video::CVPixelBufferGetHeight(&buffer) == 0
        {
            return;
        }

        (self.ivars().sink)(Frame { buffer });
        // After the sink, so the count means "frames that reached the
        // encoder".
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

        // SAFETY: plain ObjC object construction with checked arguments.
        unsafe {
            let config = SCStreamConfiguration::new();
            config.setWidth(settings.width as usize);
            config.setHeight(settings.height as usize);
            // NV12 avoids a per-frame color conversion; both VideoToolbox and
            // libwebrtc accept it directly.
            config.setPixelFormat(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange);
            // A ceiling, not a target: ScreenCaptureKit only emits on screen
            // change, so a still screen costs nothing.
            config.setMinimumFrameInterval(CMTime {
                value: 1,
                timescale: settings.fps.max(1) as i32,
                flags: objc2_core_media::CMTimeFlags::Valid,
                epoch: 0,
            });
            // The cursor is part of what someone is trying to show you.
            config.setShowsCursor(true);
            // Frames are consumed inline; a deep queue only adds latency to a
            // stream whose value is being current.
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

            // Leak the delegate: ScreenCaptureKit holds it weakly, so dropping
            // it would leave the stream calling into freed memory.
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
        // SAFETY: stopping a running stream; the completion handler is
        // optional and we don't need to wait for it on teardown.
        unsafe {
            self.stream.stopCaptureWithCompletionHandler(None);
        }
    }
}

/// Everything shareable right now, via the asynchronous shareable-content query.
///
/// This is also where a missing Screen Recording permission surfaces: the query
/// fails rather than returning an empty list, so the error reaches the user
/// instead of looking like a machine with no screens.
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
        // SAFETY: non-null for the callback's duration; `retain` takes our own
        // reference so it outlives the callback.
        let _ = tx.send(Ok(unsafe { Retained::retain(content) }.unwrap()));
    });
    // SAFETY: the handler outlives the call via RcBlock.
    unsafe {
        SCShareableContent::getShareableContentWithCompletionHandler(&handler);
    }
    match rx.recv_timeout(CONTENT_TIMEOUT) {
        Ok(r) => r,
        Err(_) => Err("timed out querying shareable content (screen recording permission?)".into()),
    }
}

/// Build the filter for a fresh view of the selected target.
///
/// System audio calls this too. A separate implementation there used to pick
/// the first display regardless of the selected video target, so sharing one
/// app/window could send an unrelated machine-wide mix.
pub(super) fn content_filter(target: Target) -> Result<Retained<SCContentFilter>, String> {
    let content = shareable_content()?;
    filter_for(&content, target)
}

/// Windows worth offering in a picker.
///
/// The raw `windows` list is not a list of windows a person would recognise: it
/// carries the menu bar, the Dock, wallpaper layers, notification overlays and
/// every invisible helper window each running app keeps around. Unfiltered, a
/// picker shows a couple of hundred entries, most of them untitled. The four
/// tests below are what every screen-share picker applies:
///
/// - `isOnScreen` — not minimised or on another Space.
/// - `windowLayer == 0` — normal windows only. The Dock, menu bar and overlays
///   live on higher layers, and sharing one of those is never the intent.
/// - a non-empty title — an untitled window cannot be labelled, so it cannot be
///   chosen meaningfully.
/// - at least 64x64 — filters out the one-pixel and off-screen scratch windows
///   that toolkits keep for hit-testing.
fn is_pickable(w: &objc2_screen_capture_kit::SCWindow) -> bool {
    // SAFETY: plain property reads on a live SCWindow.
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

/// Displays, then applications, then individual windows.
///
/// That order is the answer to "what do people actually reach for": a whole
/// screen most of the time, a specific window next, an app when it has several
/// windows they all want shown. Applications are derived from the pickable
/// windows rather than from `content.applications()`, which lists every running
/// process — including hundreds with no window to share.
pub fn sources() -> Result<Vec<Source>, String> {
    let content = shareable_content()?;
    let mut out = Vec::new();

    // SAFETY: plain property reads on live SCShareableContent members.
    unsafe {
        let displays = content.displays();
        let multi = displays.len() > 1;
        for (i, d) in displays.iter().enumerate() {
            out.push(Source {
                target: Target::Display(d.displayID()),
                // SCDisplay carries no name, so it is numbered only when there
                // is more than one to tell apart.
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

        // One entry per app with >1 shareable window; with a single window,
        // app and window entries would capture the same thing under two names.
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
        // Grouped by app then title for stability; an unstable picker moves
        // the entry out from under the cursor.
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

/// Resolve a `Target` against freshly queried content and build its filter.
///
/// Re-resolved rather than holding the OS object from the pick: a window can
/// close, an app can quit, and a display can be unplugged between choosing and
/// starting. Failing here with a plain reason beats capturing a stale object.
fn filter_for(
    content: &SCShareableContent,
    target: Target,
) -> Result<Retained<SCContentFilter>, String> {
    // SAFETY: plain property reads and ObjC construction on live objects.
    unsafe {
        let empty: Retained<NSArray<objc2_screen_capture_kit::SCWindow>> = NSArray::new();
        match target {
            Target::Display(id) => {
                let display = content
                    .displays()
                    .iter()
                    .find(|d| d.displayID() == id)
                    .ok_or("that screen is no longer connected")?;
                // Nothing excluded: "entire screen" means the screen as it is.
                // The recursive-mirror artifact that argues for excluding
                // ourselves needs the sharer's window showing the stream,
                // which never happens here (no self-preview).
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
                // The window wherever it is, independent of display and
                // stacking order.
                Ok(SCContentFilter::initWithDesktopIndependentWindow(
                    SCContentFilter::alloc(),
                    &window,
                ))
            }
            Target::Application(pid) => {
                // An application filter is still scoped to a display, so it
                // needs one: the display the app's first shareable window is
                // on, which is where the user is looking at it.
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
                // Use top-left origin to determine display: a window
                // straddling screens belongs to the one its origin is on,
                // matching macOS behavior.
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
