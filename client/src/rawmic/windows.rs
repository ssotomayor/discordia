//! Raw-mode WASAPI microphone capture.
//!
//! The one line that makes this module worth having is the
//! `SetClientProperties` call in `setup`: `AUDCLNT_STREAMOPTIONS_RAW` before
//! `Initialize` is what tells the audio engine to skip the endpoint's effects.
//! Everything else exists because owning the client means owning device
//! selection, format negotiation and the pump loop as well.
//!
//! The stream category stays `Other` rather than `Communications` on purpose.
//! Communications is what makes Windows duck every other application while we
//! capture, and this setting is about the microphone's processing — changing
//! what the rest of the machine sounds like in passing would be a second,
//! unasked-for change hiding inside the first.
//!
//! Shared mode, not exclusive: raw and exclusive are independent, and exclusive
//! would take the device away from every other app on the machine. Raw already
//! answers the question the setting asks.

use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use tokio::sync::mpsc::UnboundedSender;
use windows::Win32::Foundation::{
    CloseHandle, E_ACCESSDENIED, HANDLE, PROPERTYKEY, WAIT_EVENT, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_E_DEVICE_IN_USE, AUDCLNT_E_SERVICE_NOT_RUNNING,
    AUDCLNT_E_UNSUPPORTED_FORMAT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    AUDCLNT_STREAMOPTIONS_NONE, AUDCLNT_STREAMOPTIONS_RAW, AudioCategory_Other,
    AudioClientProperties, DEVICE_STATE_ACTIVE, IAudioCaptureClient, IAudioClient, IAudioClient2,
    IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
    eCapture, eConsole,
};
use windows::Win32::System::Com::StructuredStorage::PropVariantClear;
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize, STGM_READ,
};
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForMultipleObjects};
use windows::Win32::System::Variant::VT_LPWSTR;
use windows::core::{GUID, HRESULT, Interface};

/// Builds the sink once the device's format is known.
///
/// Two-phase because the format is the *device's*, not ours to name: the
/// resampler on the other side is built around its rate, and that is only known
/// after the client is initialised — which happens on the capture thread, so
/// this runs there too. Arguments are the sample rate and the channel count;
/// what comes back is handed every buffer, interleaved, as long as the capture
/// lives.
pub type SinkBuilder = Box<dyn FnOnce(u32, u32) -> Sink + Send>;

/// What a `SinkBuilder` builds: one buffer of interleaved samples at a time.
pub type Sink = Box<dyn FnMut(&[f32]) + Send>;

/// Engine buffer, in 100 ns units. 20 ms: long enough that a scheduling hiccup
/// doesn't cost audio, short enough to keep the 10 ms cadence downstream honest.
const BUFFER_100NS: i64 = 200_000;

/// How long the wait for a buffer may block before we call the device dead. An
/// event-driven capture client signals every period whether or not anyone is
/// speaking — silence is still buffers — so two seconds of nothing is a broken
/// stream, not a quiet room.
const WAIT_MS: u32 = 2_000;

/// How long to wait for the capture thread to report that it started.
const START_TIMEOUT: Duration = Duration::from_secs(5);

/// `WAVE_FORMAT_EXTENSIBLE` and `WAVE_FORMAT_IEEE_FLOAT` from mmreg.h, and the
/// two subformat GUIDs that go with them. Spelled out for the same reason
/// `sysaudio::windows` spells out its own: the bindings live in modules we
/// would otherwise compile whole to get four constants.
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
const WAVE_FORMAT_PCM: u16 = 1;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
const SUBTYPE_PCM: GUID = GUID::from_u128(0x00000001_0000_0010_8000_00aa00389b71);
const SUBTYPE_IEEE_FLOAT: GUID = GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);

/// `PKEY_Device_FriendlyName`. The name cpal reports for the same endpoint, so
/// the device the user picked in the dropdown is the device opened here.
const PKEY_DEVICE_FRIENDLY_NAME: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
    pid: 14,
};

/// A raw handle we move to the capture thread. `HANDLE` wraps a pointer and so
/// isn't `Send`; the value is just a kernel handle and moving it is fine.
#[derive(Clone, Copy)]
struct SendHandle(HANDLE);
// SAFETY: kernel handles are process-wide and have no thread affinity.
unsafe impl Send for SendHandle {}
unsafe impl Sync for SendHandle {}

/// A running raw capture. Dropping it stops the stream and joins the thread.
pub struct Capture {
    /// Signalled on drop to break the pump immediately, rather than leaving
    /// teardown to wait out a `WAIT_MS` timeout.
    shutdown: SendHandle,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Capture {
    /// Open `device` (by the name cpal reports; `None` = the default capture
    /// endpoint) in raw mode and start pumping buffers into the sink.
    ///
    /// `fatal` carries a failure that happens *after* this returns `Ok` — the
    /// microphone dying mid-call, which is otherwise indistinguishable from a
    /// user who stopped talking.
    pub fn start(
        device: Option<String>,
        fatal: UnboundedSender<String>,
        sink: SinkBuilder,
    ) -> Result<Self, String> {
        Self::start_with_bypass(device, fatal, sink, true)
    }

    /// The same capture, with the choice of asking for raw mode or not.
    ///
    /// Only a test calls this with `false`. It exists because the switch in the
    /// client can report that raw mode was *requested* and nothing more:
    /// `SetClientProperties` either accepts or refuses, and there is no
    /// read-back saying the endpoint's effects left the path. Opening the same
    /// device both ways at once, against the same room, is the one comparison
    /// that can tell an accepted request from an effective one.
    pub fn start_with_bypass(
        device: Option<String>,
        fatal: UnboundedSender<String>,
        sink: SinkBuilder,
        bypass: bool,
    ) -> Result<Self, String> {
        // SAFETY: creating an unnamed manual-reset event.
        let shutdown = unsafe { CreateEventW(None, true, false, None) }
            .map_err(|e| format!("create shutdown event: {e}"))?;
        let shutdown = SendHandle(shutdown);

        // Setup runs on the capture thread and reports back, so a device that
        // refuses raw mode reaches the caller with its real reason instead of
        // surfacing later as a microphone that is simply silent.
        let (ready_tx, ready_rx) = std_mpsc::channel::<Result<(), String>>();
        let thread = std::thread::Builder::new()
            .name("dxf-rawmic-win".into())
            .spawn(move || run(device, fatal, sink, shutdown, ready_tx, bypass))
            .map_err(|e| format!("spawn capture thread: {e}"))?;

        match ready_rx.recv_timeout(START_TIMEOUT) {
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
                // Signalled but deliberately NOT closed: the thread may yet
                // reach the pump, where it waits on this very handle. Same
                // trade `sysaudio::windows` makes — one leaked handle on an
                // already-failed path beats a wait on a recycled one.
                unsafe {
                    let _ = SetEvent(shutdown.0);
                }
                Err("timed out opening the microphone in raw mode".into())
            }
        }
    }
}

impl Drop for Capture {
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

/// The capture thread: open, initialise, then pump until told to stop.
fn run(
    device: Option<String>,
    fatal: UnboundedSender<String>,
    sink: SinkBuilder,
    shutdown: SendHandle,
    ready: std_mpsc::Sender<Result<(), String>>,
    bypass: bool,
) {
    // SAFETY: paired with CoUninitialize below.
    let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    if com.is_err() {
        let _ = ready.send(Err(format!("COM init failed ({com:?})")));
        return;
    }

    let started = match setup(device, bypass) {
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

    let mut sink = sink(started.format.rate, started.format.channels);
    // A failure from here on is mid-call: `start` already returned `Ok`, so the
    // track is published and nobody would otherwise learn it went quiet.
    if let Err(e) = pump(&started, &mut sink, shutdown) {
        let _ = fatal.send(e);
    }

    // SAFETY: balances the CoInitializeEx above.
    unsafe { CoUninitialize() };
}

/// How the engine hands us samples.
struct Format {
    rate: u32,
    channels: u32,
    sample: Sample,
}

/// The encodings we decode. The shared mix format is 32-bit float on every
/// machine we have seen; the integer arms are there so an unusual driver gets
/// audio rather than an error.
enum Sample {
    F32,
    I16,
    I32,
}

/// Everything `setup` acquired, kept together so `pump` reads on its own terms.
struct Started {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    event: SendHandle,
    format: Format,
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

fn setup(device: Option<String>, bypass: bool) -> Result<Started, String> {
    let device = open(device)?;
    let name = friendly_name(&device).unwrap_or_else(|| "<unknown>".into());

    // SAFETY: activating the audio client of an endpoint we hold.
    let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }
        .map_err(|e| format!("open the microphone: {}", explain(e.code())))?;

    // The whole point of the module, and it only works before `Initialize`.
    let client2: IAudioClient2 = client
        .cast()
        .map_err(|_| "this microphone's driver is too old for raw mode".to_string())?;
    let props = AudioClientProperties {
        cbSize: std::mem::size_of::<AudioClientProperties>() as u32,
        bIsOffload: false.into(),
        eCategory: AudioCategory_Other,
        // `bypass` is always true in the client. It is a parameter so that a
        // test can open the *same* endpoint the other way and compare, which is
        // the only way to see whether the endpoint had any processing to skip —
        // `SetClientProperties` succeeding says the request was accepted, not
        // that anything changed.
        Options: if bypass {
            AUDCLNT_STREAMOPTIONS_RAW
        } else {
            AUDCLNT_STREAMOPTIONS_NONE
        },
    };
    // SAFETY: `props` is a complete, correctly-sized struct that outlives the
    // call.
    unsafe { client2.SetClientProperties(&props) }
        .map_err(|e| format!("Windows refused raw capture: {}", explain(e.code())))?;

    // Asked for *after* raw mode is set: the engine is entitled to answer
    // differently once it knows the effects are out of the path, and
    // initialising against the format it did not name is how a client ends up
    // rejected for a format the device actually supports.
    //
    // SAFETY: the returned block is CoTaskMem-allocated and freed below.
    let mix = unsafe { client.GetMixFormat() }
        .map_err(|e| format!("read the microphone's format: {}", explain(e.code())))?;
    let format = describe(mix);

    // SAFETY: `mix` is the engine's own format block, valid until it is freed.
    let init = unsafe {
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
            BUFFER_100NS,
            0,
            mix,
            None,
        )
    };
    // SAFETY: freeing exactly what GetMixFormat allocated, and nothing below
    // reads it again.
    unsafe { CoTaskMemFree(Some(mix.cast())) };
    init.map_err(|e| format!("start raw capture: {}", explain(e.code())))?;
    let format = format?;

    // SAFETY: an unnamed auto-reset event handed to the audio engine.
    let event = unsafe { CreateEventW(None, false, false, None) }
        .map_err(|e| format!("create audio event: {e}"))?;
    let event = SendHandle(event);
    // SAFETY: the client is initialised and the handle outlives it — `Started`
    // stops the client before closing the event.
    unsafe { client.SetEventHandle(event.0) }
        .map_err(|e| format!("set event handle: {}", explain(e.code())))?;
    // SAFETY: initialised client; GetService is the documented way to reach the
    // capture half.
    let capture: IAudioCaptureClient = unsafe { client.GetService() }
        .map_err(|e| format!("get capture client: {}", explain(e.code())))?;
    // SAFETY: everything the engine needs is wired up.
    unsafe { client.Start() }.map_err(|e| format!("start capture: {}", explain(e.code())))?;

    eprintln!(
        "[rawmic] windows raw capture: device={name} rate={} ch={} format={}",
        format.rate,
        format.channels,
        match format.sample {
            Sample::F32 => "f32",
            Sample::I16 => "i16",
            Sample::I32 => "i32",
        }
    );

    Ok(Started {
        client,
        capture,
        event,
        format,
    })
}

/// The endpoint named, or the default one.
///
/// A name that no longer matches anything falls back to the default rather than
/// failing: the alternative is a microphone that stops working because a device
/// was unplugged, which is not what the user changed.
fn open(device: Option<String>) -> Result<IMMDevice, String> {
    // SAFETY: creating the documented device-enumerator object.
    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
            .map_err(|e| format!("enumerate audio devices: {}", explain(e.code())))?;

    if let Some(want) = device.as_deref() {
        // SAFETY: enumerating active capture endpoints from a live enumerator.
        let found = unsafe {
            enumerator
                .EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)
                .ok()
                .and_then(|all| {
                    let count = all.GetCount().ok()?;
                    (0..count)
                        .filter_map(|i| all.Item(i).ok())
                        .find(|d| friendly_name(d).as_deref() == Some(want))
                })
        };
        match found {
            Some(d) => return Ok(d),
            None => eprintln!("[rawmic] '{want}' is not a capture endpoint; using the default"),
        }
    }

    // eConsole, matching what cpal calls the default input device — otherwise
    // turning this setting on could silently change which microphone is live.
    //
    // SAFETY: querying the default endpoint from a live enumerator.
    unsafe { enumerator.GetDefaultAudioEndpoint(eCapture, eConsole) }
        .map_err(|e| format!("no microphone available: {}", explain(e.code())))
}

/// The endpoint's friendly name, as cpal reports it.
fn friendly_name(device: &IMMDevice) -> Option<String> {
    // SAFETY: reading one string property from a device we hold, and clearing
    // the variant afterwards. The generated `PROPVARIANT` has no `Drop`, so the
    // clear has to be explicit.
    unsafe {
        let store = device.OpenPropertyStore(STGM_READ).ok()?;
        let mut prop = store.GetValue(&PKEY_DEVICE_FRIENDLY_NAME).ok()?;
        let inner = &prop.Anonymous.Anonymous;
        let name = if inner.vt == VT_LPWSTR {
            inner.Anonymous.pwszVal.to_string().ok()
        } else {
            None
        };
        let _ = PropVariantClear(&mut prop);
        name
    }
}

/// Read the engine's format block. Anything we cannot decode is named in the
/// error rather than approximated — a wrong guess here is not a worse-sounding
/// microphone, it is noise.
fn describe(format: *const WAVEFORMATEX) -> Result<Format, String> {
    // SAFETY: the engine's block is a valid WAVEFORMATEX, and an extensible one
    // is a WAVEFORMATEX followed by the extension — which `cbSize` confirms
    // before it is read as one.
    let f = unsafe { *format };
    let tag = if f.wFormatTag == WAVE_FORMAT_EXTENSIBLE {
        let need = (std::mem::size_of::<WAVEFORMATEXTENSIBLE>()
            - std::mem::size_of::<WAVEFORMATEX>()) as u16;
        if f.cbSize < need {
            return Err("the microphone reported a truncated format".into());
        }
        let ext = unsafe { &*(format.cast::<WAVEFORMATEXTENSIBLE>()) };
        match ext.SubFormat {
            SUBTYPE_IEEE_FLOAT => WAVE_FORMAT_IEEE_FLOAT,
            SUBTYPE_PCM => WAVE_FORMAT_PCM,
            other => return Err(format!("unsupported microphone format {other:?}")),
        }
    } else {
        f.wFormatTag
    };

    let sample = match (tag, f.wBitsPerSample) {
        (WAVE_FORMAT_IEEE_FLOAT, 32) => Sample::F32,
        (WAVE_FORMAT_PCM, 16) => Sample::I16,
        (WAVE_FORMAT_PCM, 32) => Sample::I32,
        (tag, bits) => {
            return Err(format!(
                "unsupported microphone format (tag {tag}, {bits}-bit)"
            ));
        }
    };
    if f.nChannels == 0 || f.nSamplesPerSec == 0 {
        return Err("the microphone reported an empty format".into());
    }
    Ok(Format {
        rate: f.nSamplesPerSec,
        channels: f.nChannels as u32,
        sample,
    })
}

/// Hand buffers to the sink as the engine signals them.
fn pump(started: &Started, sink: &mut Sink, shutdown: SendHandle) -> Result<(), String> {
    let handles = [started.event.0, shutdown.0];
    // Reused for the life of the capture: this thread is the audio path, and an
    // allocation per buffer is an allocation 100 times a second.
    let mut scratch: Vec<f32> = Vec::with_capacity(4096);

    loop {
        // SAFETY: both handles are owned and live for the whole loop.
        let waited = unsafe { WaitForMultipleObjects(&handles, false, WAIT_MS) };
        if waited == WAIT_EVENT(WAIT_OBJECT_0.0 + 1) {
            break;
        }
        if waited == WAIT_TIMEOUT {
            return Err("the microphone stopped delivering audio".into());
        }
        if waited != WAIT_OBJECT_0 {
            return Err(format!("the audio engine stopped responding ({waited:?})"));
        }
        drain(started, sink, &mut scratch)?;
    }
    eprintln!("[rawmic] windows raw capture stopped");
    Ok(())
}

/// Pull every queued packet and hand it on.
fn drain(started: &Started, sink: &mut Sink, scratch: &mut Vec<f32>) -> Result<(), String> {
    let channels = started.format.channels as usize;
    loop {
        // SAFETY: the capture client is live for the life of `Started`.
        match unsafe { started.capture.GetNextPacketSize() } {
            Ok(0) => return Ok(()),
            Ok(_) => {}
            Err(e) => {
                return Err(format!(
                    "reading the microphone queue failed ({})",
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
                "reading a microphone buffer failed ({})",
                explain(e.code())
            ));
        }

        let n = frames as usize * channels;
        scratch.clear();
        if n > 0 {
            if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                // The flag is the *only* signal that the buffer is silence: its
                // contents are undefined, so copying them would be noise. The
                // zeroes still go through, because the pipeline downstream
                // wants an unbroken 10 ms cadence.
                scratch.resize(n, 0.0);
            } else {
                // SAFETY: the engine guarantees `frames * blockAlign` bytes in
                // the format `describe` read off it.
                unsafe {
                    match started.format.sample {
                        Sample::F32 => {
                            scratch.extend_from_slice(std::slice::from_raw_parts(
                                data.cast::<f32>(),
                                n,
                            ));
                        }
                        Sample::I16 => scratch.extend(
                            std::slice::from_raw_parts(data.cast::<i16>(), n)
                                .iter()
                                .map(|s| *s as f32 / i16::MAX as f32),
                        ),
                        Sample::I32 => scratch.extend(
                            std::slice::from_raw_parts(data.cast::<i32>(), n)
                                .iter()
                                .map(|s| *s as f32 / i32::MAX as f32),
                        ),
                    }
                }
            }
            sink(scratch);
        }

        // SAFETY: releasing exactly what GetBuffer handed out.
        if let Err(e) = unsafe { started.capture.ReleaseBuffer(frames) } {
            return Err(format!(
                "releasing a microphone buffer failed ({})",
                explain(e.code())
            ));
        }
    }
}

/// Turn an `HRESULT` into something worth putting in front of a person. Only
/// the codes with an actionable meaning get prose; everything else keeps its
/// raw value, which is more useful to a bug report than invented wording.
fn explain(hr: HRESULT) -> String {
    if hr == AUDCLNT_E_SERVICE_NOT_RUNNING {
        "the Windows Audio service isn't running".into()
    } else if hr == AUDCLNT_E_DEVICE_IN_USE {
        "another app has the microphone exclusively".into()
    } else if hr == AUDCLNT_E_UNSUPPORTED_FORMAT {
        "the audio engine rejected the capture format".into()
    } else if hr == E_ACCESSDENIED {
        "Windows denied access to the microphone".into()
    } else {
        format!("error {:#010x}", hr.0 as u32)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    use parking_lot::Mutex;
    use tokio::sync::mpsc::unbounded_channel;

    /// Collect a capture's samples and the format it opened at.
    fn collect(
        bypass: bool,
        into: Arc<Mutex<Vec<f32>>>,
        fmt: Arc<Mutex<String>>,
    ) -> super::Capture {
        let (fatal_tx, _fatal_rx) = unbounded_channel::<String>();
        let (buf, f) = (into, fmt);
        super::Capture::start_with_bypass(
            None,
            fatal_tx,
            Box::new(move |rate, channels| {
                *f.lock() = format!("{rate}Hz {channels}ch");
                Box::new(move |data: &[f32]| buf.lock().extend_from_slice(data))
            }),
            bypass,
        )
        .expect("open the microphone")
    }

    /// Below this peak the room is not producing anything the endpoint's
    /// effects would touch, so a comparison of the two paths is vacuous.
    /// −34 dBFS. Picked against a measurement rather than a round number: the
    /// idle hum on this host's line-in peaks at 0.0113, and ordinary speech at a
    /// normal distance runs an order of magnitude above that. A floor of 0.01
    /// sat *below* the hum and let a vacuous run pass for a reading.
    const SILENCE_FLOOR: f32 = 0.02;

    fn rms(v: &[f32]) -> f32 {
        if v.is_empty() {
            return 0.0;
        }
        (v.iter().map(|s| s * s).sum::<f32>() / v.len() as f32).sqrt()
    }

    /// Does asking for raw mode change the signal, or only the request?
    ///
    /// This is the measurement `docs/AUDIT-2026-08-17.md` asks for, and it is deliberately not
    /// the one that entry proposed. It suggested the `live_sfu` sweep — but that
    /// sweep feeds a `NativeAudioSource` with synthesised frames and never opens
    /// a microphone at all, and "how the device is opened" is the entire content
    /// of this module. The sweep cannot see this.
    ///
    /// What can: open the *same* endpoint twice in shared mode, one asking for
    /// `AUDCLNT_STREAMOPTIONS_RAW` and one not, at the same time, against the
    /// same room. One stimulus, two paths. If the endpoint's effects are in the
    /// processed path, the two differ; the switch in the client is then doing
    /// something rather than reporting that it asked.
    ///
    /// **Read the numbers, not the result.** Two identical captures have two
    /// explanations — raw mode did nothing, or this machine has no effects to
    /// remove — and nothing here can separate them, because Windows exposes no
    /// "is there an APO on this endpoint" that we read. So the test asserts only
    /// that both paths delivered audio, and prints the rest. A *difference* is
    /// conclusive; a match is only evidence about this machine.
    ///
    /// Make noise while it runs — speech is the signal these effects are tuned
    /// for, and a silent room measures nothing on either path.
    ///
    /// ```text
    /// cargo test -p dioxusfun -- --ignored --nocapture raw_mode_changes
    /// ```
    #[test]
    #[ignore = "needs a microphone, a desktop session, and something to hear"]
    fn raw_mode_changes_the_signal_or_says_it_did_not() {
        let (raw_buf, cooked_buf) = (
            Arc::new(Mutex::new(Vec::<f32>::new())),
            Arc::new(Mutex::new(Vec::<f32>::new())),
        );
        let (raw_fmt, cooked_fmt) = (
            Arc::new(Mutex::new(String::new())),
            Arc::new(Mutex::new(String::new())),
        );

        // Both first, so the overlap is as close to the whole window as the two
        // opens allow. They are separate clients on one endpoint, which shared
        // mode permits — exclusive mode would make the second one fail.
        let raw = collect(true, raw_buf.clone(), raw_fmt.clone());
        let cooked = collect(false, cooked_buf.clone(), cooked_fmt.clone());

        std::thread::sleep(Duration::from_secs(6));
        drop(raw);
        drop(cooked);

        let (r, c) = (raw_buf.lock().clone(), cooked_buf.lock().clone());
        let (r_fmt, c_fmt) = (raw_fmt.lock().clone(), cooked_fmt.lock().clone());
        let (r_rms, c_rms) = (rms(&r), rms(&c));
        let r_peak = r.iter().fold(0.0f32, |a, s| a.max(s.abs()));
        let c_peak = c.iter().fold(0.0f32, |a, s| a.max(s.abs()));

        println!(
            "raw      : {} samples, {r_fmt}, rms {r_rms:.5}, peak {r_peak:.4}",
            r.len()
        );
        println!(
            "processed: {} samples, {c_fmt}, rms {c_rms:.5}, peak {c_peak:.4}",
            c.len()
        );
        if r_fmt != c_fmt {
            println!(
                "the engine named a different format for the two — that alone is                  the endpoint answering differently once the effects are out"
            );
        }
        if r_rms > 0.0 && c_rms > 0.0 {
            println!(
                "level difference: {:+.2} dB (raw relative to processed)",
                20.0 * (r_rms / c_rms).log10()
            );
        }
        // The reading that would otherwise be misread as an answer. A first run
        // here landed on the default capture device — a rear-panel line-in with
        // nothing plugged into it — and reported a flawless 0.00 dB difference
        // between the two paths. That is not "raw mode does nothing": it is two
        // captures of the same silence, and an effects chain has nothing to act
        // on either. Whoever runs this next needs to see that said out loud.
        if r_peak.max(c_peak) < SILENCE_FLOOR {
            println!(
                "NOTHING WAS HEARD: peak {:.4} on the louder path, below the                  {SILENCE_FLOOR} floor. This run compares silence to silence and                  says nothing about raw mode. Check which device the [rawmic]                  line above opened — the Windows default is often a line-in                  with nothing in it — then make noise and run it again.",
                r_peak.max(c_peak)
            );
        }

        // Sharper than comparing levels, and the thing that turned out to
        // matter: are the two streams the same *samples*? Two independent
        // clients on one endpoint getting bit-identical audio means the engine
        // handed both the same buffers, which is what "the flag changed
        // nothing" looks like from here — as opposed to two similar-sounding
        // but separately-processed streams.
        let overlap = r.len().min(c.len());
        let differing = r[..overlap]
            .iter()
            .zip(&c[..overlap])
            .filter(|(a, b)| a != b)
            .count();
        println!(
            "sample-for-sample over {overlap} overlapping: {differing} differ              ({:.4}%)",
            100.0 * differing as f32 / overlap.max(1) as f32
        );

        assert!(
            !r.is_empty(),
            "raw mode opened and delivered nothing — the microphone would be              silent for the whole call"
        );
        assert!(
            !c.is_empty(),
            "the processed path delivered nothing, so there is nothing to              compare raw mode against and the run says nothing either way"
        );
    }

    /// Does raw mode actually open, and does audio come out of it?
    ///
    /// The part that cannot be read off the source: `SetClientProperties` is
    /// entitled to fail per device, and a client that fails it is spent — so a
    /// driver that refuses raw mode is exactly the shape of bug that looks fine
    /// in review and delivers a dead microphone in a call. Speak while it runs;
    /// it reports the peak it saw.
    ///
    /// ```text
    /// cargo test -p dioxusfun -- --ignored --nocapture rawmic
    /// ```
    #[test]
    #[ignore = "needs a microphone and a desktop session"]
    fn rawmic_delivers_real_samples() {
        let (fatal_tx, mut fatal_rx) = unbounded_channel::<String>();
        let seen = Arc::new(AtomicU64::new(0));
        let peak = Arc::new(Mutex::new(0.0f32));
        let (seen_cb, peak_cb) = (seen.clone(), peak.clone());

        let capture = super::Capture::start(
            None,
            fatal_tx,
            Box::new(move |rate, channels| {
                println!("opened at {rate}Hz {channels}ch");
                Box::new(move |data: &[f32]| {
                    seen_cb.fetch_add(data.len() as u64, Ordering::Relaxed);
                    let mut p = peak_cb.lock();
                    for s in data {
                        *p = p.max(s.abs());
                    }
                })
            }),
        )
        .expect("open the microphone in raw mode");

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && seen.load(Ordering::Relaxed) < 48_000 {
            std::thread::sleep(Duration::from_millis(100));
        }
        drop(capture);

        if let Ok(err) = fatal_rx.try_recv() {
            panic!("the capture reported a fatal error: {err}");
        }
        let (samples, peak) = (seen.load(Ordering::Relaxed), *peak.lock());
        println!("samples={samples} peak={peak:.4}");
        assert!(
            samples > 0,
            "raw mode opened and delivered nothing at all — the microphone \
             would be silent for the whole call"
        );
        // Not asserted on: a silent room is a legitimate reading, and the point
        // of the run is the line above plus whatever the peak says to whoever
        // spoke into it.
    }
}
