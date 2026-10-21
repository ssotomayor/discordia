use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc as std_mpsc};
use std::time::Duration;

use tokio::sync::mpsc::UnboundedSender;
use windows::Win32::Foundation::{
    CloseHandle, E_ACCESSDENIED, HANDLE, PROPERTYKEY, WAIT_EVENT, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_E_DEVICE_IN_USE, AUDCLNT_E_SERVICE_NOT_RUNNING,
    AUDCLNT_E_UNSUPPORTED_FORMAT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    AUDCLNT_STREAMOPTIONS_NONE, AUDCLNT_STREAMOPTIONS_RAW, AudioCategory_Other,
    AudioClientProperties, DEVICE_STATE, DEVICE_STATE_ACTIVE, EDataFlow, ERole,
    IAudioCaptureClient, IAudioClient, IAudioClient2, IMMDevice, IMMDeviceEnumerator,
    IMMNotificationClient, IMMNotificationClient_Impl, MMDeviceEnumerator, WAVEFORMATEX,
    WAVEFORMATEXTENSIBLE, eCapture, eConsole,
};
use windows::Win32::System::Com::StructuredStorage::PropVariantClear;
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize, STGM_READ,
};
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForMultipleObjects};
use windows::Win32::System::Variant::VT_LPWSTR;
use windows::core::{GUID, HRESULT, Interface, PCWSTR, implement};

#[derive(Debug)]
pub enum CaptureFault {
    Device(String),
    DefaultChanged,
}

#[implement(IMMNotificationClient)]
struct DefaultEndpoint {
    opened_id: String,
    fatal: UnboundedSender<CaptureFault>,
    notified: AtomicBool,
}

impl DefaultEndpoint {
    fn changed(&self, flow: EDataFlow, role: ERole, id: Option<&str>) {
        if flow == eCapture
            && role == eConsole
            && id != Some(self.opened_id.as_str())
            && !self.notified.swap(true, Ordering::Relaxed)
            && let Err(error) = self.fatal.send(CaptureFault::DefaultChanged)
        {
            tracing::debug!(%error, "Microphone session already closed");
        }
    }
}

#[allow(non_snake_case)]
impl IMMNotificationClient_Impl for DefaultEndpoint_Impl {
    fn OnDefaultDeviceChanged(
        &self,
        flow: EDataFlow,
        role: ERole,
        id: &PCWSTR,
    ) -> windows::core::Result<()> {
        // SAFETY: Windows keeps the nullable notification string valid for this callback.
        let id = if id.is_null() {
            None
        } else {
            unsafe { id.to_string() }.ok()
        };
        self.changed(flow, role, id.as_deref());
        Ok(())
    }
    fn OnDeviceStateChanged(&self, _: &PCWSTR, _: DEVICE_STATE) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnDeviceAdded(&self, _: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnDeviceRemoved(&self, _: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnPropertyValueChanged(&self, _: &PCWSTR, _: &PROPERTYKEY) -> windows::core::Result<()> {
        Ok(())
    }
}

struct EndpointSubscription {
    enumerator: IMMDeviceEnumerator,
    notification: IMMNotificationClient,
}

impl Drop for EndpointSubscription {
    fn drop(&mut self) {
        // SAFETY: Both COM interfaces remain owned until the callback is unregistered.
        if let Err(error) = unsafe {
            self.enumerator
                .UnregisterEndpointNotificationCallback(&self.notification)
        } {
            tracing::warn!(%error, "Couldn't unregister microphone endpoint notification");
        }
    }
}

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

struct EventHandle(HANDLE);
// SAFETY: Windows events are thread-independent; ownership survives every outstanding wait.
unsafe impl Send for EventHandle {}
unsafe impl Sync for EventHandle {}

impl Drop for EventHandle {
    fn drop(&mut self) {
        // SAFETY: This owner is created only from a successful CreateEventW.
        if let Err(error) = unsafe { CloseHandle(self.0) } {
            tracing::warn!(%error, "Couldn't close microphone event");
        }
    }
}

pub struct Capture {
    pub device_name: String,
    shutdown: Arc<EventHandle>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Capture {
    pub fn start(
        device: Option<String>,
        fatal: UnboundedSender<CaptureFault>,
        sink: SinkBuilder,
    ) -> Result<Self, String> {
        Self::start_with_bypass(device, fatal, sink, true)
    }

    pub fn start_with_bypass(
        device: Option<String>,
        fatal: UnboundedSender<CaptureFault>,
        sink: SinkBuilder,
        bypass: bool,
    ) -> Result<Self, String> {
        let shutdown = unsafe { CreateEventW(None, true, false, None) }
            .map_err(|e| format!("create shutdown event: {e}"))?;
        let shutdown = Arc::new(EventHandle(shutdown));
        let worker_shutdown = Arc::clone(&shutdown);

        let (ready_tx, ready_rx) = std_mpsc::channel::<Result<String, String>>();
        let thread = std::thread::Builder::new()
            .name("dxf-rawmic-win".into())
            .spawn(move || run(device, fatal, sink, worker_shutdown, ready_tx, bypass))
            .map_err(|e| format!("spawn capture thread: {e}"))?;
        Self::wait_for_start(shutdown, thread, ready_rx, START_TIMEOUT)
    }

    fn wait_for_start(
        shutdown: Arc<EventHandle>,
        thread: std::thread::JoinHandle<()>,
        ready_rx: std_mpsc::Receiver<Result<String, String>>,
        timeout: Duration,
    ) -> Result<Self, String> {
        match ready_rx.recv_timeout(timeout) {
            Ok(Ok(device_name)) => Ok(Self {
                device_name,
                shutdown,
                thread: Some(thread),
            }),
            Ok(Err(e)) => Err(e),
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
    }
}

fn run(
    device: Option<String>,
    fatal: UnboundedSender<CaptureFault>,
    sink: SinkBuilder,
    shutdown: Arc<EventHandle>,
    ready: std_mpsc::Sender<Result<String, String>>,
    bypass: bool,
) {
    let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    if com.is_err() {
        let _ = ready.send(Err(format!("COM init failed ({com:?})")));
        return;
    }

    let started = match setup(device, bypass, &fatal) {
        Ok(s) => {
            if ready.send(Ok(s.device_name.clone())).is_err() {
                drop(s);
                unsafe { CoUninitialize() };
                return;
            }
            s
        }
        Err(e) => {
            let _ = ready.send(Err(e));
            unsafe { CoUninitialize() };
            return;
        }
    };

    let mut sink = sink(started.format.rate, started.format.channels);
    if let Err(e) = pump(&started, &mut sink, &shutdown) {
        let _ = fatal.send(CaptureFault::Device(e));
    }

    drop(started);
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
    device_name: String,
    client: IAudioClient,
    capture: IAudioCaptureClient,
    event: EventHandle,
    format: Format,
    subscription: Option<EndpointSubscription>,
}

impl Drop for Started {
    fn drop(&mut self) {
        drop(self.subscription.take());
        unsafe {
            let _ = self.client.Stop();
        }
    }
}

fn setup(
    device: Option<String>,
    bypass: bool,
    fatal: &UnboundedSender<CaptureFault>,
) -> Result<Started, String> {
    let (device, follows_default) = open(device)?;
    let subscription = if follows_default {
        Some(watch_default(&device, fatal.clone())?)
    } else {
        None
    };
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
    let event = EventHandle(event);
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
        device_name: name,
        client,
        capture,
        event,
        format,
        subscription,
    })
}

fn open(device: Option<String>) -> Result<(IMMDevice, bool), String> {
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
            Some(d) => return Ok((d, false)),
            None => eprintln!("[rawmic] '{want}' is not a capture endpoint; using the default"),
        }
    }

    unsafe { enumerator.GetDefaultAudioEndpoint(eCapture, eConsole) }
        .map(|device| (device, true))
        .map_err(|e| format!("no microphone available: {}", explain(e.code())))
}

fn endpoint_id(device: &IMMDevice) -> Result<String, String> {
    // SAFETY: GetId returns an allocated UTF-16 string, released after copying it.
    let pointer = unsafe { device.GetId() }.map_err(|e| format!("read endpoint ID: {e}"))?;
    let id = unsafe { pointer.to_string() }.map_err(|e| format!("decode endpoint ID: {e}"));
    unsafe { CoTaskMemFree(Some(pointer.0.cast())) };
    id
}

fn watch_default(
    device: &IMMDevice,
    fatal: UnboundedSender<CaptureFault>,
) -> Result<EndpointSubscription, String> {
    // SAFETY: This function runs on the capture thread with COM initialized.
    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
            .map_err(|e| format!("watch default microphone: {e}"))?;
    let opened_id = endpoint_id(device)?;
    let notification: IMMNotificationClient = DefaultEndpoint {
        opened_id: opened_id.clone(),
        fatal: fatal.clone(),
        notified: AtomicBool::new(false),
    }
    .into();
    // SAFETY: The subscription retains both interfaces through unregistration.
    unsafe { enumerator.RegisterEndpointNotificationCallback(&notification) }
        .map_err(|e| format!("register default microphone notification: {e}"))?;
    let subscription = EndpointSubscription {
        enumerator,
        notification,
    };
    // The default may have changed between opening the endpoint and registering its observer.
    let current = unsafe {
        subscription
            .enumerator
            .GetDefaultAudioEndpoint(eCapture, eConsole)
    }
    .ok()
    .and_then(|device| endpoint_id(&device).ok());
    if current.as_deref() != Some(opened_id.as_str())
        && let Err(error) = fatal.send(CaptureFault::DefaultChanged)
    {
        tracing::debug!(%error, "Microphone session already closed");
    }
    Ok(subscription)
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

fn pump(started: &Started, sink: &mut Sink, shutdown: &EventHandle) -> Result<(), String> {
    let mut scratch: Vec<f32> = Vec::with_capacity(4096);

    while wait_for_audio(&started.event, shutdown)? {
        drain(started, sink, &mut scratch)?;
    }
    eprintln!("[rawmic] windows raw capture stopped");
    Ok(())
}

fn wait_for_audio(event: &EventHandle, shutdown: &EventHandle) -> Result<bool, String> {
    let handles = [shutdown.0, event.0];
    // SAFETY: Both event owners remain borrowed for the entire wait.
    let waited = unsafe { WaitForMultipleObjects(&handles, false, WAIT_MS) };
    if waited == WAIT_OBJECT_0 {
        Ok(false)
    } else if waited == WAIT_EVENT(WAIT_OBJECT_0.0 + 1) {
        Ok(true)
    } else if waited == WAIT_TIMEOUT {
        Err("the microphone stopped delivering audio".into())
    } else {
        Err(format!("the audio engine stopped responding ({waited:?})"))
    }
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
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    use parking_lot::Mutex;
    use tokio::sync::mpsc::unbounded_channel;

    #[test]
    fn default_endpoint_notifications_ignore_outputs_roles_and_the_current_endpoint() {
        use windows::Win32::Media::Audio::{eCommunications, eRender};
        use windows::core::HSTRING;
        let (fatal, mut received) = unbounded_channel();
        let notification: IMMNotificationClient = DefaultEndpoint {
            opened_id: "endpoint-A".into(),
            fatal,
            notified: AtomicBool::new(false),
        }
        .into();
        let old = HSTRING::from("endpoint-A");
        let new = HSTRING::from("endpoint-B");
        // SAFETY: The interfaces and both strings remain owned throughout these callbacks.
        unsafe {
            notification
                .OnDefaultDeviceChanged(eRender, eConsole, &new)
                .unwrap();
            notification
                .OnDefaultDeviceChanged(eCapture, eCommunications, &new)
                .unwrap();
            notification
                .OnDefaultDeviceChanged(eCapture, eConsole, &old)
                .unwrap();
        }
        assert!(received.try_recv().is_err());
        unsafe {
            notification
                .OnDefaultDeviceChanged(eCapture, eConsole, &new)
                .unwrap();
            notification
                .OnDefaultDeviceChanged(eCapture, eConsole, &new)
                .unwrap();
        }
        assert!(matches!(
            received.try_recv(),
            Ok(CaptureFault::DefaultChanged)
        ));
        assert!(received.try_recv().is_err());
    }

    #[test]
    fn removing_the_default_endpoint_requests_capture_recovery() {
        let (fatal, mut received) = unbounded_channel();
        let notification: IMMNotificationClient = DefaultEndpoint {
            opened_id: "endpoint-A".into(),
            fatal,
            notified: AtomicBool::new(false),
        }
        .into();
        // SAFETY: A null ID is the documented notification when no endpoint remains.
        unsafe {
            notification
                .OnDefaultDeviceChanged(eCapture, eConsole, PCWSTR::null())
                .unwrap();
        }
        assert!(matches!(
            received.try_recv(),
            Ok(CaptureFault::DefaultChanged)
        ));
    }

    #[test]
    fn shutdown_takes_priority_over_ready_audio() {
        let audio =
            super::EventHandle(unsafe { super::CreateEventW(None, true, true, None) }.unwrap());
        let stop =
            super::EventHandle(unsafe { super::CreateEventW(None, true, true, None) }.unwrap());
        assert!(!super::wait_for_audio(&audio, &stop).unwrap());
    }

    #[test]
    fn startup_timeout_returns_while_the_driver_still_owns_its_shutdown_event() {
        let shutdown = Arc::new(super::EventHandle(
            unsafe { super::CreateEventW(None, true, false, None) }.unwrap(),
        ));
        let weak = Arc::downgrade(&shutdown);
        let worker_shutdown = Arc::clone(&shutdown);
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (closed_tx, closed_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            release_rx.recv().unwrap();
            drop(ready_tx);
            drop(worker_shutdown);
            closed_tx.send(()).unwrap();
        });
        let (finished_tx, finished_rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let result = super::Capture::wait_for_start(
                shutdown,
                worker,
                ready_rx,
                Duration::from_millis(5),
            );
            finished_tx.send(result.is_err()).unwrap();
        });
        let result = finished_rx.recv_timeout(Duration::from_secs(2));
        let worker_kept_event = weak.strong_count() == 1;
        release_tx.send(()).unwrap();
        waiter.join().unwrap();
        closed_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(result, Ok(true));
        assert!(worker_kept_event);
        assert!(weak.upgrade().is_none());
    }

    fn collect(
        bypass: bool,
        into: Arc<Mutex<Vec<f32>>>,
        fmt: Arc<Mutex<String>>,
    ) -> super::Capture {
        let (fatal_tx, _fatal_rx) = unbounded_channel::<CaptureFault>();
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
                "the engine named a different format for the two — that alone is the endpoint answering differently once the effects are out"
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
                "NOTHING WAS HEARD: peak {:.4} on the louder path, below the {SILENCE_FLOOR} floor. This run compares silence to silence and says nothing about raw mode. Check which device the [rawmic] line above opened — the Windows default is often a line-in with nothing in it — then make noise and run it again.",
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
            "sample-for-sample over {overlap} overlapping: {differing} differ ({:.4}%)",
            100.0 * differing as f32 / overlap.max(1) as f32
        );

        assert!(
            !r.is_empty(),
            "raw mode opened and delivered nothing — the microphone would be silent for the whole call"
        );
        assert!(
            !c.is_empty(),
            "the processed path delivered nothing, so there is nothing to compare raw mode against and the run says nothing either way"
        );
    }

    #[test]
    #[ignore = "needs a microphone and a desktop session"]
    fn rawmic_delivers_real_samples() {
        let (fatal_tx, mut fatal_rx) = unbounded_channel::<CaptureFault>();
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
            panic!("the capture reported a fatal error: {err:?}");
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
