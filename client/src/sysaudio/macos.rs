use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AllocAnyThread, DefinedClass, define_class, msg_send};
use objc2_core_audio_types::{AudioBufferList, kAudioFormatFlagIsFloat, kAudioFormatLinearPCM};
use objc2_core_media::{
    CMAudioFormatDescriptionGetStreamBasicDescription, CMBlockBuffer, CMSampleBuffer,
};
use objc2_foundation::NSError;
use objc2_screen_capture_kit::{
    SCStream, SCStreamConfiguration, SCStreamDelegate, SCStreamOutput, SCStreamOutputType,
};
use tokio::sync::mpsc::UnboundedSender;

use crate::sysvideo::Target;

const CONTENT_TIMEOUT: Duration = Duration::from_secs(20);

struct TapState {
    cutter: std::cell::RefCell<super::frames::FrameCutter>,
    fatal: UnboundedSender<String>,
    failed: std::sync::atomic::AtomicBool,
}

define_class!(
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

    unsafe impl SCStreamDelegate for Tap {
        #[unsafe(method(stream:didStopWithError:))]
        unsafe fn stream_did_stop(&self, _stream: &SCStream, error: &NSError) {
            self.fail(format!(
                "ScreenCaptureKit stopped system audio: {}",
                error.localizedDescription()
            ));
        }
    }
);

impl Tap {
    fn fail(&self, message: String) {
        let state = self.ivars();
        if !state
            .failed
            .swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            eprintln!("[sysaudio] macOS capture failed: {message}");
            let _ = state.fatal.send(message);
        }
    }

    fn handle_audio(&self, sample: &CMSampleBuffer) {
        if self
            .ivars()
            .failed
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return;
        }
        if !unsafe { sample.is_valid() } {
            self.fail("ScreenCaptureKit returned an invalid audio sample".into());
            return;
        }

        let format = unsafe { sample.format_description() };
        let Some(format) = format else {
            self.fail("screen audio has no format description".into());
            return;
        };
        let asbd = unsafe { CMAudioFormatDescriptionGetStreamBasicDescription(&format) };
        let Some(asbd) = (unsafe { asbd.as_ref() }) else {
            self.fail("screen audio has no PCM format".into());
            return;
        };
        if asbd.mFormatID != kAudioFormatLinearPCM
            || asbd.mFormatFlags & kAudioFormatFlagIsFloat == 0
            || asbd.mBitsPerChannel != 32
            || asbd.mSampleRate.round() as u32 != 48_000
        {
            self.fail(format!(
                "unsupported screen-audio format: id={:#x} flags={:#x} {}-bit {} Hz",
                asbd.mFormatID, asbd.mFormatFlags, asbd.mBitsPerChannel, asbd.mSampleRate
            ));
            return;
        }

        let mut needed = 0usize;
        let query_status = unsafe {
            sample.audio_buffer_list_with_retained_block_buffer(
                std::ptr::from_mut(&mut needed),
                std::ptr::null_mut(),
                0,
                None,
                None,
                0,
                std::ptr::null_mut(),
            )
        };
        if query_status != 0 || needed < std::mem::size_of::<AudioBufferList>() {
            self.fail(format!(
                "couldn't size the screen-audio buffers (status {query_status}, size {needed})"
            ));
            return;
        }

        let word = std::mem::size_of::<usize>();
        let mut storage = vec![0usize; needed.div_ceil(word)];
        let list_ptr = storage.as_mut_ptr().cast::<AudioBufferList>();
        let mut block: *mut CMBlockBuffer = std::ptr::null_mut();

        let status = unsafe {
            sample.audio_buffer_list_with_retained_block_buffer(
                std::ptr::null_mut(),
                list_ptr,
                storage.len() * word,
                None,
                None,
                0,
                std::ptr::from_mut(&mut block),
            )
        };
        if status != 0 {
            self.fail(format!(
                "couldn't read screen-audio buffers (status {status})"
            ));
            return;
        }

        let _block = if block.is_null() {
            self.fail("screen audio returned no backing buffer".into());
            return;
        } else {
            unsafe {
                objc2_core_foundation::CFRetained::from_raw(std::ptr::NonNull::new_unchecked(block))
            }
        };
        let list = unsafe { &*list_ptr };

        let n = list.mNumberBuffers as usize;
        let mut mono: Vec<f32> = Vec::new();
        let mut mixed_channels = 0usize;
        for i in 0..n {
            let buf = unsafe { &*list.mBuffers.as_ptr().add(i) };
            let channels = buf.mNumberChannels as usize;
            if buf.mData.is_null() || channels == 0 {
                continue;
            }
            let sample_count = buf.mDataByteSize as usize / std::mem::size_of::<f32>();
            let samples =
                unsafe { std::slice::from_raw_parts(buf.mData.cast::<f32>(), sample_count) };
            let frames = sample_count / channels;
            if mono.is_empty() {
                mono.resize(frames, 0.0);
            }
            let frames = frames.min(mono.len());
            for (frame, mixed) in mono.iter_mut().enumerate().take(frames) {
                let start = frame * channels;
                for sample in &samples[start..start + channels] {
                    *mixed += *sample;
                }
            }
            mixed_channels += channels;
        }
        if !mono.is_empty() && mixed_channels > 1 {
            let scale = 1.0 / mixed_channels as f32;
            for m in mono.iter_mut() {
                *m *= scale;
            }
        }

        if mono.is_empty() {
            return;
        }

        self.ivars().cutter.borrow_mut().push_mono(&mono);
    }
}

pub struct MacCapture {
    stream: Retained<SCStream>,
}

impl MacCapture {
    pub fn start(
        target: Target,
        tx: UnboundedSender<Vec<f32>>,
        fatal: UnboundedSender<String>,
    ) -> Result<Self, String> {
        let filter = crate::sysvideo::content_filter(target)?;

        unsafe {
            let config = SCStreamConfiguration::new();
            config.setCapturesAudio(true);
            config.setSampleRate(48_000);
            config.setChannelCount(2);
            config.setExcludesCurrentProcessAudio(true);
            config.setWidth(2);
            config.setHeight(2);

            let tap = Tap::alloc().set_ivars(TapState {
                cutter: std::cell::RefCell::new(super::frames::FrameCutter::new(tx)),
                fatal,
                failed: std::sync::atomic::AtomicBool::new(false),
            });
            let tap: Retained<Tap> = msg_send![super(tap), init];
            let output = ProtocolObject::from_ref(&*tap);
            let delegate = ProtocolObject::from_ref(&*tap);

            let stream: Retained<SCStream> = SCStream::initWithFilter_configuration_delegate(
                SCStream::alloc(),
                &filter,
                &config,
                Some(delegate),
            );

            let queue = dispatch2::DispatchQueue::new("fun.dioxus.sysaudio", None);
            stream
                .addStreamOutput_type_sampleHandlerQueue_error(
                    output,
                    SCStreamOutputType::Audio,
                    Some(&queue),
                )
                .map_err(|e| format!("add audio output: {e}"))?;

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
        unsafe {
            self.stream.stopCaptureWithCompletionHandler(None);
        }
    }
}
