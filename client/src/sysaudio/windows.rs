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

const RATE: u32 = 48_000;
const CHANNELS: u16 = 2;

const FORMAT_IEEE_FLOAT: u16 = 3;

const BUFFER_100NS: i64 = 200_000;

const WAIT_MS: u32 = 20;

const ACTIVATE_TIMEOUT: Duration = Duration::from_secs(5);

const MAX_SILENCE_FRAMES: u64 = 10;

#[derive(Clone, Copy)]
struct SendHandle(HANDLE);
unsafe impl Send for SendHandle {}
unsafe impl Sync for SendHandle {}

#[implement(IActivateAudioInterfaceCompletionHandler)]
struct ActivationDone(SendHandle);

impl IActivateAudioInterfaceCompletionHandler_Impl for ActivationDone_Impl {
    fn ActivateCompleted(
        &self,
        _op: Ref<'_, IActivateAudioInterfaceAsyncOperation>,
    ) -> WinResult<()> {
        unsafe {
            let _ = SetEvent(self.0.0);
        }
        Ok(())
    }
}

pub struct WinCapture {
    shutdown: SendHandle,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl WinCapture {
    pub fn start(
        tx: UnboundedSender<Vec<f32>>,
        fatal: UnboundedSender<String>,
    ) -> Result<Self, String> {
        let shutdown = unsafe { CreateEventW(None, true, false, None) }
            .map_err(|e| format!("create shutdown event: {e}"))?;
        let shutdown = SendHandle(shutdown);

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
                unsafe {
                    let _ = CloseHandle(shutdown.0);
                }
                Err(e)
            }
            Err(_) => {
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
    tx: UnboundedSender<Vec<f32>>,
    fatal: UnboundedSender<String>,
    shutdown: SendHandle,
    ready: std_mpsc::Sender<Result<(), String>>,
) {
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

    if let Err(e) = pump(started, tx, shutdown) {
        let _ = fatal.send(e);
    }

    unsafe { CoUninitialize() };
}

struct Started {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    event: SendHandle,
    bits: u16,
}

impl Drop for Started {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
            let _ = CloseHandle(self.event.0);
        }
    }
}

fn setup() -> Result<Started, String> {
    let mut client = activate()?;

    let (bits, tag) = match init(&client, 32, FORMAT_IEEE_FLOAT) {
        Ok(()) => (32u16, "f32"),
        Err(hr) if hr == AUDCLNT_E_UNSUPPORTED_FORMAT => {
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

    let event = unsafe { CreateEventW(None, false, false, None) }
        .map_err(|e| format!("create audio event: {e}"))?;
    let event = SendHandle(event);

    unsafe {
        client
            .SetEventHandle(event.0)
            .map_err(|e| format!("set event handle: {}", explain(e.code())))?;
    }
    let capture: IAudioCaptureClient = unsafe { client.GetService() }
        .map_err(|e| format!("get capture client: {}", explain(e.code())))?;
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

fn activation_params() -> *mut AUDIOCLIENT_ACTIVATION_PARAMS {
    Box::into_raw(Box::new(AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: unsafe { GetCurrentProcessId() },
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
            },
        },
    }))
}

fn settle_activation_wait(waited: WAIT_EVENT, done: SendHandle) -> Result<(), String> {
    if waited != WAIT_OBJECT_0 {
        return Err("Windows didn't answer the audio-capture request in time.".into());
    }
    unsafe {
        let _ = CloseHandle(done.0);
    }
    Ok(())
}

fn activate() -> Result<IAudioClient, String> {
    let params = activation_params();

    let mut prop = PROPVARIANT::default();
    unsafe {
        let inner = &mut prop.Anonymous.Anonymous;
        inner.vt = VT_BLOB;
        inner.Anonymous.blob.cbSize = std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32;
        inner.Anonymous.blob.pBlobData = params.cast::<u8>();
    }

    let done = unsafe { CreateEventW(None, false, false, None) }
        .map_err(|e| format!("create activation event: {e}"))?;
    let done = SendHandle(done);
    let handler: IActivateAudioInterfaceCompletionHandler = ActivationDone(done).into();

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

    let waited = unsafe { WaitForSingleObject(done.0, ACTIVATE_TIMEOUT.as_millis() as u32) };
    settle_activation_wait(waited, done)?;

    let mut hr = HRESULT(0);
    let mut iface = None;
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

fn pump(
    started: Started,
    tx: UnboundedSender<Vec<f32>>,
    shutdown: SendHandle,
) -> Result<(), String> {
    let mut cutter = FrameCutter::new(tx);
    let handles = [started.event.0, shutdown.0];
    let clock = Instant::now();

    loop {
        let waited = unsafe { WaitForMultipleObjects(&handles, false, WAIT_MS) };
        if waited == WAIT_EVENT(WAIT_OBJECT_0.0 + 1) {
            break;
        }
        if waited == WAIT_OBJECT_0 {
            if !drain(&started, &mut cutter)? {
                break;
            }
            continue;
        }
        if waited != WAIT_TIMEOUT {
            return Err(format!("the audio engine stopped responding ({waited:?})"));
        }
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

fn drain(started: &Started, cutter: &mut FrameCutter) -> Result<bool, String> {
    loop {
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
            cutter.push_silence(frames as usize)
        } else if data.is_null() {
            true
        } else if started.bits == 32 {
            let n = frames as usize * CHANNELS as usize;
            let samples = unsafe { std::slice::from_raw_parts(data.cast::<f32>(), n) };
            cutter.push_interleaved(samples, CHANNELS as usize)
        } else {
            let n = frames as usize * CHANNELS as usize;
            let samples = unsafe { std::slice::from_raw_parts(data.cast::<i16>(), n) };
            let scratch: Vec<f32> = samples
                .iter()
                .map(|s| *s as f32 / i16::MAX as f32)
                .collect();
            cutter.push_interleaved(&scratch, CHANNELS as usize)
        };

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

    #[test]
    fn every_activation_gets_its_own_blob() {
        let (first, second) = (super::activation_params(), super::activation_params());
        assert_ne!(
            first, second,
            "two activations were handed one address; whichever the engine is \
             still holding, the other one is a use-after-free waiting to happen"
        );
    }

    #[test]
    fn an_abandoned_activation_keeps_its_event_handle() {
        let event = unsafe { CreateEventW(None, false, false, None) }.expect("create event");

        let err = super::settle_activation_wait(WAIT_TIMEOUT, super::SendHandle(event))
            .expect_err("a wait that did not end in WAIT_OBJECT_0 must fail the activation");
        assert!(
            err.contains("in time"),
            "the timeout must reach the user as a timeout rather than as a \
             generic failure: {err}"
        );

        let mut flags = 0u32;
        let still_open = unsafe { GetHandleInformation(event, &mut flags) };
        assert!(
            still_open.is_ok(),
            "the abandoned activation's event was closed; the completion handler \
             can still signal it"
        );

        unsafe {
            let _ = CloseHandle(event);
        }
    }

    #[test]
    fn a_completed_activation_settles_cleanly() {
        let event = unsafe { CreateEventW(None, false, false, None) }.expect("create event");
        super::settle_activation_wait(WAIT_OBJECT_0, super::SendHandle(event))
            .expect("a handler that signalled must settle as success");
    }

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
        let _ = tone.wait();
        drop(capture);

        if let Ok(err) = fatal_rx.try_recv() {
            panic!("the capture reported a fatal error: {err}");
        }
        println!("frames={frames} samples={samples} peak={peak:.4}");

        assert!(
            frames > 0,
            "start() succeeded and delivered no frames at all — the same shape \
             as issue #162"
        );
        assert!(
            samples >= 4800,
            "only {samples} samples; the capture is starved"
        );
        assert!(
            peak > 0.01,
            "{frames} frames captured but every sample was ~zero (peak \
             {peak:.6}) — a track published from this would be silent"
        );
    }
}
