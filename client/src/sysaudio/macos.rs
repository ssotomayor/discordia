//! macOS system-audio capture via ScreenCaptureKit (macOS 13+).
//!
//! Bound with `objc2` rather than the higher-level `screencapturekit` crate on
//! purpose: that one builds a Swift component, so the app would have to ship
//! the Swift runtime and link with an extra rpath. This is more code here and
//! nothing extra in the binary.
//!
//! Permission: ScreenCaptureKit needs the Screen Recording (TCC) grant — the
//! same one screen sharing already requires, so no second prompt appears. If it
//! hasn't been granted, `SCShareableContent` fails and we report that instead
//! of hanging.

use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass};
use objc2_core_audio_types::AudioBufferList;
use objc2_core_media::{CMBlockBuffer, CMSampleBuffer};
use objc2_foundation::{NSArray, NSError};
use objc2_screen_capture_kit::{
    SCContentFilter, SCDisplay, SCShareableContent, SCStream, SCStreamConfiguration, SCStreamOutput,
    SCStreamOutputType,
};
use tokio::sync::mpsc::UnboundedSender;

/// How long to wait for the shareable-content query. It is asynchronous and,
/// when the permission dialog is showing, can take as long as the user does.
const CONTENT_TIMEOUT: Duration = Duration::from_secs(20);

/// Per-callback state for the stream output delegate.
struct TapState {
    /// Accumulates callback buffers into whole 10 ms frames.
    cutter: std::cell::RefCell<super::frames::FrameCutter>,
}

define_class!(
    // ScreenCaptureKit calls this back on its own dispatch queue.
    #[unsafe(super(NSObject))]
    #[name = "DxfSystemAudioTap"]
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
            if kind != SCStreamOutputType::Audio {
                return;
            }
            self.handle_audio(sample);
        }
    }
);

impl Tap {
    /// Pull PCM out of a sample buffer and forward it as mono 10 ms frames.
    fn handle_audio(&self, sample: &CMSampleBuffer) {
        let state = self.ivars();
        // Zeroed rather than Default: AudioBufferList is a C struct with a
        // trailing variable-length array and implements neither.
        let mut list: AudioBufferList = unsafe { std::mem::zeroed() };
        let mut block: *mut CMBlockBuffer = std::ptr::null_mut();

        // SAFETY: `list` is sized as declared, and `block` receives a +1
        // reference that is released below.
        let status = unsafe {
            sample.audio_buffer_list_with_retained_block_buffer(
                std::ptr::null_mut(),
                std::ptr::from_mut(&mut list),
                std::mem::size_of::<AudioBufferList>(),
                None,
                None,
                0,
                std::ptr::from_mut(&mut block),
            )
        };
        if status != 0 {
            return;
        }

        // ScreenCaptureKit delivers non-interleaved f32: one buffer per
        // channel. Averaging them to mono is what the publish path wants, and
        // avoids sending twice the data for a stereo desktop mix.
        let n = list.mNumberBuffers as usize;
        let mut mono: Vec<f32> = Vec::new();
        for i in 0..n.min(8) {
            // SAFETY: mNumberBuffers reports how many entries are valid.
            let buf = unsafe { &*list.mBuffers.as_ptr().add(i) };
            if buf.mData.is_null() {
                continue;
            }
            let count = buf.mDataByteSize as usize / std::mem::size_of::<f32>();
            // SAFETY: mDataByteSize is the length of mData in bytes.
            let samples = unsafe { std::slice::from_raw_parts(buf.mData.cast::<f32>(), count) };
            if mono.is_empty() {
                mono.extend_from_slice(samples);
            } else {
                for (m, s) in mono.iter_mut().zip(samples) {
                    *m += *s;
                }
            }
        }
        if !mono.is_empty() && n > 1 {
            let scale = 1.0 / n as f32;
            for m in mono.iter_mut() {
                *m *= scale;
            }
        }

        if !block.is_null() {
            // SAFETY: the "RetainedBlockBuffer" variant hands us a +1
            // reference; wrapping it in CFRetained releases it on drop.
            unsafe {
                drop(objc2_core_foundation::CFRetained::from_raw(
                    std::ptr::NonNull::new_unchecked(block),
                ));
            }
        }
        if mono.is_empty() {
            return;
        }

        // Cut into exact frames; the remainder waits for the next callback.
        // The channels were already folded together above — ScreenCaptureKit
        // delivers one buffer per channel, not interleaved.
        state.cutter.borrow_mut().push_mono(&mono);
    }
}

pub struct MacCapture {
    stream: Retained<SCStream>,
}

impl MacCapture {
    pub fn start(tx: UnboundedSender<Vec<f32>>) -> Result<Self, String> {
        let display = first_display()?;

        // SAFETY: plain ObjC object construction with checked arguments.
        unsafe {
            let empty: Retained<NSArray<objc2_screen_capture_kit::SCWindow>> = NSArray::new();
            let filter: Retained<SCContentFilter> =
                SCContentFilter::initWithDisplay_excludingWindows(
                    SCContentFilter::alloc(),
                    &display,
                    &empty,
                );

            let config = SCStreamConfiguration::new();
            config.setCapturesAudio(true);
            config.setSampleRate(48_000);
            config.setChannelCount(2);
            // Without this we would capture our own output — including the
            // call we are publishing into — and feed it straight back as a
            // howling loop.
            config.setExcludesCurrentProcessAudio(true);
            // Video still has to be configured even though we only want audio;
            // keep it at the smallest sane size so nothing is encoded for free.
            config.setWidth(2);
            config.setHeight(2);

            let tap = Tap::alloc().set_ivars(TapState {
                cutter: std::cell::RefCell::new(super::frames::FrameCutter::new(tx)),
            });
            let tap: Retained<Tap> = msg_send![super(tap), init];
            let output = ProtocolObject::from_ref(&*tap);

            let stream: Retained<SCStream> = SCStream::initWithFilter_configuration_delegate(
                SCStream::alloc(),
                &filter,
                &config,
                None,
            );

            let queue = dispatch2::DispatchQueue::new("fun.dioxus.sysaudio", None);
            stream
                .addStreamOutput_type_sampleHandlerQueue_error(
                    output,
                    SCStreamOutputType::Audio,
                    Some(&queue),
                )
                .map_err(|e| format!("add audio output: {e}"))?;

            // Leak the delegate for the life of the stream: ScreenCaptureKit
            // holds it weakly, so dropping it here would leave the stream
            // calling into freed memory.
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
                Err(_) => return Err("timed out starting system audio capture".into()),
            }

            Ok(Self { stream })
        }
    }
}

impl Drop for MacCapture {
    fn drop(&mut self) {
        // SAFETY: stopping a running stream; the completion handler is
        // optional and we don't need to wait for it on teardown.
        unsafe {
            self.stream.stopCaptureWithCompletionHandler(None);
        }
    }
}

/// The first display, via the asynchronous shareable-content query.
///
/// This is also where a missing Screen Recording permission surfaces: the query
/// fails rather than returning an empty list, so the error reaches the user
/// instead of looking like a machine with no screens.
fn first_display() -> Result<Retained<SCDisplay>, String> {
    let (tx, rx) = std_mpsc::channel();
    let handler = RcBlock::new(
        move |content: *mut SCShareableContent, err: *mut NSError| {
            if !err.is_null() {
                let msg = unsafe { (*err).localizedDescription() }.to_string();
                let _ = tx.send(Err(msg));
                return;
            }
            if content.is_null() {
                let _ = tx.send(Err("no shareable content returned".into()));
                return;
            }
            // SAFETY: non-null and owned by the callback for its duration.
            let displays = unsafe { (*content).displays() };
            match displays.firstObject() {
                Some(d) => {
                    let _ = tx.send(Ok(d));
                }
                None => {
                    let _ = tx.send(Err("no displays available".into()));
                }
            }
        },
    );
    // SAFETY: the handler outlives the call via RcBlock.
    unsafe {
        SCShareableContent::getShareableContentWithCompletionHandler(&handler);
    }
    match rx.recv_timeout(CONTENT_TIMEOUT) {
        Ok(r) => r,
        Err(_) => Err("timed out querying shareable content (screen recording permission?)".into()),
    }
}
