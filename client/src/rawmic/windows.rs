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

pub type SinkBuilder = Box<dyn FnOnce(u32, u32) -> Sink + Send>;

pub type Sink = Box<dyn FnMut(&[f32]) + Send>;

const BUFFER_100NS: i64 = 200_000;

const WAIT_MS: u32 = 2_000;

const START_TIMEOUT: Duration = Duration::from_secs(5);

const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
const WAVE_FORMAT_PCM: u16 = 1;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
const SUBTYPE_PCM: GUID = GUID::from_u128(0x00000001_0000_0010_8000_00aa00389b71);
const SUBTYPE_IEEE_FLOAT: GUID = GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);

const PKEY_DEVICE_FRIENDLY_NAME: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
    pid: 14,
};

#[derive(Clone, Copy)]
struct SendHandle(HANDLE);
unsafe impl Send for SendHandle {}
unsafe impl Sync for SendHandle {}

pub struct Capture {
    shutdown: SendHandle,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Capture {
    pub fn start(
        device: Option<String>,
        fatal: UnboundedSender<String>,
        sink: SinkBuilder,
    ) -> Result<Self, String> {
        Self::start_with_bypass(device, fatal, sink, true)
    }

    pub fn start_with_bypass(
        device: Option<String>,
        fatal: UnboundedSender<String>,
        sink: SinkBuilder,
        bypass: bool,
    ) -> Result<Self, String> {
        let shutdown = unsafe { CreateEventW(None, true, false, None) }
            .map_err(|e| format!("create shutdown event: {e}"))?;
        let shutdown = SendHandle(shutdown);

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
                unsafe {
                    let _ = CloseHandle(shutdown.0);
                }
                Err(e)
            }
            Err(_) => {
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

fn run(
    device: Option<String>,
    fatal: UnboundedSender<String>,
    sink: SinkBuilder,
    shutdown: SendHandle,
    ready: std_mpsc::Sender<Result<(), String>>,
    bypass: bool,
) {
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
    if let Err(e) = pump(&started, &mut sink, shutdown) {
        let _ = fatal.send(e);
    }

    unsafe { CoUninitialize() };
}

struct Format {
    rate: u32,
    channels: u32,
    sample: Sample,
}

enum Sample {
    F32,
    I16,
    I32,
}

struct Started {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    event: SendHandle,
    format: Format,
}

impl Drop for Started {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
            let _ = CloseHandle(self.event.0);
        }
    }
}

fn setup(device: Option<String>, bypass: bool) -> Result<Started, String> {
    let device = open(device)?;
    let name = friendly_name(&device).unwrap_or_else(|| "<unknown>".into());

    let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }
        .map_err(|e| format!("open the microphone: {}", explain(e.code())))?;

    let client2: IAudioClient2 = client
        .cast()
        .map_err(|_| "this microphone's driver is too old for raw mode".to_string())?;
    let props = AudioClientProperties {
        cbSize: std::mem::size_of::<AudioClientProperties>() as u32,
        bIsOffload: false.into(),
        eCategory: AudioCategory_Other,
        Options: if bypass {
            AUDCLNT_STREAMOPTIONS_RAW
        } else {
            AUDCLNT_STREAMOPTIONS_NONE
        },
    };
    unsafe { client2.SetClientProperties(&props) }
        .map_err(|e| format!("Windows refused raw capture: {}", explain(e.code())))?;

    let mix = unsafe { client.GetMixFormat() }
        .map_err(|e| format!("read the microphone's format: {}", explain(e.code())))?;
    let format = describe(mix);

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
    unsafe { CoTaskMemFree(Some(mix.cast())) };
    init.map_err(|e| format!("start raw capture: {}", explain(e.code())))?;
    let format = format?;

    let event = unsafe { CreateEventW(None, false, false, None) }
        .map_err(|e| format!("create audio event: {e}"))?;
    let event = SendHandle(event);
    unsafe { client.SetEventHandle(event.0) }
        .map_err(|e| format!("set event handle: {}", explain(e.code())))?;
    let capture: IAudioCaptureClient = unsafe { client.GetService() }
        .map_err(|e| format!("get capture client: {}", explain(e.code())))?;
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

fn open(device: Option<String>) -> Result<IMMDevice, String> {
    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
            .map_err(|e| format!("enumerate audio devices: {}", explain(e.code())))?;

    if let Some(want) = device.as_deref() {
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

    unsafe { enumerator.GetDefaultAudioEndpoint(eCapture, eConsole) }
        .map_err(|e| format!("no microphone available: {}", explain(e.code())))
}

fn friendly_name(device: &IMMDevice) -> Option<String> {
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

fn describe(format: *const WAVEFORMATEX) -> Result<Format, String> {
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

fn pump(started: &Started, sink: &mut Sink, shutdown: SendHandle) -> Result<(), String> {
    let handles = [started.event.0, shutdown.0];
    let mut scratch: Vec<f32> = Vec::with_capacity(4096);

    loop {
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

fn drain(started: &Started, sink: &mut Sink, scratch: &mut Vec<f32>) -> Result<(), String> {
    let channels = started.format.channels as usize;
    loop {
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
                scratch.resize(n, 0.0);
            } else {
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

        if let Err(e) = unsafe { started.capture.ReleaseBuffer(frames) } {
            return Err(format!(
                "releasing a microphone buffer failed ({})",
                explain(e.code())
            ));
        }
    }
}

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

    const SILENCE_FLOOR: f32 = 0.02;

    fn rms(v: &[f32]) -> f32 {
        if v.is_empty() {
            return 0.0;
        }
        (v.iter().map(|s| s * s).sum::<f32>() / v.len() as f32).sqrt()
    }

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
        if r_peak.max(c_peak) < SILENCE_FLOOR {
            println!(
                "NOTHING WAS HEARD: peak {:.4} on the louder path, below the                  {SILENCE_FLOOR} floor. This run compares silence to silence and                  says nothing about raw mode. Check which device the [rawmic]                  line above opened — the Windows default is often a line-in                  with nothing in it — then make noise and run it again.",
                r_peak.max(c_peak)
            );
        }

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
    }
}
