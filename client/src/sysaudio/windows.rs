//! Windows system-audio capture via WASAPI **process** loopback.
//!
//! There are two loopbacks on Windows and only one of them is usable here.
//! *Endpoint* loopback (`IMMDeviceEnumerator` + `AUDCLNT_STREAMFLAGS_LOOPBACK`
//! on a render device) is the easy one, and it captures the whole machine mix —
//! including this app's own playback, so every voice in the call rides back out
//! and everyone hears themselves. *Process* loopback, activated through a
//! virtual device path, can be told to exclude a process tree: ours. That is
//! the entire reason this file exists, and it is why the easy API is not used
//! anywhere below.
//!
//! Requires Windows 10 build 20348. `super::scope()` gates on that, so by the
//! time anything here runs the build is known to be new enough.
//!
//! No resampler: unlike an ordinary capture client, a process-loopback client
//! does not support `GetMixFormat` — the format is ours to name. We name
//! 48 kHz, which is what the publish path wants, so the question never arises.

use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant};

use windows::core::{implement, Interface, Ref, Result as WinResult, HRESULT};
use windows::Win32::Foundation::{
    CloseHandle, E_ACCESSDENIED, HANDLE, WAIT_EVENT, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::Media::Audio::{
    ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation,
    IActivateAudioInterfaceCompletionHandler, IActivateAudioInterfaceCompletionHandler_Impl,
    IAudioCaptureClient, IAudioClient, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_E_DEVICE_IN_USE,
    AUDCLNT_E_SERVICE_NOT_RUNNING, AUDCLNT_E_UNSUPPORTED_FORMAT, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK,
    AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
    AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
    PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE, VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
    WAVEFORMATEX, WAVE_FORMAT_PCM,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
use windows::Win32::System::Threading::{
    CreateEventW, GetCurrentProcessId, SetEvent, WaitForMultipleObjects, WaitForSingleObject,
};
use windows::Win32::System::Variant::VT_BLOB;
use tokio::sync::mpsc::UnboundedSender;

use super::frames::{FrameCutter, FRAME};

/// Capture format. Both are 48 kHz — only the sample encoding differs, and the
/// float one is tried first because it is what the mixer speaks natively.
const RATE: u32 = 48_000;
const CHANNELS: u16 = 2;

/// `WAVE_FORMAT_IEEE_FLOAT` from mmreg.h. Spelled out rather than imported:
/// the binding for it lives in `Win32_Media_Multimedia`, and pulling that whole
/// module in to get one integer is a poor trade.
const FORMAT_IEEE_FLOAT: u16 = 3;

/// Engine buffer, in 100 ns units. 20 ms: long enough that an ordinary
/// scheduling hiccup doesn't drop audio, short enough that the wait below
/// returns often enough to keep the 10 ms cadence honest.
const BUFFER_100NS: i64 = 200_000;

/// How long the wait for audio may block before we conclude nothing is playing
/// and emit silence ourselves. Two frames' worth.
const WAIT_MS: u32 = 20;

/// How long to wait for the asynchronous activation to complete.
const ACTIVATE_TIMEOUT: Duration = Duration::from_secs(5);

/// Cap on silence injected in one go. Without it, a machine returning from
/// sleep would compute an enormous backlog and flush minutes of zeroes into
/// the track at once.
const MAX_SILENCE_FRAMES: u64 = 10;

/// A raw handle we move to the capture thread. `HANDLE` wraps a pointer and so
/// isn't `Send`; the value is just a kernel handle and moving it is fine.
#[derive(Clone, Copy)]
struct SendHandle(HANDLE);
// SAFETY: kernel handles are process-wide and have no thread affinity.
unsafe impl Send for SendHandle {}
unsafe impl Sync for SendHandle {}

/// Signals a Win32 event when the async activation finishes.
///
/// A Win32 event rather than an `mpsc::Sender` deliberately: this is called
/// back on an MTA pool thread, and a bare handle sidesteps every `Send`/`Sync`
/// question a channel would raise inside a COM object.
#[implement(IActivateAudioInterfaceCompletionHandler)]
struct ActivationDone(SendHandle);

impl IActivateAudioInterfaceCompletionHandler_Impl for ActivationDone_Impl {
    fn ActivateCompleted(
        &self,
        _op: Ref<'_, IActivateAudioInterfaceAsyncOperation>,
    ) -> WinResult<()> {
        // SAFETY: the handle outlives the operation — `start` waits on it
        // before returning, and only then drops it.
        unsafe {
            let _ = SetEvent(self.0 .0);
        }
        Ok(())
    }
}

pub struct WinCapture {
    /// Signalled on drop to break the capture loop immediately, rather than
    /// leaving teardown to wait out a `WAIT_MS` timeout.
    shutdown: SendHandle,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl WinCapture {
    pub fn start(
        tx: UnboundedSender<Vec<f32>>,
        fatal: UnboundedSender<String>,
    ) -> Result<Self, String> {
        // SAFETY: creating an unnamed, manual-reset-free event.
        let shutdown = unsafe { CreateEventW(None, true, false, None) }
            .map_err(|e| format!("create shutdown event: {e}"))?;
        let shutdown = SendHandle(shutdown);

        // Setup runs on the capture thread and reports back, so a failure to
        // activate or initialise reaches the caller with its real reason
        // instead of surfacing later as a track that is simply silent.
        let (ready_tx, ready_rx) = std_mpsc::channel::<Result<(), String>>();
        let thread = std::thread::Builder::new()
            .name("dxf-sysaudio-win".into())
            .spawn(move || run(tx, fatal, shutdown, ready_tx))
            .map_err(|e| format!("spawn capture thread: {e}"))?;

        match ready_rx.recv_timeout(ACTIVATE_TIMEOUT + Duration::from_secs(2)) {
            Ok(Ok(())) => Ok(Self {
                shutdown,
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                // SAFETY: the thread has already given up; closing is safe.
                unsafe {
                    let _ = CloseHandle(shutdown.0);
                }
                Err(e)
            }
            Err(_) => {
                // Signalled but deliberately NOT closed: the thread is still
                // running and may yet reach the capture loop, where it waits on
                // this very handle. Closing it here would leave that wait on a
                // freed — possibly recycled — handle. One leaked handle on a
                // path that has already failed is the cheaper mistake.
                unsafe {
                    let _ = SetEvent(shutdown.0);
                }
                Err("timed out starting system-audio capture".into())
            }
        }
    }
}

impl Drop for WinCapture {
    fn drop(&mut self) {
        // SAFETY: signalling and closing handles we own.
        unsafe {
            let _ = SetEvent(self.shutdown.0);
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        unsafe {
            let _ = CloseHandle(self.shutdown.0);
        }
    }
}

/// The capture thread: activate, initialise, then pump until told to stop.
fn run(
    tx: UnboundedSender<Vec<f32>>,
    fatal: UnboundedSender<String>,
    shutdown: SendHandle,
    ready: std_mpsc::Sender<Result<(), String>>,
) {
    // SAFETY: paired with CoUninitialize below.
    let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    if com.is_err() {
        let _ = ready.send(Err(format!("COM init failed ({com:?})")));
        return;
    }

    let started = match setup() {
        Ok(s) => {
            let _ = ready.send(Ok(()));
            s
        }
        Err(e) => {
            let _ = ready.send(Err(e));
            unsafe { CoUninitialize() };
            return;
        }
    };

    // A failure from here on is mid-share: `start` already returned `Ok`, so the
    // track is published and nobody would otherwise learn it went quiet.
    if let Err(e) = pump(started, tx, shutdown) {
        let _ = fatal.send(e);
    }

    // SAFETY: balances the CoInitializeEx above.
    unsafe { CoUninitialize() };
}

/// Everything acquired by `setup`, kept together so `pump` can be read on its
/// own terms.
struct Started {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    event: SendHandle,
    /// 32 for float samples, 16 for PCM — decides how a packet is decoded.
    bits: u16,
}

impl Drop for Started {
    fn drop(&mut self) {
        // SAFETY: stopping a client we started, and closing our own event.
        unsafe {
            let _ = self.client.Stop();
            let _ = CloseHandle(self.event.0);
        }
    }
}

fn setup() -> Result<Started, String> {
    let mut client = activate()?;

    // Float first: it is what the rest of the pipeline speaks, so it saves a
    // conversion. Some drivers refuse it, hence the PCM fallback.
    let (bits, tag) = match init(&client, 32, FORMAT_IEEE_FLOAT) {
        Ok(()) => (32u16, "f32"),
        Err(hr) if hr == AUDCLNT_E_UNSUPPORTED_FORMAT => {
            // A client whose `Initialize` failed is spent — WASAPI's contract is
            // to release it and activate a fresh one. Retrying on the same
            // client makes the second call fail because it is already spent,
            // not because of the format, so the PCM fallback never actually
            // happens and capture dies reporting that both formats were
            // rejected when only one ever was.
            drop(client);
            client = activate()?;
            init(&client, 16, WAVE_FORMAT_PCM as u16).map_err(|hr2| {
                format!(
                    "Windows rejected both capture formats for per-app audio ({}).",
                    explain(hr2)
                )
            })?;
            (16u16, "i16")
        }
        Err(hr) => return Err(format!("Couldn't start system-audio capture: {}", explain(hr))),
    };
    eprintln!("[sysaudio] windows process loopback: {RATE}Hz {CHANNELS}ch {tag}");

    // SAFETY: an unnamed auto-reset event handed to the audio engine.
    let event = unsafe { CreateEventW(None, false, false, None) }
        .map_err(|e| format!("create audio event: {e}"))?;
    let event = SendHandle(event);

    // SAFETY: the client is initialised and the handle outlives it — `Started`
    // stops the client before closing the event.
    unsafe {
        client
            .SetEventHandle(event.0)
            .map_err(|e| format!("set event handle: {}", explain(e.code())))?;
    }
    // SAFETY: initialised client; GetService is the documented way to reach the
    // capture half.
    let capture: IAudioCaptureClient = unsafe { client.GetService() }
        .map_err(|e| format!("get capture client: {}", explain(e.code())))?;
    // SAFETY: everything the engine needs is wired up.
    unsafe {
        client
            .Start()
            .map_err(|e| format!("start capture: {}", explain(e.code())))?;
    }

    Ok(Started {
        client,
        capture,
        event,
        bits,
    })
}

/// Ask the audio engine for a process-loopback client that excludes us.
fn activate() -> Result<IAudioClient, String> {
    // Boxed rather than left on the stack so that an activation which times out
    // — and may still be reading the blob on another thread — can be abandoned
    // by leaking this, instead of pulling it out from under the engine when the
    // function returns. See the timeout branch below.
    let mut params = Box::new(AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                // Our own tree. Excluding it is the whole point: it drops this
                // app's playback — the call itself — before it can be captured
                // and sent back out. It also covers any child process we spawn,
                // which is what keeps a bundled LiveKit out of the mix.
                TargetProcessId: unsafe { GetCurrentProcessId() },
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
            },
        },
    });

    // The activation parameters travel as a VT_BLOB PROPVARIANT. Built field by
    // field because the safe constructors don't cover BLOB.
    //
    // Pointing a PROPVARIANT at the stack would be a bug if it cleared itself —
    // `PropVariantClear` would hand a stack address to `CoTaskMemFree`. It
    // doesn't: the generated `PROPVARIANT` is a plain `repr(C)` union with no
    // `Drop`, and clearing is only ever explicit. Re-check that if the `windows`
    // crate is bumped.
    let mut prop = PROPVARIANT::default();
    // SAFETY: writing the documented layout of a zeroed PROPVARIANT. `params`
    // outlives the call below, which is all the blob pointer requires.
    unsafe {
        let inner = &mut prop.Anonymous.Anonymous;
        inner.vt = VT_BLOB;
        inner.Anonymous.blob.cbSize = std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32;
        inner.Anonymous.blob.pBlobData = std::ptr::from_mut(&mut *params).cast::<u8>();
    }

    // SAFETY: an unnamed auto-reset event, signalled by the handler below.
    let done = unsafe { CreateEventW(None, false, false, None) }
        .map_err(|e| format!("create activation event: {e}"))?;
    let done = SendHandle(done);
    let handler: IActivateAudioInterfaceCompletionHandler = ActivationDone(done).into();

    // SAFETY: all arguments outlive the call; the operation is awaited below.
    let op = unsafe {
        ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(&prop),
            &handler,
        )
    }
    .map_err(|e| {
        format!(
            "This build of Windows ({}) couldn't start per-app audio capture: {}",
            super::os_build_label(),
            explain(e.code())
        )
    })?;

    // SAFETY: waiting on our own event.
    let waited = unsafe { WaitForSingleObject(done.0, ACTIVATE_TIMEOUT.as_millis() as u32) };
    if waited != WAIT_OBJECT_0 {
        // Both deliberately leaked. On this path the completion handler has NOT
        // run and may still fire on an MTA pool thread: closing `done` now would
        // let it `SetEvent` a handle value Windows has since recycled, quietly
        // signalling some unrelated kernel object instead of failing loudly.
        // Freeing `params` would likewise pull the blob out from under an
        // activation still in flight. Same trade `WinCapture::start` makes with
        // its shutdown handle — a leak on an already-failed path costs far less
        // than a use-after-free.
        std::mem::forget(params);
        return Err("Windows didn't answer the audio-capture request in time.".into());
    }
    // SAFETY: the handler has run — it is what signalled this event — so nothing
    // will touch the handle again.
    unsafe {
        let _ = CloseHandle(done.0);
    }

    let mut hr = HRESULT(0);
    let mut iface = None;
    // SAFETY: both out-params are live for the call.
    unsafe {
        op.GetActivateResult(&mut hr, &mut iface)
            .map_err(|e| format!("audio capture activation failed: {}", explain(e.code())))?;
    }
    if hr.is_err() {
        return Err(format!(
            "Windows refused per-app audio capture: {}",
            explain(hr)
        ));
    }
    iface
        .ok_or_else(|| "Windows returned no audio client".to_string())?
        .cast::<IAudioClient>()
        .map_err(|e| format!("unexpected audio client type: {e}"))
}

/// Initialise the client at `bits` per sample. Returns the raw `HRESULT` on
/// failure so the caller can tell "wrong format, try the other one" from a
/// genuine problem.
fn init(client: &IAudioClient, bits: u16, tag: u16) -> Result<(), HRESULT> {
    let block_align = CHANNELS * bits / 8;
    let format = WAVEFORMATEX {
        wFormatTag: tag,
        nChannels: CHANNELS,
        nSamplesPerSec: RATE,
        nAvgBytesPerSec: RATE * block_align as u32,
        nBlockAlign: block_align,
        wBitsPerSample: bits,
        cbSize: 0,
    };
    // SAFETY: `format` is a complete WAVEFORMATEX and outlives the call.
    unsafe {
        client
            .Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                BUFFER_100NS,
                0,
                &format,
                None,
            )
            .map_err(|e| e.code())
    }
}

/// Drain packets as they arrive, and keep the stream flowing when they don't.
///
/// `Ok` means a clean stop — told to shut down, or the receiver went away with
/// the voice session. `Err` means the capture itself broke, which is worth
/// telling the user about because the track stays published either way.
fn pump(
    started: Started,
    tx: UnboundedSender<Vec<f32>>,
    shutdown: SendHandle,
) -> Result<(), String> {
    let mut cutter = FrameCutter::new(tx);
    let handles = [started.event.0, shutdown.0];
    let clock = Instant::now();

    loop {
        // SAFETY: both handles are owned and live for the whole loop.
        let waited = unsafe { WaitForMultipleObjects(&handles, false, WAIT_MS) };
        if waited == WAIT_EVENT(WAIT_OBJECT_0.0 + 1) {
            break;
        }
        if waited == WAIT_OBJECT_0 {
            // Audio is flowing: it sets the pace, and nothing is added to it.
            // Topping up here as well would splice silence into the middle of
            // live audio the moment the engine's clock ran a hair slow — an
            // audible click, in exchange for a cadence nobody was missing.
            if !drain(&started, &mut cutter)? {
                break;
            }
            continue;
        }
        if waited != WAIT_TIMEOUT {
            return Err(format!("the audio engine stopped responding ({waited:?})"));
        }
        // Timed out, so nothing is playing: the engine goes quiet rather than
        // delivering zeroes, and libwebrtc wants a steady 10 ms cadence. The
        // gap is measured against the wall clock rather than counted per
        // iteration, which makes it self-correcting — once real audio resumes,
        // `emitted` catches up on its own and nothing more is injected.
        let expected = clock.elapsed().as_millis() as u64 / 10;
        let emitted = cutter.emitted();
        if expected > emitted + 1 {
            let missing = (expected - emitted - 1).min(MAX_SILENCE_FRAMES) as usize;
            if !cutter.push_silence(missing * FRAME) {
                break;
            }
        }
    }
    eprintln!("[sysaudio] windows capture stopped");
    Ok(())
}

/// Pull every queued packet. `Ok(false)` means the receiver is gone (a clean
/// stop); `Err` means WASAPI failed.
fn drain(started: &Started, cutter: &mut FrameCutter) -> Result<bool, String> {
    loop {
        // SAFETY: the capture client is live for the life of `Started`.
        match unsafe { started.capture.GetNextPacketSize() } {
            Ok(0) => return Ok(true),
            Ok(_) => {}
            Err(e) => return Err(format!("reading the audio queue failed ({})", explain(e.code()))),
        }

        let mut data: *mut u8 = std::ptr::null_mut();
        let mut frames: u32 = 0;
        let mut flags: u32 = 0;
        // SAFETY: out-params are live; the buffer is released below before the
        // next call, as the API requires.
        if let Err(e) = unsafe {
            started
                .capture
                .GetBuffer(&mut data, &mut frames, &mut flags, None, None)
        } {
            return Err(format!("reading a capture buffer failed ({})", explain(e.code())));
        }

        let alive = if frames == 0 {
            true
        } else if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
            // The flag is the *only* signal that the buffer is silence: its
            // contents are undefined, so copying them would be noise.
            cutter.push_silence(frames as usize)
        } else if data.is_null() {
            true
        } else if started.bits == 32 {
            let n = frames as usize * CHANNELS as usize;
            // SAFETY: the engine guarantees `frames * blockAlign` bytes.
            let samples = unsafe { std::slice::from_raw_parts(data.cast::<f32>(), n) };
            cutter.push_interleaved(samples, CHANNELS as usize)
        } else {
            let n = frames as usize * CHANNELS as usize;
            // SAFETY: as above, for the 16-bit fallback format.
            let samples = unsafe { std::slice::from_raw_parts(data.cast::<i16>(), n) };
            let scratch: Vec<f32> = samples
                .iter()
                .map(|s| *s as f32 / i16::MAX as f32)
                .collect();
            cutter.push_interleaved(&scratch, CHANNELS as usize)
        };

        // SAFETY: releasing exactly what GetBuffer handed out.
        if let Err(e) = unsafe { started.capture.ReleaseBuffer(frames) } {
            return Err(format!(
                "releasing a capture buffer failed ({})",
                explain(e.code())
            ));
        }
        if !alive {
            return Ok(false);
        }
    }
}

/// Turn an `HRESULT` into something worth putting in front of a person.
///
/// Only the codes with an actionable meaning get prose; everything else keeps
/// its raw value, which is more useful to a bug report than invented wording.
fn explain(hr: HRESULT) -> String {
    if hr == AUDCLNT_E_SERVICE_NOT_RUNNING {
        "the Windows Audio service isn't running".into()
    } else if hr == AUDCLNT_E_DEVICE_IN_USE {
        "another app has the audio device exclusively".into()
    } else if hr == AUDCLNT_E_UNSUPPORTED_FORMAT {
        "the audio engine rejected the capture format".into()
    } else if hr == E_ACCESSDENIED {
        "Windows denied access to audio capture".into()
    } else {
        format!("error {:#010x}", hr.0 as u32)
    }
}
