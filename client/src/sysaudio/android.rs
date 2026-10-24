use jni::JNIEnv;
use jni::objects::{JObject, JShortArray, JString, JValue};
use jni::sys::{jboolean, jint};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use crate::audio_queue::{AudioSender, SAMPLES, offer};

struct Sink {
    slot: u32,
    audio: AudioSender<f32>,
    fatal: tokio::sync::mpsc::UnboundedSender<String>,
    audible: Arc<AtomicBool>,
}

static SINK: parking_lot::Mutex<Option<Sink>> = parking_lot::Mutex::new(None);

pub(super) struct AndroidCapture {
    slot: u32,
}

impl AndroidCapture {
    pub(super) async fn is_active(&self) -> Result<bool, String> {
        let slot = self.slot;
        crate::android::on_ui(move |env, activity| {
            env.call_method(
                activity,
                "playbackAudioActive",
                "(I)Z",
                &[JValue::Int(slot as jint)],
            )
            .and_then(|value| value.z())
            .map_err(|error| error.to_string())
        })
        .await
    }

    pub(super) async fn start(
        audio: AudioSender<f32>,
        fatal: tokio::sync::mpsc::UnboundedSender<String>,
    ) -> Result<Self, String> {
        let slot = crate::android::next_slot();
        *SINK.lock() = Some(Sink {
            slot,
            audio,
            fatal,
            audible: Arc::new(AtomicBool::new(false)),
        });
        let capture = Self { slot };
        let started = crate::android::on_ui(move |env, activity| {
            env.call_method(
                activity,
                "startPlaybackAudio",
                "(I)Z",
                &[JValue::Int(slot as jint)],
            )
            .and_then(|value| value.z())
            .map_err(|error| error.to_string())
        })
        .await?;
        if !started {
            return Err(
                "Android could not capture this app's playback audio. Video is still shared."
                    .into(),
            );
        }
        Ok(capture)
    }
}

impl Drop for AndroidCapture {
    fn drop(&mut self) {
        let slot = self.slot;
        {
            let mut sink = SINK.lock();
            if sink.as_ref().is_some_and(|sink| sink.slot == slot) {
                *sink = None;
            }
        }
        crate::platform::wry::prelude::dispatch(move |env, activity, _| {
            if let Err(error) = env.call_method(
                activity,
                "stopPlaybackAudioSlot",
                "(I)V",
                &[JValue::Int(slot as jint)],
            ) {
                tracing::warn!(%error, "Android playback capture cleanup failed");
            }
        });
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_dioxus_main_MainActivity_onPlaybackAudio(
    env: JNIEnv<'_>,
    _activity: JObject<'_>,
    slot: jint,
    buffer: JShortArray<'_>,
) -> jboolean {
    let audio = SINK
        .lock()
        .as_ref()
        .filter(|sink| sink.slot == slot as u32)
        .map(|sink| (sink.audio.clone(), sink.audible.clone()));
    let Some((audio, audible)) = audio else {
        return 0;
    };
    let result = (|| {
        if env
            .get_array_length(&buffer)
            .map_err(|error| error.to_string())?
            != SAMPLES as jint
        {
            return Err("invalid Android playback audio packet".to_string());
        }
        let mut pcm = [0_i16; SAMPLES];
        env.get_short_array_region(&buffer, 0, &mut pcm)
            .map_err(|error| error.to_string())?;
        if pcm.iter().any(|sample| sample.unsigned_abs() > 32)
            && !audible.swap(true, Ordering::Relaxed)
        {
            tracing::info!("Android playback capture produced non-silent samples");
        }
        Ok(offer(&audio, super::pcm::pcm16_frame(pcm)).is_ok())
    })();
    if env.exception_check().unwrap_or(false)
        && let Err(error) = env.exception_clear()
    {
        tracing::warn!(%error, "Could not clear an Android playback capture exception");
    }
    match result {
        Ok(alive) => u8::from(alive),
        Err(error) => {
            report_error(slot as u32, error);
            0
        }
    }
}

fn report_error(slot: u32, message: String) {
    let fatal = SINK
        .lock()
        .as_ref()
        .filter(|sink| sink.slot == slot)
        .map(|sink| sink.fatal.clone());
    if let Some(fatal) = fatal
        && fatal.send(message).is_err()
    {
        tracing::debug!("Android playback capture owner already stopped");
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_dioxus_main_MainActivity_onPlaybackAudioError(
    mut env: JNIEnv<'_>,
    _activity: JObject<'_>,
    slot: jint,
    message: JString<'_>,
) {
    let message = env
        .get_string(&message)
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "Android playback capture stopped".into());
    if env.exception_check().unwrap_or(false)
        && let Err(error) = env.exception_clear()
    {
        tracing::warn!(%error, "Could not clear an Android playback error callback exception");
    }
    report_error(slot as u32, message);
}
