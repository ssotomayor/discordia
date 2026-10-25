use crate::nostr::{nip17::ChatMessage, service::conversation_id};
use crate::state::{AppState, DmStanding};

#[derive(serde::Serialize)]
struct Notice {
    account: String,
    peer: String,
    sender: String,
    message: String,
}

fn notice(state: &AppState, message: &ChatMessage, account: &str, live: bool) -> Option<Notice> {
    let channel = conversation_id(&message.peer);
    let at = message.created_at - state.clock_offset(&message.author);
    if !live
        || message.author == account
        || message.author != message.peer
        || state.dm_standing(&message.peer) != DmStanding::Inbox
        || state.is_muted(channel)
        || state
            .dm_read_at
            .get(&message.peer)
            .is_some_and(|read| at < *read)
    {
        return None;
    }
    Some(Notice {
        account: account.into(),
        peer: message.peer.clone(),
        sender: state
            .display_name(&message.peer)
            .chars()
            .filter(|c| !c.is_control())
            .take(100)
            .collect(),
        message: crate::i18n::text("New private message", &[]),
    })
}

fn open_inbox(state: &mut AppState, peer: &str) -> bool {
    if state.dm_standing(peer) != DmStanding::Inbox {
        return false;
    }
    let channel = conversation_id(peer);
    if !state.dms.iter().any(|dm| dm.channel_id == channel) {
        state.dms.push(crate::state::DmInfo {
            channel_id: channel,
            other_pubkey: peer.into(),
        });
    }
    state.dm_mode = true;
    state.selected_channel = Some(channel);
    state.dm_notification_open = Some(channel);
    true
}

#[cfg(target_os = "android")]
pub(crate) fn received(state: &AppState, message: &ChatMessage, account: &str, live: bool) {
    if let Some(notice) = notice(state, message, account, live) {
        match serde_json::to_string(&notice) {
            Ok(payload) => native::dispatch("showDmNotification", payload),
            Err(error) => tracing::warn!(%error, "Could not prepare DM notification"),
        }
    }
}

#[cfg(target_os = "android")]
pub(crate) use native::{NotificationSettings, use_notifications};

#[cfg(target_os = "android")]
mod native {
    use super::*;
    use dioxus::prelude::*;
    use jni::{
        JNIEnv,
        objects::{JObject, JString, JValue},
    };
    use std::sync::OnceLock;
    use tokio::sync::watch;

    #[derive(Clone, Default)]
    struct NativeState {
        resumed: bool,
        open: Option<(String, String)>,
    }

    fn events() -> &'static watch::Sender<NativeState> {
        static EVENTS: OnceLock<watch::Sender<NativeState>> = OnceLock::new();
        EVENTS.get_or_init(|| watch::channel(NativeState::default()).0)
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_dioxus_main_MainActivity_onDmNotificationVisibility(
        _env: JNIEnv<'_>,
        _activity: JObject<'_>,
        resumed: jni::sys::jboolean,
    ) {
        events().send_modify(|state| state.resumed = resumed != 0);
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_dioxus_main_MainActivity_onDmNotificationOpened(
        mut env: JNIEnv<'_>,
        _activity: JObject<'_>,
        account: JString<'_>,
        peer: JString<'_>,
    ) {
        let result = (|| -> Result<(String, String), jni::errors::Error> {
            Ok((
                env.get_string(&account)?.into(),
                env.get_string(&peer)?.into(),
            ))
        })();
        if env.exception_check().unwrap_or(false) {
            if let Err(error) = env.exception_clear() {
                tracing::warn!(%error, "Could not clear notification JNI exception");
            }
        }
        if let Ok((account, peer)) = result {
            if account.parse::<secp256k1::XOnlyPublicKey>().is_ok()
                && peer.parse::<secp256k1::XOnlyPublicKey>().is_ok()
            {
                events().send_modify(|state| state.open = Some((account, peer)));
            }
        }
    }

    pub(super) fn dispatch(method: &'static str, value: String) {
        crate::platform::wry::prelude::dispatch(move |env, activity, _| {
            let result = env.new_string(value).and_then(|value| {
                env.call_method(
                    activity,
                    method,
                    "(Ljava/lang/String;)V",
                    &[JValue::Object(&value)],
                )
                .map(|_| ())
            });
            if let Err(error) = result {
                tracing::warn!(%error, method, "Android notification operation failed");
            }
            if env.exception_check().unwrap_or(false) {
                if let Err(error) = env.exception_clear() {
                    tracing::warn!(%error, "Could not clear notification exception");
                }
            }
        });
    }

    pub(crate) fn use_notifications(mut state: Signal<AppState>, account: String) {
        let setup_account = account.clone();
        use_hook(move || {
            let channel = crate::i18n::text("Private messages", &[]);
            crate::platform::wry::prelude::dispatch(move |env, activity, _| {
                let result = (|| -> Result<(), jni::errors::Error> {
                    let account = env.new_string(setup_account)?;
                    let channel = env.new_string(channel)?;
                    env.call_method(
                        activity,
                        "setDmNotificationAccount",
                        "(Ljava/lang/String;Ljava/lang/String;)V",
                        &[JValue::Object(&account), JValue::Object(&channel)],
                    )?;
                    Ok(())
                })();
                if let Err(error) = result {
                    tracing::warn!(%error, "Could not initialize DM notifications");
                }
                if env.exception_check().unwrap_or(false) {
                    if let Err(error) = env.exception_clear() {
                        tracing::warn!(%error, "Could not clear notification exception");
                    }
                }
            });
        });
        let mut pending = use_signal(|| None::<String>);
        let active_account = account.clone();
        use_hook(move || {
            crate::service_task::spawn(async move {
                let mut receiver = events().subscribe();
                loop {
                    let event = receiver.borrow_and_update().clone();
                    if state.peek().app_foreground != event.resumed {
                        state.write().app_foreground = event.resumed;
                    }
                    if let Some((owner, peer)) = event.open {
                        if owner == active_account {
                            pending.set(Some(peer.clone()));
                            events().send_if_modified(|event| {
                                if event.open.as_ref() == Some(&(owner.clone(), peer.clone())) {
                                    event.open = None;
                                    true
                                } else {
                                    false
                                }
                            });
                        }
                    }
                    if receiver.changed().await.is_err() {
                        break;
                    }
                }
            })
        });
        use_effect(move || {
            let Some(peer) = pending() else {
                return;
            };
            if state.read().dm_standing(&peer) != DmStanding::Inbox {
                return;
            }
            if open_inbox(&mut state.write(), &peer) {
                pending.set(None);
            }
        });
        let viewing = use_memo(move || {
            let current = state.read();
            let peer = if current.dm_mode && current.dm_pane_open {
                current
                    .selected_channel
                    .and_then(|channel| current.dm_of(channel))
                    .map(|dm| dm.other_pubkey.clone())
                    .unwrap_or_default()
            } else {
                String::new()
            };
            (peer, current.app_foreground)
        });
        use_effect(move || {
            let (peer, foreground) = viewing();
            if foreground && !peer.is_empty() {
                state.write().mark_dm_read(conversation_id(&peer));
            }
            dispatch("dismissDmNotification", peer);
        });
        use_drop(move || dispatch("clearDmNotificationAccount", account));
    }

    async fn notification_state(enable: Option<bool>) -> Result<String, String> {
        crate::android::on_ui(move |env, activity| {
            let value = if let Some(enable) = enable {
                env.call_method(
                    activity,
                    "enableDmNotifications",
                    "(Z)Ljava/lang/String;",
                    &[JValue::Bool(u8::from(enable))],
                )
            } else {
                env.call_method(activity, "dmNotificationState", "()Ljava/lang/String;", &[])
            }
            .and_then(|value| value.l())
            .map_err(|error| error.to_string())?;
            env.get_string(&JString::from(value))
                .map(|value| value.into())
                .map_err(|error| error.to_string())
        })
        .await
    }

    #[component]
    pub(crate) fn NotificationSettings() -> Element {
        let mut status = use_signal(|| "loading".to_string());
        let mut busy = use_signal(|| false);
        use_future(move || async move {
            status.set(
                notification_state(None)
                    .await
                    .unwrap_or_else(|_| "unavailable".into()),
            );
        });
        let enabled = status() == "enabled";
        rsx! {
            section { class: "mt-6 border-t border-[var(--border)] pt-4",
                h3 { class: "font-semibold", {crate::i18n::text("DM notifications", &[])} }
                p { class: "mt-2 text-sm text-[var(--text-muted)]", {crate::i18n::text("Notify about new private messages while Discordia is running. Notifications may stop if Android closes the app.", &[])} }
                p { class: "mt-2 text-sm", {crate::i18n::text(if enabled { "Enabled" } else { "Disabled" }, &[])} }
                if status() == "blocked" {
                    p { class: "text-sm text-[var(--text-muted)]", {crate::i18n::text("Allow notifications in Android settings to receive DM alerts.", &[])} }
                }
                if status() == "unavailable" {
                    p { role: "alert", class: "text-sm", {crate::i18n::text("Notification settings could not be read. Try again.", &[])} }
                }
                button { class: "mt-3 min-h-[44px] rounded border border-[var(--border)] px-3", disabled: busy() || status() == "loading",
                    onclick: move |_| {
                        busy.set(true);
                        spawn(async move {
                            let result = async {
                                let mut current = notification_state(Some(!enabled)).await?;
                                while current == "pending" {
                                    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                                    current = notification_state(None).await?;
                                }
                                Ok::<_, String>(current)
                            };
                            let result = tokio::time::timeout(std::time::Duration::from_secs(90), result).await;
                            status.set(match result { Ok(Ok(status)) => status, _ => "unavailable".into() });
                            busy.set(false);
                        });
                    },
                    {crate::i18n::text(if enabled { "Disable notifications" } else { "Enable notifications" }, &[])}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message() -> ChatMessage {
        ChatMessage {
            id: "event".into(),
            author: "peer".into(),
            peer: "peer".into(),
            content: "private content".into(),
            created_at: 100,
            reply_to: None,
            emoji: vec![],
        }
    }

    #[test]
    fn only_new_unread_inbox_messages_can_notify() {
        let mut state = AppState::empty();
        let mut message = message();
        assert!(notice(&state, &message, "self", true).is_none());
        state.dm_accepted.insert("peer".into());
        assert!(notice(&state, &message, "self", true).is_some());
        assert!(notice(&state, &message, "self", false).is_none());
        state.dm_read_at.insert("peer".into(), 101);
        assert!(notice(&state, &message, "self", true).is_none());
        state.dm_read_at.clear();
        state.muted_channels.insert(conversation_id("peer"));
        assert!(notice(&state, &message, "self", true).is_none());
        state.muted_channels.clear();
        message.author = "self".into();
        assert!(notice(&state, &message, "self", true).is_none());
    }

    #[test]
    fn notification_payload_never_contains_the_dm_body() {
        let mut state = AppState::empty();
        state.dm_accepted.insert("peer".into());
        let payload =
            serde_json::to_string(&notice(&state, &message(), "self", true).unwrap()).unwrap();
        assert!(!payload.contains("private content"));
    }

    #[test]
    fn background_chat_does_not_mark_incoming_messages_as_read() {
        let mut state = AppState::empty();
        let channel = conversation_id("peer");
        state.dm_accepted.insert("peer".into());
        state.dm_mode = true;
        state.dm_pane_open = true;
        state.selected_channel = Some(channel);
        state.app_foreground = false;
        state.note_dm_arrival(channel, "peer", 100);
        assert_eq!(state.dm_unread.get(&channel), Some(&1));
        assert!(!state.dm_read_at.contains_key("peer"));
        state.app_foreground = true;
        state.note_dm_arrival(channel, "peer", 101);
        assert_eq!(state.dm_read_at.get("peer"), Some(&101));
    }

    #[test]
    fn ignored_or_blocked_peers_do_not_notify() {
        let mut state = AppState::empty();
        state.dm_ignored.insert("peer".into());
        assert!(notice(&state, &message(), "self", true).is_none());
        state.dm_ignored.clear();
        state.dm_accepted.insert("peer".into());
        state.mutes = crate::nostr::nip51::MuteList::from_pubkeys(["peer".into()]);
        assert!(notice(&state, &message(), "self", true).is_none());
    }

    #[test]
    fn a_fresh_message_in_the_same_second_as_the_read_mark_can_notify() {
        let mut state = AppState::empty();
        state.dm_accepted.insert("peer".into());
        state.dm_read_at.insert("peer".into(), 100);
        assert!(notice(&state, &message(), "self", true).is_some());
    }

    #[test]
    fn opening_a_notification_preserves_voice_and_does_not_accept_strangers() {
        let mut state = AppState::empty();
        let voice = crate::protocol::Id::new_v4();
        state.voice.channel_id = Some(voice);
        assert!(!open_inbox(&mut state, "stranger"));
        assert!(state.selected_channel.is_none());
        state.dm_accepted.insert("peer".into());
        assert!(open_inbox(&mut state, "peer"));
        assert_eq!(state.voice.channel_id, Some(voice));
        assert_eq!(state.dm_notification_open, Some(conversation_id("peer")));
        assert!(state.dm_mode);
        assert!(!state.dm_accepted.contains("stranger"));
    }
}
