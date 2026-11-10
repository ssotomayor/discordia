//! JNI entry points. `handlePush` may run in a process whose Activity never
//! started, so it takes its env, directory and language as arguments and must
//! not reach `ndk_context`, `android::config_dir` or Dioxus.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;

use jni::JNIEnv;
use jni::objects::{JObject, JString};
use jni::sys::{jboolean, jstring};

fn read(env: &mut JNIEnv<'_>, value: &JString<'_>) -> Option<String> {
    if value.is_null() {
        return None;
    }
    env.get_string(value).map(String::from).ok()
}

fn clear_exception(env: &mut JNIEnv<'_>) {
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_clear();
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_discordia_app_DioxusPushService_handlePush<'local>(
    mut env: JNIEnv<'local>,
    _service: JObject<'local>,
    _context: JObject<'local>,
    config_dir: JString<'local>,
    language: JString<'local>,
    data: JString<'local>,
) -> jstring {
    let alert = catch_unwind(AssertUnwindSafe(|| -> Option<String> {
        let dir = PathBuf::from(read(&mut env, &config_dir)?);
        let language = read(&mut env, &language).unwrap_or_default();
        let data = read(&mut env, &data)?;
        if !dir.is_absolute() {
            return None;
        }
        let alert = super::headless::handle_push(
            &dir,
            &data,
            &language,
            unix_now(),
            || {
                crate::android::headless_vault_passphrase(&mut env, &dir)
                    .map(|mut secret| std::mem::take(&mut *secret))
            },
            super::headless::fetch_event_blocking,
        )?;
        serde_json::to_string(&alert).ok()
    }));
    clear_exception(&mut env);
    match alert {
        Ok(Some(json)) => env
            .new_string(json)
            .map(JString::into_raw)
            .unwrap_or(std::ptr::null_mut()),
        _ => std::ptr::null_mut(),
    }
}

/// Only updates `device::platform()`; the mounted account's loop acts on it.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_discordia_app_DioxusNotifier_onPushPlatform<'local>(
    mut env: JNIEnv<'local>,
    _notifier: JObject<'local>,
    token: JString<'local>,
    package: JString<'local>,
    enabled: jboolean,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let token = read(&mut env, &token).unwrap_or_default();
        let package = read(&mut env, &package).unwrap_or_default();
        super::device::set_platform(super::device::platform_from(&token, &package, enabled != 0));
    }));
    clear_exception(&mut env);
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_dioxus_main_MainActivity_onMentionNotificationOpened<'local>(
    mut env: JNIEnv<'local>,
    _activity: JObject<'local>,
    account: JString<'local>,
    origin: JString<'local>,
    guild: JString<'local>,
    channel: JString<'local>,
    message: JString<'local>,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let parts = [&account, &origin, &guild, &channel, &message]
            .map(|value| read(&mut env, value).unwrap_or_default());
        let [account, origin, guild, channel, message] = &parts;
        if let Some(tap) = super::tap::from_parts(account, origin, guild, channel, message) {
            super::tap::open_mention(tap);
        }
    }));
    clear_exception(&mut env);
}
