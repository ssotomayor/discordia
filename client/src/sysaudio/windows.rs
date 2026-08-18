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

use tokio::sync::mpsc::UnboundedSender;
use windows::Win32::Foundation::{
    CloseHandle, E_ACCESSDENIED, HANDLE, WAIT_EVENT, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_E_DEVICE_IN_USE, AUDCLNT_E_SERVICE_NOT_RUNNING,
    AUDCLNT_E_UNSUPPORTED_FORMAT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    AUDCLNT_STREAMFLAGS_LOOPBACK, AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
    AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
    ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation,
    IActivateAudioInterfaceCompletionHandler, IActivateAudioInterfaceCompletionHandler_Impl,
    IAudioCaptureClient, IAudioClient, PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
    VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, WAVE_FORMAT_PCM, WAVEFORMATEX,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::Threading::{
    CreateEventW, GetCurrentProcessId, SetEvent, WaitForMultipleObjects, WaitForSingleObject,
};
use windows::Win32::System::Variant::VT_BLOB;
use windows::core::{HRESULT, Interface, Ref, Result as WinResult, implement};

use super::frames::{FRAME, FrameCutter};

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
            let _ = SetEvent(self.0.0);
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

        // Setup runs on the capture thread so activation failures reach the
        // caller immediately, rather than surfacing later as a silent track.
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
                // Signalled but not closed: the thread may still be waiting on
                // this handle. Closing it risks a use-after-free; leaking one
                // handle on a failed path is safer.
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

    // Failures here are mid-share: `start` already returned `Ok`, so the track
    // is published and would otherwise go quiet silently.
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

    // Float first to avoid conversion; some drivers refuse it, hence the PCM
    // fallback.
    let (bits, tag) = match init(&client, 32, FORMAT_IEEE_FLOAT) {
        Ok(()) => (32u16, "f32"),
        Err(hr) if hr == AUDCLNT_E_UNSUPPORTED_FORMAT => {
            // A client whose `Initialize` failed is spent; WASAPI requires
            // releasing it and activating a fresh one. Retrying on the same
            // client fails because it is already spent, not because of the
            // format.
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
        Err(hr) => {
            return Err(format!(
                "Couldn't start system-audio capture: {}",
                explain(hr)
            ));
        }
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

/// A fresh activation blob, deliberately leaked.
///
/// The engine keeps the pointer it is handed: freeing the block when `activate`
/// returned is what made the first capture on any machine die with
/// `STATUS_HEAP_CORRUPTION`. Measured, not guessed — leaking it fixed the crash,
/// padding by 64 bytes did not (so: not an overrun) and freeing through
/// `CoTaskMemFree` did not either (so: not a mismatched allocator), and probing
/// the allocator afterwards found the block untouched, so the engine still holds
/// it. Nothing says for how long, and there is no handle to hang it off.
///
/// So nobody frees it and nobody else gets it. One block per activation, 12
/// bytes, never reused — which is what makes the question this file used to have
/// to answer ("who else is holding this address, and are they done?") not a
/// question. Sharing one process-wide blob saved those bytes and cost a lease, a
/// retirement rule for abandoned activations, and a wedge with no recovery when
/// a capture thread hung with the lease in hand. See `docs/AUDIT-2026-08-17.md`.
///
/// One thing to know before trusting the "never freed" half: it is an
/// observation of this Windows, not a promise from Microsoft, who document
/// neither the lifetime nor who owns the block. A future engine that *does* free
/// it would be handing a Rust allocation to `CoTaskMemFree`, which survives
/// today only because that and Rust's allocator both sit on the process heap. If
/// per-app capture starts failing right after a Windows update, this is the line
/// to re-measure — the probe was: activate, check the block is untouched, then
/// check that 64 same-size allocations afterwards never land on its address.
fn activation_params() -> *mut AUDIOCLIENT_ACTIVATION_PARAMS {
    // Heap allocation avoids read-only memory faults if an engine writes back
    // into the blob.
    Box::into_raw(Box::new(AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                // Excluding our own tree is the whole point: it drops this app's
                // playback — the call itself — before it can be captured and
                // sent back out, and covers a spawned LiveKit with it.
                TargetProcessId: unsafe { GetCurrentProcessId() },
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
            },
        },
    }))
}

/// What the end of the activation wait means, and what becomes of the event.
///
/// A function rather than two branches inside `activate` because the timeout
/// half had never been executed — not in a test, not on any machine. Nobody has
/// made WASAPI take longer than `ACTIVATE_TIMEOUT` to answer, so the rule that
/// an abandoned activation keeps its event handle existed only as a reading of
/// the code. `activate` itself cannot be driven there without a real audio
/// engine *and* an engine that stalls; this can, by being handed the wait result
/// WASAPI would have produced. Same seam as `ScriptedMinter` in
/// `server/tests/voice.rs`: answer the call rather than arrange for it to fail.
fn settle_activation_wait(waited: WAIT_EVENT, done: SendHandle) -> Result<(), String> {
    if waited != WAIT_OBJECT_0 {
        // Leak `done` to avoid a use-after-free: the completion handler may
        // still fire on an MTA thread, and closing the handle now would let
        // its `SetEvent` signal a recycled handle value.
        return Err("Windows didn't answer the audio-capture request in time.".into());
    }
    // SAFETY: the handler has run — it is what signalled this event — so nothing
    // will touch the handle again.
    unsafe {
        let _ = CloseHandle(done.0);
    }
    Ok(())
}

/// Ask the audio engine for a process-loopback client that excludes us.
fn activate() -> Result<IAudioClient, String> {
    let params = activation_params();

    // Built manually because safe constructors don't cover BLOB. `PROPVARIANT`
    // is a plain `repr(C)` union with no `Drop`, so `PropVariantClear` won't
    // free stack memory; re-verify if the `windows` crate is bumped.
    let mut prop = PROPVARIANT::default();
    // SAFETY: writing the documented layout of a zeroed PROPVARIANT. The blob
    // outlives the process, never mind the call.
    unsafe {
        let inner = &mut prop.Anonymous.Anonymous;
        inner.vt = VT_BLOB;
        inner.Anonymous.blob.cbSize = std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32;
        inner.Anonymous.blob.pBlobData = params.cast::<u8>();
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
    settle_activation_wait(waited, done)?;

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
            Err(e) => {
                return Err(format!(
                    "reading the audio queue failed ({})",
                    explain(e.code())
                ));
            }
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
            return Err(format!(
                "reading a capture buffer failed ({})",
                explain(e.code())
            ));
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

#[cfg(test)]
mod tests {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use tokio::sync::mpsc::unbounded_channel;
    use windows::Win32::Foundation::{
        CloseHandle, GetHandleInformation, WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows::Win32::System::Threading::CreateEventW;

    /// The one part of this module a CI runner can execute — everything else
    /// needs an audio device, which is the same "nobody ran it" that hid the
    /// crash below.
    ///
    /// One address per activation is the whole safety argument now: an
    /// activation the engine is still reading — including one abandoned on
    /// timeout, which nothing can wait for — cannot be handed to anyone else,
    /// because nobody else is ever given that address.
    #[test]
    fn every_activation_gets_its_own_blob() {
        let (first, second) = (super::activation_params(), super::activation_params());
        assert_ne!(
            first, second,
            "two activations were handed one address; whichever the engine is \
             still holding, the other one is a use-after-free waiting to happen"
        );
    }

    /// The abandoned-activation path, executed for the first time.
    ///
    /// `activate`'s timeout branch was reasoned-about code: no machine has made
    /// WASAPI take longer than `ACTIVATE_TIMEOUT` to answer, so the rule it
    /// implements — an activation nobody can wait for keeps its event handle,
    /// because the completion handler may still `SetEvent` it from an MTA pool
    /// thread — had never once run. The other never-run branch in this file was
    /// `windows_loopback_delivers_real_samples`, and it found a heap corruption
    /// the first time anyone executed it.
    ///
    /// What this does and does not cover: it drives the decision WASAPI's answer
    /// produces, not WASAPI being slow. `WaitForSingleObject` is still never seen
    /// timing out for real — that needs a stalled audio engine, which no test can
    /// arrange — so this closes the branch, not the scenario.
    #[test]
    fn an_abandoned_activation_keeps_its_event_handle() {
        // SAFETY: an unnamed auto-reset event, exactly as `activate` makes one.
        let event = unsafe { CreateEventW(None, false, false, None) }.expect("create event");

        let err = super::settle_activation_wait(WAIT_TIMEOUT, super::SendHandle(event))
            .expect_err("a wait that did not end in WAIT_OBJECT_0 must fail the activation");
        assert!(
            err.contains("in time"),
            "the timeout must reach the user as a timeout rather than as a \
             generic failure: {err}"
        );

        // Closing this handle would cause a use-after-free when the completion
        // handler fires; Windows recycles handle values, so the stray
        // `SetEvent` would signal an unrelated object.
        let mut flags = 0u32;
        // SAFETY: querying a handle we own; the out-param is live for the call.
        let still_open = unsafe { GetHandleInformation(event, &mut flags) };
        assert!(
            still_open.is_ok(),
            "the abandoned activation's event was closed; the completion handler \
             can still signal it"
        );

        // Production leaks this on purpose and the test cannot, or it leaks one
        // handle per run of the suite.
        // SAFETY: no completion handler was ever registered against this event,
        // so nothing else can touch it.
        unsafe {
            let _ = CloseHandle(event);
        }
    }

    /// The other half of the same rule: a completed activation owns its handle
    /// and gives it back. Asserted through the return value only — checking that
    /// the handle is *gone* would race every other thread in the test binary,
    /// since Windows is free to hand the freed number straight to the next
    /// `CreateEventW` anywhere in the process.
    #[test]
    fn a_completed_activation_settles_cleanly() {
        // SAFETY: as above.
        let event = unsafe { CreateEventW(None, false, false, None) }.expect("create event");
        super::settle_activation_wait(WAIT_OBJECT_0, super::SendHandle(event))
            .expect("a handler that signalled must settle as success");
    }

    /// Does this backend capture anything, and does the process survive asking?
    ///
    /// Nobody had checked: it landed in `94120de` and was corrected in `638746b`
    /// on review, both times on the strength of being read — and the macOS
    /// backend, written the same way, returns `Ok` and then delivers zero samples
    /// for its whole life, which is invisible from inside and still open in
    /// `docs/AUDIT-2026-08-17.md`. This found worse on its first run: `start()` took the process
    /// down with STATUS_HEAP_CORRUPTION, the activation blob having been freed
    /// while the engine still held it (see `activation_params`). It passes now,
    /// peak around 0.35 against a tone from another process.
    ///
    /// ```text
    /// cargo test -p dioxusfun -- --ignored --nocapture windows_loopback
    /// ```
    #[tokio::test]
    #[ignore = "needs an audio device, a desktop session, and Windows build 20348+"]
    async fn windows_loopback_delivers_real_samples() {
        assert!(
            crate::sysaudio::supported(),
            "unsupported on this build ({}); nothing to exercise",
            crate::sysaudio::os_build_label()
        );

        let (tx, mut rx) = unbounded_channel::<Vec<f32>>();
        let (fatal_tx, mut fatal_rx) = unbounded_channel::<String>();
        let capture = crate::sysaudio::start(tx, fatal_tx, None).expect("start capture");

        // A *different* process on purpose: the capture excludes our own tree,
        // so noise this test made itself would be filtered out by design.
        let mut tone = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "1..8 | ForEach-Object { [console]::beep(880, 400) }",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the tone player");

        let (mut frames, mut samples, mut peak) = (0usize, 0usize, 0.0f32);
        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
                Ok(Some(frame)) => {
                    frames += 1;
                    samples += frame.len();
                    for s in frame {
                        peak = peak.max(s.abs());
                    }
                    if peak > 0.01 && frames > 20 {
                        break;
                    }
                }
                Ok(None) => break,
                Err(_) => {}
            }
        }

        let _ = tone.kill();
        // An unwaited child stays a zombie for as long as this process lives.
        let _ = tone.wait();
        drop(capture);

        if let Ok(err) = fatal_rx.try_recv() {
            panic!("the capture reported a fatal error: {err}");
        }
        println!("frames={frames} samples={samples} peak={peak:.4}");

        assert!(
            frames > 0,
            "start() succeeded and delivered no frames at all — the same shape \
             as the open macOS finding in docs/AUDIT-2026-08-17.md"
        );
        assert!(
            samples >= 4800,
            "only {samples} samples; the capture is starved"
        );
        // The part frame counting cannot see: silence is still frames.
        assert!(
            peak > 0.01,
            "{frames} frames captured but every sample was ~zero (peak \
             {peak:.6}) — a track published from this would be silent"
        );
    }
}
