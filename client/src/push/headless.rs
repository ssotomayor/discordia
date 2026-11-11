//! One push in, at most one alert out, from the files on disk alone. Nothing
//! here may reach `config_dir`, the keystore, `ndk_context` or Dioxus.

use std::path::Path;
use std::time::Duration;

use serde::Serialize;
use zeroize::Zeroizing;

use crate::nostr::event::{Event, Rumor};
use crate::nostr::{call_history, dm_image, nip17, nip44, nip59};
use crate::protocol::push::{DmWrap, MentionPayload, PushData, PushReason};
use crate::protocol::{Id, is_canonical_pubkey};
use crate::push::peers::{self, PushPeers};
use crate::push::registration::PushState;
use crate::push::{clean_name, is_event_id};
use crate::settings::ClientSettings;

/// A wrap is backdated up to two days (NIP-59), so older is a replay, not news.
pub const MAX_AGE_SECS: i64 = 2 * 24 * 60 * 60;
const MAX_DATA_BYTES: usize = 64 * 1024;
const MAX_EVENT_BYTES: usize = 256 * 1024;
/// The app writes 16; a larger cost would hold a phone for minutes per push.
const MAX_KEY_COST: u8 = 20;
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
const MENTION_VERSION: u32 = 1;

/// What the Android side posts. A DM's text only while `dm_notification_text` is on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Alert {
    /// `dm_notifications::Notice` plus the message id; `at` is the sender's
    /// clock, as the live path sends it, so the two dedup against each other.
    Dm {
        account: String,
        peer: String,
        sender: String,
        message: String,
        public: String,
        silent: bool,
        at: i64,
        id: String,
    },
    Mention {
        account: String,
        origin: String,
        guild: Id,
        channel: Id,
        message_id: Id,
        author: String,
        author_name: String,
        guild_name: String,
        channel_name: String,
        /// "<author> mentioned you" in the person's language.
        text: String,
        silent: bool,
        at: i64,
    },
    /// The relay's diagnostic push: show a test notification.
    Ping,
}

/// `passphrase` runs only after every check that needs no key has passed, and
/// `fetch(event_id, relay_url)` only for a wrap too large to ride inline.
pub fn handle_push(
    dir: &Path,
    data: &str,
    system_language: &str,
    now: i64,
    passphrase: impl FnOnce() -> Result<String, String>,
    fetch: impl FnOnce(&str, &str) -> Result<String, String>,
) -> Option<Alert> {
    if data.len() > MAX_DATA_BYTES {
        return None;
    }
    match serde_json::from_str::<PushData>(data).ok()? {
        PushData::Ping => Some(Alert::Ping),
        PushData::Dm { p, wrap } => dm(dir, &p, wrap, system_language, now, passphrase, fetch),
        PushData::Mention { h, c } => mention(dir, &h, &c, system_language, now),
        PushData::Unknown => None,
    }
}

fn settings_in(dir: &Path) -> ClientSettings {
    std::fs::read_to_string(dir.join("settings.json"))
        .ok()
        .and_then(|content| crate::settings::parse(&content))
        .unwrap_or_default()
}

/// Never `Language::System` out: resolving that reads the Activity's locale.
fn language_of(settings: &ClientSettings, system_language: &str) -> crate::i18n::Language {
    match settings.language {
        crate::i18n::Language::System => crate::i18n::from_tag(system_language),
        chosen => chosen,
    }
}

fn is_dnd(dir: &Path, account: &str) -> bool {
    crate::profile::load_in(dir, account)
        .and_then(|profile| profile.status)
        .is_some_and(|status| status.trim() == "dnd")
}

fn dm(
    dir: &Path,
    recipient: &str,
    wrap: DmWrap,
    system_language: &str,
    now: i64,
    passphrase: impl FnOnce() -> Result<String, String>,
    fetch: impl FnOnce(&str, &str) -> Result<String, String>,
) -> Option<Alert> {
    let account = crate::identity::active_in(dir)?;
    if recipient != account || !PushState::load(dir).dms || is_dnd(dir, &account) {
        return None;
    }
    let gift = gift_wrap(wrap, fetch)?;
    // Only call signals carry `expiration`; dropping them here spares the scrypt.
    if gift.tag("p") != Some(account.as_str()) || gift.tag("expiration").is_some() {
        return None;
    }
    let locked = crate::identity::ncryptsec_in(dir, &account)?;
    if crate::keyvault::cost(&locked).is_none_or(|cost| cost > MAX_KEY_COST) {
        return None;
    }
    let passphrase = Zeroizing::new(passphrase().ok()?);
    let secret = unlock(&locked, &passphrase, &account)?;
    let rumor = nip59::unwrap(&secret, &gift).ok()?;
    if !is_new_chat(&account, &rumor) {
        return None;
    }
    let message = nip17::open_rumor(&account, &rumor).ok()?;
    let settings = settings_in(dir);
    let peers = peers::read(dir, &account);
    decide_dm(&settings, &peers, &account, &message, system_language, now)
}

/// Parsed, matched to the id asked for, and signed by its own key.
fn gift_wrap(
    wrap: DmWrap,
    fetch: impl FnOnce(&str, &str) -> Result<String, String>,
) -> Option<Event> {
    let (json, wanted) = match wrap {
        DmWrap::Inline { e } => (e, None),
        DmWrap::ById { id, r } => {
            if !is_event_id(&id) {
                return None;
            }
            (fetch(&id, &r).ok()?, Some(id))
        }
    };
    if json.len() > MAX_EVENT_BYTES {
        return None;
    }
    let event: Event = serde_json::from_str(&json).ok()?;
    if wanted.is_some_and(|id| id != event.id)
        || event.kind != nip59::KIND_GIFT_WRAP
        || !event.verify()
    {
        return None;
    }
    Some(event)
}

fn unlock(locked: &str, passphrase: &str, account: &str) -> Option<secp256k1::SecretKey> {
    let bytes = crate::keyvault::decrypt(locked, passphrase).ok()?;
    let secret = secp256k1::SecretKey::from_slice(bytes.as_ref()).ok()?;
    // As `Identity::load`: the file's name only locates a key; it must be the one named.
    (crate::nostr::event::xonly_hex(&secret) == account).then_some(secret)
}

/// A first message: not a call signal, call record, edit or deletion.
fn is_new_chat(account: &str, rumor: &Rumor) -> bool {
    (rumor.kind == nip17::KIND_CHAT || rumor.kind == dm_image::KIND)
        && nip17::edit_target(rumor).is_none()
        && matches!(call_history::open(account, rumor), Ok(None))
}

/// The phone's rows of the plan's table, mirroring `dm_notifications::notice`
/// over what the app saved instead of what it holds in memory.
fn decide_dm(
    settings: &ClientSettings,
    peers: &PushPeers,
    account: &str,
    message: &nip17::ChatMessage,
    system_language: &str,
    now: i64,
) -> Option<Alert> {
    let peer = message.peer.as_str();
    if message.author == account || message.author != peer || !is_canonical_pubkey(peer) {
        return None;
    }
    // The app's own rule, over what a cold process has: `push-peers.json` names
    // the contacts and guild-mates the app last saw.
    let standing = crate::dm_standing::standing(
        settings.dm_blocked.iter().any(|key| key == peer),
        crate::dm_standing::choice_in(&settings.dm_accepted, &settings.dm_ignored, peer),
        peers.name(peer).is_some(),
    );
    if standing != crate::dm_standing::DmStanding::Inbox
        || settings
            .muted_channels
            .contains(&crate::nostr::service::conversation_id(peer))
        || settings.dm_deleted_ids.contains(&message.id)
    {
        return None;
    }
    let offset = settings
        .dm_clock_offset
        .iter()
        .find(|(author, _)| author == peer)
        .map_or(0, |(_, offset)| *offset);
    let at = message.created_at.saturating_sub(offset);
    if settings
        .dm_read_at
        .iter()
        .any(|(key, read)| key == peer && at < *read)
        || now.saturating_sub(at) > MAX_AGE_SECS
    {
        return None;
    }
    let language = language_of(settings, system_language);
    Some(Alert::Dm {
        account: account.to_owned(),
        peer: peer.to_owned(),
        sender: peers
            .name(peer)
            .map(clean_name)
            .unwrap_or_else(|| crate::identity::truncate_pubkey(peer)),
        message: crate::push::dm_alert_text(
            &message.content,
            settings.dm_notification_text,
            |key| crate::i18n::translate(language, key, &[]),
        ),
        public: crate::i18n::translate(language, "New private message", &[]),
        silent: settings.sfx_volume == 0,
        at: message.created_at,
        id: message.id.clone(),
    })
}

fn mention(
    dir: &Path,
    handle: &str,
    ciphertext: &str,
    system_language: &str,
    now: i64,
) -> Option<Alert> {
    let account = crate::identity::active_in(dir)?;
    let state = PushState::load(dir);
    if state.account.as_deref() != Some(account.as_str()) || !state.mentions {
        return None;
    }
    let (origin, key) = state.server_of(handle)?;
    if is_dnd(dir, &account) {
        return None;
    }
    let payload: MentionPayload =
        serde_json::from_str(&nip44::decrypt(&key, ciphertext).ok()?).ok()?;
    decide_mention(
        &settings_in(dir),
        &account,
        origin,
        &payload,
        system_language,
        now,
    )
}

/// `channel_read_at` is milliseconds per server origin; the payload's `at` is seconds.
fn decide_mention(
    settings: &ClientSettings,
    account: &str,
    origin: &str,
    payload: &MentionPayload,
    system_language: &str,
    now: i64,
) -> Option<Alert> {
    if payload.v != MENTION_VERSION
        || payload.k != PushReason::Mention
        || payload.author == account
        || !is_canonical_pubkey(&payload.author)
        || settings.muted_channels.contains(&payload.channel)
        || settings.muted_guilds.contains(&payload.guild)
        || now.saturating_sub(payload.at) > MAX_AGE_SECS
    {
        return None;
    }
    let read = settings
        .channel_read_at
        .get(origin)
        .and_then(|marks| marks.get(&payload.channel));
    if read.is_some_and(|mark| payload.at.saturating_mul(1000) <= *mark) {
        return None;
    }
    let author_name = clean_name(&payload.author_name);
    let author_name = if author_name.trim().is_empty() {
        crate::identity::truncate_pubkey(&payload.author)
    } else {
        author_name
    };
    // An isolate ends any direction override the name opens, so it cannot reverse the line.
    let text = crate::i18n::translate(
        language_of(settings, system_language),
        "{0} mentioned you",
        &[format!("\u{2068}{author_name}\u{2069}")],
    );
    Some(Alert::Mention {
        account: account.to_owned(),
        origin: origin.to_owned(),
        guild: payload.guild,
        channel: payload.channel,
        message_id: payload.message,
        author: payload.author.clone(),
        author_name,
        guild_name: clean_name(&payload.guild_name),
        channel_name: clean_name(&payload.channel_name),
        text,
        silent: settings.sfx_volume == 0,
        at: payload.at,
    })
}

/// For the JNI wrapper's `fetch`: one relay, one `REQ` by id, the first match
/// or `FETCH_TIMEOUT`, on a runtime of its own. `wss://` only.
pub fn fetch_event_blocking(id: &str, relay: &str) -> Result<String, String> {
    if !is_event_id(id) {
        return Err("not an event id".into());
    }
    let url = url::Url::parse(relay).map_err(|e| e.to_string())?;
    if url.scheme() != "wss" {
        return Err("only wss:// relays are dialled".into());
    }
    block_on(fetch_event(id, relay))
}

fn block_on(
    fetch: impl std::future::Future<Output = Result<String, String>>,
) -> Result<String, String> {
    // `block_on` inside a runtime panics; a caller there must await `fetch_event` itself.
    if tokio::runtime::Handle::try_current().is_ok() {
        return Err("already inside an async runtime".into());
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(async {
        tokio::time::timeout(FETCH_TIMEOUT, fetch)
            .await
            .map_err(|_| "the relay did not answer in time".to_string())?
    })
}

pub async fn fetch_event(id: &str, relay: &str) -> Result<String, String> {
    use futures_util::{SinkExt, StreamExt};
    use serde_json::Value;
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

    const SUBSCRIPTION: &str = "push";
    let config = WebSocketConfig {
        max_message_size: Some(MAX_EVENT_BYTES * 2),
        max_frame_size: Some(MAX_EVENT_BYTES * 2),
        ..Default::default()
    };
    let (mut socket, _) = tokio_tungstenite::connect_async_with_config(relay, Some(config), false)
        .await
        .map_err(|e| e.to_string())?;
    let request = serde_json::json!(["REQ", SUBSCRIPTION, {"ids": [id], "limit": 1}]);
    socket
        .send(Message::Text(request.to_string()))
        .await
        .map_err(|e| e.to_string())?;
    let found = loop {
        let text = match socket.next().await {
            Some(Ok(Message::Text(text))) => text,
            Some(Ok(Message::Close(_))) | None => break Err("the relay closed".to_string()),
            Some(Ok(_)) => continue,
            Some(Err(e)) => break Err(e.to_string()),
        };
        let Ok(Value::Array(parts)) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if parts.get(1).and_then(Value::as_str) != Some(SUBSCRIPTION) {
            continue;
        }
        match parts.first().and_then(Value::as_str) {
            Some("EVENT") => {
                if let Some(event) = parts.get(2)
                    && event.get("id").and_then(Value::as_str) == Some(id)
                {
                    break Ok(event.to_string());
                }
            }
            Some("EOSE") => break Err("the relay does not hold that event".to_string()),
            Some("CLOSED") => break Err("the relay refused the request".to_string()),
            _ => {}
        }
    };
    let close = serde_json::json!(["CLOSE", SUBSCRIPTION]);
    let _ = socket.send(Message::Text(close.to_string())).await;
    let _ = socket.close(None).await;
    found
}

#[cfg(test)]
#[path = "headless_tests.rs"]
mod tests;
