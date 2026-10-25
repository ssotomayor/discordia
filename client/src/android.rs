use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use jni::objects::{JByteArray, JByteBuffer, JObject, JString, JValue};
use jni::sys::{jint, jlong};
use jni::{JNIEnv, JavaVM};
use livekit::webrtc::video_frame::{I420Buffer, VideoRotation};
use zeroize::Zeroizing;

// Changing the alias or label would make existing encrypted identities unreadable.
const VAULT_ALIAS: &str = "com.discordia.identity-vault.hmac.v1";
const VAULT_LABEL: &[u8] = b"discordia/identity-vault/passphrase/v1";
const VAULT_MARKER: &str = "android-keystore-hmac-v1";
static CONFIG_DIR: OnceLock<PathBuf> = OnceLock::new();
static VAULT_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
static MEDIA_READY: AtomicBool = AtomicBool::new(false);
static MEDIA_INIT: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
static PERMISSION_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static DOCUMENT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static NETWORK_CHANGES: OnceLock<tokio::sync::watch::Sender<u64>> = OnceLock::new();

pub fn network_changes() -> tokio::sync::watch::Receiver<u64> {
    NETWORK_CHANGES
        .get_or_init(|| tokio::sync::watch::channel(0).0)
        .subscribe()
}

pub async fn background_app() -> Result<(), String> {
    on_ui(|env, activity| {
        env.call_method(activity, "moveTaskToBack", "(Z)Z", &[JValue::Bool(1)])
            .map_err(system_error)?;
        Ok(())
    })
    .await
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_dioxus_main_MainActivity_onNetworkChanged(
    _env: JNIEnv,
    _activity: JObject,
) {
    NETWORK_CHANGES
        .get_or_init(|| tokio::sync::watch::channel(0).0)
        .send_modify(|revision| *revision = revision.wrapping_add(1));
}

/// One native capturer per slot; the slot is echoed back with every frame, so a
/// capturer that outlives its publication cannot push into the next one.
static CAPTURE_SLOTS: AtomicU32 = AtomicU32::new(1);
static CAPTURE_SINKS: parking_lot::Mutex<Option<CaptureSinks>> = parking_lot::Mutex::new(None);

type CaptureSinks = std::collections::HashMap<u32, CaptureEntry>;

struct CaptureEntry {
    sink: Arc<dyn Fn(crate::sysvideo::Frame) + Send + Sync>,
    fatal: tokio::sync::mpsc::UnboundedSender<String>,
    admit: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
}

pub fn next_slot() -> u32 {
    CAPTURE_SLOTS.fetch_add(1, Ordering::Relaxed)
}

pub fn register_frame_sink(
    slot: u32,
    sink: Arc<dyn Fn(crate::sysvideo::Frame) + Send + Sync>,
    fatal: tokio::sync::mpsc::UnboundedSender<String>,
) {
    CAPTURE_SINKS
        .lock()
        .get_or_insert_with(CaptureSinks::new)
        .insert(
            slot,
            CaptureEntry {
                sink,
                fatal,
                admit: None,
            },
        );
}

pub fn filter_capture_frames(slot: u32, admit: Arc<dyn Fn() -> bool + Send + Sync>) {
    if let Some(entry) = CAPTURE_SINKS
        .lock()
        .as_mut()
        .and_then(|sinks| sinks.get_mut(&slot))
    {
        entry.admit = Some(admit);
    }
}

pub fn unregister_frame_sink(slot: u32) {
    if let Some(sinks) = CAPTURE_SINKS.lock().as_mut() {
        sinks.remove(&slot);
    }
}

fn capture_sink(slot: u32) -> Option<Arc<dyn Fn(crate::sysvideo::Frame) + Send + Sync>> {
    CAPTURE_SINKS
        .lock()
        .as_ref()
        .and_then(|sinks| sinks.get(&slot))
        .map(|entry| entry.sink.clone())
}

fn capture_fatal(slot: u32) -> Option<tokio::sync::mpsc::UnboundedSender<String>> {
    CAPTURE_SINKS
        .lock()
        .as_ref()
        .and_then(|sinks| sinks.get(&slot))
        .map(|entry| entry.fatal.clone())
}

pub(crate) async fn on_ui<T: Send + 'static>(
    action: impl FnOnce(&mut JNIEnv<'_>, &JObject<'_>) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    crate::platform::wry::prelude::dispatch(move |env, activity, _| {
        let mut result = action(env, activity);
        match env.exception_check() {
            Ok(true) => {
                if let Err(error) = env.exception_clear() {
                    result = Err(system_error(error));
                } else if result.is_ok() {
                    result = Err("Android platform call raised an exception.".into());
                }
            }
            Err(error) => result = Err(system_error(error)),
            Ok(false) => {}
        }
        if tx.send(result).is_err() {
            tracing::debug!("Android UI operation requester closed");
        }
    });
    tokio::time::timeout(Duration::from_secs(10), rx)
        .await
        .map_err(|_| "Android UI did not respond.".to_string())?
        .map_err(|_| "Android UI operation was interrupted.".to_string())?
}

pub async fn ensure_media_ready() -> Result<(), String> {
    MEDIA_INIT
        .get_or_try_init(|| async {
            on_ui(|env, activity| {
                for name in [
                    "livekit/org/jni_zero/JniZero",
                    "livekit/org/jni_zero/CommonApis",
                    "livekit/org/webrtc/ContextUtils",
                    "livekit/org/webrtc/DefaultVideoEncoderFactory",
                ] {
                    crate::platform::wry::prelude::find_class(env, activity, name.into())
                        .map_err(|_| "Android voice components are missing. Rebuild with scripts/build-android.py.".to_string())?;
                }
                // WebRTC FindClass needs an application Java frame, not the looper's boot loader.
                let ready = env
                    .call_method(activity, "initializeWebRtc", "()Z", &[])
                    .and_then(|value| value.z())
                    .map_err(system_error)?;
                if !ready {
                    return Err("Android voice initialization failed.".into());
                }
                MEDIA_READY.store(true, Ordering::Release);
                tracing::info!("Android WebRTC initialized with application context");
                Ok(())
            })
            .await
        })
        .await
        .map(|_| ())
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_dioxus_main_MainActivity_initializeWebRtc(
    mut env: JNIEnv<'_>,
    activity: JObject<'_>,
) -> jni::sys::jboolean {
    let result = (|| -> Result<bool, String> {
        let application = env
            .call_method(
                &activity,
                "getApplicationContext",
                "()Landroid/content/Context;",
                &[],
            )
            .and_then(|value| value.l())
            .map_err(system_error)?;
        let vm = env.get_java_vm().map_err(system_error)?;
        // SAFETY: Android supplies this JVM/frame; the application reference stays live until the JNI return.
        Ok(unsafe {
            webrtc_sys::android::ffi::init_android_context(
                vm.get_java_vm_pointer().cast(),
                application.as_raw() as usize,
            )
        })
    })();
    match env.exception_check() {
        Ok(true) => {
            if let Err(error) = env.exception_clear() {
                tracing::error!(%error, "failed to clear WebRTC initialization exception");
            }
            return jni::sys::JNI_FALSE;
        }
        Err(error) => {
            tracing::error!(%error, "failed to inspect WebRTC initialization exception");
            return jni::sys::JNI_FALSE;
        }
        Ok(false) => {}
    }
    match result {
        Ok(true) => jni::sys::JNI_TRUE,
        Ok(false) => jni::sys::JNI_FALSE,
        Err(error) => {
            tracing::error!(%error, "Android WebRTC initialization failed");
            jni::sys::JNI_FALSE
        }
    }
}

pub fn check_voice_ready() -> Result<(), String> {
    if MEDIA_READY.load(Ordering::Acquire) {
        Ok(())
    } else {
        Err("Android voice is not initialized yet.".into())
    }
}

pub fn check_microphone_permission() -> Result<(), String> {
    if permission_granted("android.permission.RECORD_AUDIO")? {
        Ok(())
    } else {
        Err("Microphone permission is required. Enable it in Android app settings.".into())
    }
}

pub fn check_camera_permission() -> Result<(), String> {
    if permission_granted("android.permission.CAMERA")? {
        Ok(())
    } else {
        Err("Camera permission is required. Enable it in Android app settings.".into())
    }
}

pub fn camera_permission_granted() -> bool {
    permission_granted("android.permission.CAMERA").unwrap_or(false)
}

fn permission_granted(permission: &str) -> Result<bool, String> {
    with_env(|env| {
        // SAFETY: Tao owns this Activity global reference for the current Activity lifetime.
        let activity =
            unsafe { JObject::from_raw(ndk_context::android_context().context().cast()) };
        let name = env.new_string(permission).map_err(system_error)?;
        let granted = env
            .call_method(
                &activity,
                "checkSelfPermission",
                "(Ljava/lang/String;)I",
                &[JValue::Object(&name)],
            )
            .and_then(|v| v.i())
            .map_err(system_error)?;
        Ok(granted == 0)
    })
}

/// Asked for on use, like the microphone: a camera starts only after the person agrees.
pub async fn prepare_camera() -> Result<(), String> {
    let _guard = PERMISSION_LOCK.lock().await;
    if check_camera_permission().is_ok() {
        return Ok(());
    }
    on_ui(|env, activity| {
        env.call_method(activity, "requestCameraPermission", "()V", &[])
            .map_err(system_error)?;
        Ok(())
    })
    .await?;
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let result = on_ui(|env, activity| {
                env.call_method(activity, "getCameraPermissionResult", "()I", &[])
                    .and_then(|v| v.i())
                    .map_err(system_error)
            })
            .await?;
            match result {
                1 => return check_camera_permission(),
                -1 => {
                    return Err(
                        "Camera permission is required. Enable it in Android app settings.".into(),
                    );
                }
                _ => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    })
    .await
    .map_err(|_| "Camera permission request timed out.".to_string())?
}

pub async fn prepare_voice() -> Result<(), String> {
    ensure_media_ready().await?;
    let _guard = PERMISSION_LOCK.lock().await;
    if check_microphone_permission().is_ok() {
        return Ok(());
    }
    on_ui(|env, activity| {
        env.call_method(activity, "requestMicrophonePermission", "()V", &[])
            .map_err(system_error)?;
        Ok(())
    })
    .await?;
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let result = on_ui(|env, activity| {
                env.call_method(activity, "getMicrophonePermissionResult", "()I", &[])
                    .and_then(|v| v.i())
                    .map_err(system_error)
            })
            .await?;
            match result {
                1 => return check_microphone_permission(),
                -1 => {
                    return Err(
                        "Microphone permission is required. Enable it in Android app settings."
                            .into(),
                    );
                }
                _ => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    })
    .await
    .map_err(|_| "Microphone permission request timed out.".to_string())?
}

pub struct BackgroundCall {
    slot: u32,
}

impl BackgroundCall {
    pub async fn start() -> Result<Self, String> {
        let guard = Self { slot: next_slot() };
        let slot = guard.slot;
        let started = on_ui(move |env, activity| {
            env.call_method(
                activity,
                "startVoiceService",
                "(I)Z",
                &[JValue::Int(slot as i32)],
            )
            .and_then(|value| value.z())
            .map_err(system_error)
        })
        .await?;
        if !started {
            return Err("Android could not keep this call active in the background. Open the app and try again.".into());
        }
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let result = on_ui(move |env, activity| {
                    env.call_method(
                        activity,
                        "voiceServiceResult",
                        "(I)I",
                        &[JValue::Int(slot as i32)],
                    )
                    .and_then(|value| value.i())
                    .map_err(system_error)
                })
                .await?;
                match result {
                    1 => return Ok(()),
                    -1 => return Err(
                        "Android refused the call foreground service. Open the app and try again."
                            .to_string(),
                    ),
                    _ => tokio::time::sleep(Duration::from_millis(50)).await,
                }
            }
        })
        .await
        .map_err(|_| "Android call foreground service timed out.".to_string())??;
        Ok(guard)
    }
}

impl Drop for BackgroundCall {
    fn drop(&mut self) {
        let slot = self.slot;
        crate::platform::wry::prelude::dispatch(move |env, activity, _| {
            if let Err(error) = env.call_method(
                activity,
                "stopVoiceService",
                "(I)V",
                &[JValue::Int(slot as i32)],
            ) {
                tracing::warn!(%error, "Android call foreground service cleanup failed");
            }
        });
    }
}

fn system_error(error: jni::errors::Error) -> String {
    format!("Android system call failed: {error}")
}

fn with_env<T>(action: impl FnOnce(&mut JNIEnv<'_>) -> Result<T, String>) -> Result<T, String> {
    let context = ndk_context::android_context();
    // SAFETY: Tao initializes this JVM before main and owns it throughout the Activity lifetime.
    let vm = unsafe { JavaVM::from_raw(context.vm().cast()) }.map_err(system_error)?;
    let mut env = vm.attach_current_thread().map_err(system_error)?;
    let result = env.with_local_frame(48, |env| Ok::<_, jni::errors::Error>(action(env)));
    if env.exception_check().map_err(system_error)? {
        env.exception_clear().map_err(system_error)?;
    }
    result.map_err(system_error)?
}

pub fn playback_capture_supported() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED.get_or_init(|| {
        with_env(|env| {
            env.get_static_field("android/os/Build$VERSION", "SDK_INT", "I")
                .and_then(|value| value.i())
                .map(|level| level >= 29)
                .map_err(system_error)
        })
        .unwrap_or(false)
    })
}

fn with_activity<T>(
    action: impl FnOnce(&mut JNIEnv<'_>, &JObject<'_>) -> Result<T, String>,
) -> Result<T, String> {
    with_env(|env| {
        // SAFETY: Tao owns this global reference; this wrapper does not delete it.
        let activity =
            unsafe { JObject::from_raw(ndk_context::android_context().context().cast()) };
        action(env, &activity)
    })
}

fn activity_string(method: &str) -> Result<String, String> {
    with_activity(|env, activity| {
        let value = env
            .call_method(activity, method, "()Ljava/lang/String;", &[])
            .and_then(|value| value.l())
            .map_err(system_error)?;
        let value = JString::from(value);
        env.get_string(&value)
            .map(|value| value.into())
            .map_err(system_error)
    })
}

pub fn system_language() -> String {
    activity_string("systemLanguage").unwrap_or_else(|error| {
        tracing::warn!(%error, "Couldn't read Android language");
        String::new()
    })
}

pub fn copy_text(text: &str) -> Result<bool, String> {
    with_activity(|env, activity| {
        let text = env.new_string(text).map_err(system_error)?;
        env.call_method(
            activity,
            "copyText",
            "(Ljava/lang/String;)Z",
            &[JValue::Object(&text)],
        )
        .and_then(|value| value.z())
        .map_err(system_error)
    })
}

pub fn read_clipboard() -> Result<serde_json::Value, String> {
    let value = activity_string("readClipboard")?;
    serde_json::from_str(&value).map_err(|error| error.to_string())
}

pub fn read_clipboard_image() -> Result<Vec<u8>, String> {
    with_activity(|env, activity| {
        let bytes = env
            .call_method(activity, "readClipboardImage", "()[B", &[])
            .and_then(|value| value.l())
            .map_err(system_error)?;
        env.convert_byte_array(JByteArray::from(bytes))
            .map_err(system_error)
    })
}

struct DocumentRequest(String);

impl Drop for DocumentRequest {
    fn drop(&mut self) {
        let request = self.0.clone();
        crate::platform::wry::prelude::dispatch(move |env, activity, _| {
            let result = (|| {
                let request = env.new_string(request)?;
                env.call_method(
                    activity,
                    "cancelDocument",
                    "(Ljava/lang/String;)V",
                    &[JValue::Object(&request)],
                )?;
                Ok::<_, jni::errors::Error>(())
            })();
            if let Err(error) = result {
                tracing::warn!(%error, "Couldn't close Android document request");
            }
            if let Err(error) = env.exception_clear() {
                tracing::warn!(%error, "Couldn't clear Android document exception");
            }
        });
    }
}

pub async fn document(
    name: String,
    extensions: String,
    bytes: Option<Vec<u8>>,
) -> Result<serde_json::Value, String> {
    let _lock = DOCUMENT_LOCK
        .try_lock()
        .map_err(|_| "A document request is already open.".to_string())?;
    let request = DocumentRequest(uuid::Uuid::new_v4().to_string());
    let id = request.0.clone();
    on_ui(move |env, activity| {
        let id = env.new_string(id).map_err(system_error)?;
        let name = env.new_string(name).map_err(system_error)?;
        let extensions = env.new_string(extensions).map_err(system_error)?;
        let bytes = match bytes {
            Some(bytes) => JObject::from(env.byte_array_from_slice(&bytes).map_err(system_error)?),
            None => JObject::null(),
        };
        env.call_method(
            activity,
            "startDocument",
            "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;[B)V",
            &[
                JValue::Object(&id),
                JValue::Object(&name),
                JValue::Object(&extensions),
                JValue::Object(&bytes),
            ],
        )
        .map_err(system_error)?;
        Ok(())
    })
    .await?;
    loop {
        let id = request.0.clone();
        let result = on_ui(move |env, activity| {
            let id = env.new_string(id).map_err(system_error)?;
            let result = env
                .call_method(
                    activity,
                    "takeDocumentResult",
                    "(Ljava/lang/String;)Ljava/lang/String;",
                    &[JValue::Object(&id)],
                )
                .and_then(|value| value.l())
                .map_err(system_error)?;
            if result.is_null() {
                return Ok(None);
            }
            let result = JString::from(result);
            let value: String = env.get_string(&result).map_err(system_error)?.into();
            Ok(Some(value))
        })
        .await?;
        if let Some(result) = result {
            let result: serde_json::Value =
                serde_json::from_str(&result).map_err(|error| error.to_string())?;
            if let Some(error) = result["error"].as_str() {
                return Err(error.to_owned());
            }
            return Ok(result);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

pub fn initialize() -> Result<(), String> {
    let path = with_env(|env| {
        // SAFETY: Tao holds this Context as a global reference; this wrapper never deletes it.
        let context = unsafe { JObject::from_raw(ndk_context::android_context().context().cast()) };
        let directory = env
            .call_method(&context, "getNoBackupFilesDir", "()Ljava/io/File;", &[])
            .and_then(|value| value.l())
            .map_err(system_error)?;
        let path = env
            .call_method(&directory, "getAbsolutePath", "()Ljava/lang/String;", &[])
            .and_then(|value| value.l())
            .map_err(system_error)?;
        let path = JString::from(path);
        let value: String = env.get_string(&path).map_err(system_error)?.into();
        Ok(PathBuf::from(value).join("discordia"))
    })?;
    if !path.is_absolute() {
        return Err("Android returned a nonabsolute private storage path".into());
    }
    std::fs::create_dir_all(&path).map_err(|error| format!("private storage: {error}"))?;
    if let Some(previous) = CONFIG_DIR.get() {
        if previous != &path {
            return Err("Android private storage changed during the session".into());
        }
    } else if CONFIG_DIR.set(path).is_err() {
        return Err("Android private storage initialized concurrently".into());
    }
    Ok(())
}

pub fn config_dir() -> PathBuf {
    match CONFIG_DIR.get() {
        Some(path) => path.clone(),
        None => {
            // Falling back to a working or temporary directory would misplace the identity.
            eprintln!("Android private storage was accessed before initialization");
            std::process::exit(1);
        }
    }
}

pub fn update_version() -> Result<Option<u64>, String> {
    with_activity(|env, activity| {
        let version = env
            .call_method(activity, "installedUpdateVersion", "()J", &[])
            .and_then(|value| value.j())
            .map_err(system_error)?;
        Ok((version > 0).then_some(version as u64))
    })
}

pub async fn install_update(path: &std::path::Path, version: u64) -> Result<bool, String> {
    let path = path
        .to_str()
        .ok_or("Invalid Android update path.")?
        .to_owned();
    let started = on_ui(move |env, activity| {
        let path = env.new_string(path).map_err(system_error)?;
        env.call_method(
            activity,
            "beginAppUpdate",
            "(Ljava/lang/String;J)Z",
            &[JValue::Object(&path), JValue::Long(version as i64)],
        )
        .and_then(|value| value.z())
        .map_err(system_error)
    })
    .await?;
    if !started {
        return Err("Android update installation is unavailable.".into());
    }
    struct Cancel;
    impl Drop for Cancel {
        fn drop(&mut self) {
            crate::platform::wry::prelude::dispatch(|env, activity, _| {
                if env
                    .call_method(activity, "cancelAppUpdate", "()V", &[])
                    .is_err()
                {
                    let _ = env.exception_clear();
                }
            });
        }
    }
    let _cancel = Cancel;
    tokio::time::timeout(Duration::from_secs(15 * 60), async {
        loop {
            let state = on_ui(|env, activity| {
                let value = env
                    .call_method(activity, "appUpdateState", "()Ljava/lang/String;", &[])
                    .and_then(|value| value.l())
                    .map_err(system_error)?;
                let value = JString::from(value);
                let text: String = env.get_string(&value).map_err(system_error)?.into();
                serde_json::from_str::<serde_json::Value>(&text).map_err(|error| error.to_string())
            })
            .await?;
            match state["status"].as_str() {
                Some("installed") => return Ok(true),
                Some("cancelled") => return Ok(false),
                Some("failed") => {
                    return Err(state["message"]
                        .as_str()
                        .unwrap_or("Android update failed.")
                        .to_owned());
                }
                _ => tokio::time::sleep(Duration::from_millis(500)).await,
            }
        }
    })
    .await
    .map_err(|_| "Android installer did not respond. You can try again.".to_owned())?
}

fn vault_state_exists() -> Result<bool, String> {
    let directory = config_dir();
    if directory.join("vault.backend").exists() || directory.join("identity.json").exists() {
        return Ok(true);
    }
    match std::fs::read_dir(directory.join("identities")) {
        Ok(mut entries) => match entries.next() {
            None => Ok(false),
            Some(Ok(_)) => Ok(true),
            Some(Err(error)) => Err(format!("read identities: {error}")),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("read identities: {error}")),
    }
}

pub fn vault_passphrase() -> Result<Zeroizing<String>, String> {
    let _guard = VAULT_LOCK.lock();
    let stored_state = vault_state_exists()?;
    let marker_path = config_dir().join("vault.backend");
    match std::fs::read_to_string(&marker_path) {
        Ok(marker) if marker.trim() != VAULT_MARKER => {
            return Err("The identity vault belongs to another storage backend.".into());
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("read vault backend: {error}")),
    }
    let bytes = with_env(|env| {
        let provider = env.new_string("AndroidKeyStore").map_err(system_error)?;
        let store = env
            .call_static_method(
                "java/security/KeyStore",
                "getInstance",
                "(Ljava/lang/String;)Ljava/security/KeyStore;",
                &[JValue::Object(&provider)],
            )
            .and_then(|value| value.l())
            .map_err(system_error)?;
        env.call_method(
            &store,
            "load",
            "(Ljava/security/KeyStore$LoadStoreParameter;)V",
            &[JValue::Object(&JObject::null())],
        )
        .map_err(system_error)?;
        let alias = env.new_string(VAULT_ALIAS).map_err(system_error)?;
        let present = env
            .call_method(
                &store,
                "containsAlias",
                "(Ljava/lang/String;)Z",
                &[JValue::Object(&alias)],
            )
            .and_then(|value| value.z())
            .map_err(system_error)?;
        if !present {
            if stored_state {
                return Err("The Android Keystore key is missing. Existing identities cannot be opened on this device.".into());
            }
            create_vault_key(env, &alias, &provider)?;
        }
        let key = env
            .call_method(
                &store,
                "getKey",
                "(Ljava/lang/String;[C)Ljava/security/Key;",
                &[JValue::Object(&alias), JValue::Object(&JObject::null())],
            )
            .and_then(|value| value.l())
            .map_err(system_error)?;
        let encoded = env
            .call_method(&key, "getEncoded", "()[B", &[])
            .and_then(|value| value.l())
            .map_err(system_error)?;
        if !encoded.is_null() {
            return Err("Android returned an exportable key instead of a Keystore key.".into());
        }
        let algorithm = env.new_string("HmacSHA256").map_err(system_error)?;
        let mac = env
            .call_static_method(
                "javax/crypto/Mac",
                "getInstance",
                "(Ljava/lang/String;)Ljavax/crypto/Mac;",
                &[JValue::Object(&algorithm)],
            )
            .and_then(|value| value.l())
            .map_err(system_error)?;
        env.call_method(
            &mac,
            "init",
            "(Ljava/security/Key;)V",
            &[JValue::Object(&key)],
        )
        .map_err(system_error)?;
        let label = env
            .byte_array_from_slice(VAULT_LABEL)
            .map_err(system_error)?;
        let result = env
            .call_method(&mac, "doFinal", "([B)[B", &[JValue::Object(&label)])
            .and_then(|value| value.l())
            .map_err(system_error)?;
        let result = JByteArray::from(result);
        let bytes = Zeroizing::new(env.convert_byte_array(&result).map_err(system_error)?);
        if bytes.len() != 32 {
            return Err("Android Keystore returned an invalid vault key length.".into());
        }
        env.set_byte_array_region(&result, 0, &[0; 32])
            .map_err(system_error)?;
        Ok(bytes)
    })?;
    crate::identity::write_private(&marker_path, VAULT_MARKER)?;
    Ok(Zeroizing::new(hex::encode(bytes.as_slice())))
}

fn create_vault_key(
    env: &mut JNIEnv<'_>,
    alias: &JString<'_>,
    provider: &JString<'_>,
) -> Result<(), String> {
    let algorithm = env.new_string("HmacSHA256").map_err(system_error)?;
    let generator = env
        .call_static_method(
            "javax/crypto/KeyGenerator",
            "getInstance",
            "(Ljava/lang/String;Ljava/lang/String;)Ljavax/crypto/KeyGenerator;",
            &[JValue::Object(&algorithm), JValue::Object(provider)],
        )
        .and_then(|value| value.l())
        .map_err(system_error)?;
    let purpose = env
        .get_static_field(
            "android/security/keystore/KeyProperties",
            "PURPOSE_SIGN",
            "I",
        )
        .and_then(|value| value.i())
        .map_err(system_error)?;
    let builder = env
        .new_object(
            "android/security/keystore/KeyGenParameterSpec$Builder",
            "(Ljava/lang/String;I)V",
            &[JValue::Object(alias), JValue::Int(purpose)],
        )
        .map_err(system_error)?;
    env.call_method(
        &builder,
        "setKeySize",
        "(I)Landroid/security/keystore/KeyGenParameterSpec$Builder;",
        &[JValue::Int(256)],
    )
    .map_err(system_error)?;
    let spec = env
        .call_method(
            &builder,
            "build",
            "()Landroid/security/keystore/KeyGenParameterSpec;",
            &[],
        )
        .and_then(|value| value.l())
        .map_err(system_error)?;
    env.call_method(
        &generator,
        "init",
        "(Ljava/security/spec/AlgorithmParameterSpec;)V",
        &[JValue::Object(&spec)],
    )
    .map_err(system_error)?;
    env.call_method(&generator, "generateKey", "()Ljavax/crypto/SecretKey;", &[])
        .map_err(system_error)?;
    Ok(())
}

pub fn display_size() -> Result<(u32, u32), String> {
    let value = activity_string("displaySize")?;
    let (width, height) = value
        .split_once('x')
        .ok_or("Android returned an invalid display size")?;
    let width = width
        .parse()
        .map_err(|_| "Android returned an invalid display width".to_string())?;
    let height = height
        .parse()
        .map_err(|_| "Android returned an invalid display height".to_string())?;
    Ok((width, height))
}

pub fn camera_devices() -> Result<Vec<crate::state::CameraDevice>, String> {
    let value = activity_string("cameraDevices")?;
    let devices: Vec<serde_json::Value> =
        serde_json::from_str(&value).map_err(|error| error.to_string())?;
    Ok(devices
        .into_iter()
        .filter_map(|device| {
            Some(crate::state::CameraDevice {
                id: device.get("id")?.as_str()?.to_string(),
                label: device
                    .get("label")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string(),
            })
        })
        .collect())
}

/// Asks for the projection on its own, before the app announces a share.
pub async fn request_screen_capture() -> Result<(), String> {
    let launched = on_ui(|env, activity| {
        env.call_method(activity, "requestScreenCapture", "()Z", &[])
            .and_then(|value| value.z())
            .map_err(system_error)
    })
    .await?;
    if !launched {
        return Err("Android refused to open the screen capture dialog.".into());
    }
    await_screen_result().await
}

pub async fn start_screen_share(
    slot: u32,
    width: u32,
    height: u32,
    fps: u32,
) -> Result<(), String> {
    let launched = on_ui(move |env, activity| {
        env.call_method(
            activity,
            "startScreenShare",
            "(IIII)Z",
            &[
                JValue::Int(slot as jint),
                JValue::Int(width as jint),
                JValue::Int(height as jint),
                JValue::Int(fps as jint),
            ],
        )
        .and_then(|value| value.z())
        .map_err(system_error)
    })
    .await?;
    if !launched {
        return Err("Android refused to open the screen capture dialog.".into());
    }
    await_screen_result().await
}

pub async fn screen_capture_pressure() -> Result<u8, String> {
    on_ui(|env, activity| {
        env.call_method(activity, "screenCapturePressure", "()I", &[])
            .and_then(|value| value.i())
            .map(|value| value as u8)
            .map_err(system_error)
    })
    .await
}

pub async fn resize_screen_share(
    slot: u32,
    settings: crate::sysvideo::Settings,
) -> Result<(), String> {
    let applied = on_ui(move |env, activity| {
        env.call_method(
            activity,
            "resizeScreenShare",
            "(IIII)Z",
            &[
                JValue::Int(slot as jint),
                JValue::Int(settings.width as jint),
                JValue::Int(settings.height as jint),
                JValue::Int(settings.fps as jint),
            ],
        )
        .and_then(|value| value.z())
        .map_err(system_error)
    })
    .await?;
    if applied {
        Ok(())
    } else {
        Err("Android screen capture could not change its budget".into())
    }
}

async fn await_screen_result() -> Result<(), String> {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let result = on_ui(|env, activity| {
                env.call_method(activity, "takeScreenShareResult", "()I", &[])
                    .and_then(|value| value.i())
                    .map_err(system_error)
            })
            .await?;
            match result {
                1 => return Ok(()),
                -1 => return Err("Screen sharing was refused.".into()),
                _ => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    })
    .await
    .map_err(|_| "The screen capture request timed out.".to_string())?
}

pub async fn start_camera_capture(
    slot: u32,
    device: Option<String>,
    width: u32,
    height: u32,
    fps: u32,
) -> Result<(), String> {
    let started = on_ui(move |env, activity| {
        let device = match device {
            Some(device) => JObject::from(env.new_string(device).map_err(system_error)?),
            None => JObject::null(),
        };
        env.call_method(
            activity,
            "startCameraCapture",
            "(ILjava/lang/String;III)Z",
            &[
                JValue::Int(slot as jint),
                JValue::Object(&device),
                JValue::Int(width as jint),
                JValue::Int(height as jint),
                JValue::Int(fps as jint),
            ],
        )
        .and_then(|value| value.z())
        .map_err(system_error)
    })
    .await?;
    if started {
        Ok(())
    } else {
        Err("The camera could not be opened.".into())
    }
}

pub fn stop_screen_share(slot: u32) {
    dispatch_capture("stopScreenShareSlot", slot);
}

pub fn stop_camera_capture(slot: u32) {
    dispatch_capture("stopCameraCaptureSlot", slot);
}

pub async fn stop_camera_capture_wait(slot: u32) -> Result<(), String> {
    on_ui(move |env, activity| {
        env.call_method(
            activity,
            "stopCameraCaptureSlot",
            "(I)V",
            &[JValue::Int(slot as jint)],
        )
        .map(|_| ())
        .map_err(system_error)
    })
    .await
}

/// Stopping a capturer touches its own handler thread, so it is posted to the UI
/// thread and never waited on: callers may be dropping a publication.
fn dispatch_capture(method: &'static str, slot: u32) {
    crate::platform::wry::prelude::dispatch(move |env, activity, _| {
        if let Err(error) = env.call_method(activity, method, "(I)V", &[JValue::Int(slot as jint)])
        {
            tracing::warn!(%error, "Couldn't stop an Android capture");
        }
        if let Err(error) = env.exception_clear() {
            tracing::warn!(%error, "Couldn't clear an Android capture exception");
        }
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_dioxus_main_MainActivity_shouldCaptureVideoFrame(
    _env: JNIEnv<'_>,
    _activity: JObject<'_>,
    slot: jint,
) -> jni::sys::jboolean {
    let admit = CAPTURE_SINKS
        .lock()
        .as_ref()
        .and_then(|sinks| sinks.get(&(slot as u32)).map(|entry| entry.admit.clone()));
    u8::from(admit.is_some_and(|filter| filter.is_none_or(|filter| filter())))
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_dioxus_main_MainActivity_onVideoFrame(
    mut env: JNIEnv<'_>,
    _activity: JObject<'_>,
    slot: jint,
    y: JObject<'_>,
    u: JObject<'_>,
    v: JObject<'_>,
    stride_y: jint,
    stride_u: jint,
    stride_v: jint,
    width: jint,
    height: jint,
    rotation: jint,
    timestamp_ns: jlong,
) {
    let Some(sink) = capture_sink(slot as u32) else {
        return;
    };
    let frame = read_frame(
        &mut env,
        y,
        u,
        v,
        [stride_y, stride_u, stride_v],
        width,
        height,
        rotation,
        timestamp_ns,
    );
    // A capturer thread must never return to Java with a pending exception.
    if env.exception_check().unwrap_or(false)
        && let Err(error) = env.exception_clear()
    {
        tracing::warn!(%error, "Couldn't clear an Android frame exception");
    }
    match frame {
        Ok(frame) => sink(frame),
        Err(error) => tracing::warn!(%error, "dropped an Android video frame"),
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_dioxus_main_MainActivity_onCaptureStopped(
    _env: JNIEnv<'_>,
    _activity: JObject<'_>,
    slot: jint,
) {
    if let Some(fatal) = capture_fatal(slot as u32) {
        let _ = fatal.send("the capture ended".into());
    }
}

#[allow(clippy::too_many_arguments)]
fn read_frame(
    env: &mut JNIEnv<'_>,
    y: JObject<'_>,
    u: JObject<'_>,
    v: JObject<'_>,
    strides: [jint; 3],
    width: jint,
    height: jint,
    rotation: jint,
    timestamp_ns: jlong,
) -> Result<crate::sysvideo::Frame, String> {
    let width = u32::try_from(width).map_err(|_| "invalid frame width".to_string())?;
    let height = u32::try_from(height).map_err(|_| "invalid frame height".to_string())?;
    if width == 0
        || height == 0
        || width > 8192
        || height > 8192
        || u64::from(width) * u64::from(height) > 16_777_216
    {
        return Err("invalid frame size".into());
    }
    let mut buffer = I420Buffer::new(width, height);
    let (stride_y, stride_u, stride_v) = buffer.strides();
    let (out_y, out_u, out_v) = buffer.data_mut();
    copy_plane(env, &y, strides[0], height, out_y, stride_y)?;
    copy_plane(env, &u, strides[1], height.div_ceil(2), out_u, stride_u)?;
    copy_plane(env, &v, strides[2], height.div_ceil(2), out_v, stride_v)?;
    let rotation = match rotation.rem_euclid(360) {
        90 => VideoRotation::VideoRotation90,
        180 => VideoRotation::VideoRotation180,
        270 => VideoRotation::VideoRotation270,
        _ => VideoRotation::VideoRotation0,
    };
    let timestamp_us = if timestamp_ns > 0 {
        timestamp_ns / 1000
    } else {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_micros() as i64)
            .unwrap_or(0)
    };
    Ok(crate::sysvideo::Frame {
        buffer,
        timestamp_us,
        rotation,
    })
}

fn copy_plane(
    env: &mut JNIEnv<'_>,
    plane: &JObject<'_>,
    source_stride: jint,
    rows: u32,
    destination: &mut [u8],
    destination_stride: u32,
) -> Result<(), String> {
    let source_stride =
        usize::try_from(source_stride).map_err(|_| "invalid plane stride".to_string())?;
    let rows = usize::try_from(rows).map_err(|_| "invalid plane height".to_string())?;
    let buffer = <&JByteBuffer<'_>>::from(plane);
    let address = env
        .get_direct_buffer_address(buffer)
        .map_err(system_error)?;
    let capacity = env
        .get_direct_buffer_capacity(buffer)
        .map_err(system_error)?;
    // SAFETY: a direct ByteBuffer stays put for the duration of the JNI call.
    let source = unsafe { std::slice::from_raw_parts(address, capacity) };
    crate::sysvideo::copy_i420_plane_into(
        source,
        source_stride,
        rows,
        destination,
        destination_stride,
    )
}
