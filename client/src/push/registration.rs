//! The phone's side of the push relay's signed API, and `push.json`, which
//! remembers what was registered so a wake can map a handle to its server.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use base64::Engine as _;
use reqwest::Method;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::identity::Identity;
use crate::protocol::push::{
    CreateHandle, DEFAULT_PUSH_RELAY, DeviceRegistered, HandleCreated, PUBKEY_HEADER,
    RegisterDevice, SIGNATURE_HEADER, TIME_HEADER, UnregisterDevice, push_audience,
    request_payload,
};

pub const RELAY_URL_ENV: &str = "DISCORDIA_PUSH_RELAY_URL";
pub const STATE_FILE: &str = "push.json";
const TIMEOUT: Duration = Duration::from_secs(15);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Build-time default, overridable at run time like `GIPHY_API_KEY`.
pub fn relay_url() -> String {
    std::env::var(RELAY_URL_ENV)
        .ok()
        .or_else(|| option_env!("DISCORDIA_PUSH_RELAY_URL").map(str::to_owned))
        .map(|url| url.trim().trim_end_matches('/').to_owned())
        .filter(|url| !url.is_empty())
        .unwrap_or_else(|| DEFAULT_PUSH_RELAY.to_owned())
}

/// One relay under two spellings: scheme and host in any case, default port
/// or not, with or without a trailing `/`.
pub fn same_relay(a: &str, b: &str) -> bool {
    fn parts(raw: &str) -> Option<(String, String, u16, String)> {
        let url = url::Url::parse(raw.trim()).ok()?;
        Some((
            url.scheme().to_owned(),
            url.host_str()?.to_ascii_lowercase(),
            url.port_or_known_default()?,
            url.path().trim_end_matches('/').to_owned(),
        ))
    }
    parts(a).is_some_and(|a| parts(b) == Some(a))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayError {
    /// The relay URL cannot be used, or a request could not be signed.
    Config(String),
    /// Nothing came back: offline, timed out, refused.
    Network(String),
    /// The relay answered with this status and message.
    Status(u16, String),
    /// `ensure_handle` before any device registration under this key and relay.
    NotRegistered,
    State(String),
}

impl std::fmt::Display for RelayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RelayError::Config(why) => write!(f, "push relay: {why}"),
            RelayError::Network(why) => write!(f, "push relay unreachable: {why}"),
            RelayError::Status(status, why) => write!(f, "push relay answered {status}: {why}"),
            RelayError::NotRegistered => f.write_str("this device is not registered for push"),
            RelayError::State(why) => write!(f, "push state: {why}"),
        }
    }
}

impl std::error::Error for RelayError {}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

fn is_loopback(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "[::1]" | "::1")
}

/// One relay, dialled by the name its signatures are bound to.
#[derive(Clone)]
pub struct RelayClient {
    base: String,
    audience: String,
    http: reqwest::Client,
}

impl RelayClient {
    /// Plain HTTP only on loopback: a token and a signed body cross this link.
    pub fn new(base_url: &str) -> Result<Self, RelayError> {
        let base = base_url.trim().trim_end_matches('/').to_owned();
        let url = url::Url::parse(&base).map_err(|e| RelayError::Config(e.to_string()))?;
        let host = url.host_str().unwrap_or_default();
        if !(url.scheme() == "https" || (url.scheme() == "http" && is_loopback(host))) {
            return Err(RelayError::Config(
                "the relay must be https:// (http:// only on loopback)".into(),
            ));
        }
        let audience = push_audience(&base)
            .ok_or_else(|| RelayError::Config("the relay URL has no host".into()))?;
        let http = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .map_err(|e| RelayError::Config(e.to_string()))?;
        Ok(Self {
            base,
            audience,
            http,
        })
    }

    pub fn from_env() -> Result<Self, RelayError> {
        Self::new(&relay_url())
    }

    pub fn url(&self) -> &str {
        &self.base
    }

    async fn signed(
        &self,
        identity: &Identity,
        method: Method,
        path: &str,
        body: Vec<u8>,
    ) -> Result<reqwest::Response, RelayError> {
        let url = url::Url::parse(&format!("{}{path}", self.base))
            .map_err(|e| RelayError::Config(e.to_string()))?;
        let time = now_unix();
        let body_hash = hex::encode(Sha256::digest(&body));
        let payload = request_payload(
            method.as_str(),
            url.path(),
            &self.audience,
            time,
            &identity.pubkey,
            &body_hash,
        )
        .ok_or_else(|| RelayError::Config("this request cannot be signed".into()))?;
        let mut request = self
            .http
            .request(method, url)
            .header(PUBKEY_HEADER, &identity.pubkey)
            .header(TIME_HEADER, time.to_string())
            .header(SIGNATURE_HEADER, identity.sign_hex(&payload));
        if !body.is_empty() {
            request = request.header(reqwest::header::CONTENT_TYPE, "application/json");
        }
        // `without_url`: a handle in the path is a capability and must not reach a log.
        let response = request
            .body(body)
            .send()
            .await
            .map_err(|e| RelayError::Network(e.without_url().to_string()))?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let why = response.text().await.unwrap_or_default();
        Err(RelayError::Status(
            status.as_u16(),
            crate::push::clean_name(&why),
        ))
    }

    async fn json<T: serde::de::DeserializeOwned>(
        response: reqwest::Response,
    ) -> Result<T, RelayError> {
        response
            .json()
            .await
            .map_err(|e| RelayError::Network(e.without_url().to_string()))
    }

    fn body<T: Serialize>(value: &T) -> Result<Vec<u8>, RelayError> {
        serde_json::to_vec(value).map_err(|e| RelayError::Config(e.to_string()))
    }

    pub async fn put_device(
        &self,
        identity: &Identity,
        device: &RegisterDevice,
    ) -> Result<DeviceRegistered, RelayError> {
        let response = self
            .signed(identity, Method::PUT, "/v1/devices", Self::body(device)?)
            .await?;
        Self::json(response).await
    }

    pub async fn delete_device(&self, identity: &Identity, token: &str) -> Result<(), RelayError> {
        let body = Self::body(&UnregisterDevice {
            token: token.to_owned(),
        })?;
        self.signed(identity, Method::DELETE, "/v1/devices", body)
            .await
            .map(drop)
    }

    pub async fn create_handle(
        &self,
        identity: &Identity,
        token: &str,
    ) -> Result<String, RelayError> {
        let body = Self::body(&CreateHandle {
            token: token.to_owned(),
        })?;
        let response = self
            .signed(identity, Method::POST, "/v1/handles", body)
            .await?;
        Ok(Self::json::<HandleCreated>(response).await?.handle)
    }

    /// `false`: the relay knows no phone of this key that takes DMs, or predates the route.
    pub async fn hold_attention(&self, identity: &Identity) -> Result<bool, RelayError> {
        match self
            .signed(identity, Method::PUT, "/v1/attention", Vec::new())
            .await
        {
            Ok(_) => Ok(true),
            Err(RelayError::Status(404, _)) => Ok(false),
            Err(error) => Err(error),
        }
    }

    pub async fn release_attention(&self, identity: &Identity) -> Result<(), RelayError> {
        self.signed(identity, Method::DELETE, "/v1/attention", Vec::new())
            .await
            .map(drop)
    }

    pub async fn delete_handle(&self, identity: &Identity, handle: &str) -> Result<(), RelayError> {
        if handle.is_empty()
            || !handle
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'='))
        {
            return Err(RelayError::Config("not a relay handle".into()));
        }
        self.signed(
            identity,
            Method::DELETE,
            &format!("/v1/handles/{handle}"),
            Vec::new(),
        )
        .await
        .map(drop)
    }
}

/// A handle and the key a server encrypts its pushes to this phone with.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerHandle {
    pub handle: String,
    /// 32 bytes, standard base64.
    pub key: String,
}

impl ServerHandle {
    pub fn key_bytes(&self) -> Option<[u8; 32]> {
        base64::engine::general_purpose::STANDARD
            .decode(self.key.trim())
            .ok()?
            .try_into()
            .ok()
    }
}

impl std::fmt::Debug for ServerHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ServerHandle(<redacted>)")
    }
}

/// `<config_dir>/push.json`. Absent or unreadable reads as empty: the worst
/// case is a fresh handle per server, never a failed launch.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushState {
    /// The key the device was last registered under; handles belong to it.
    #[serde(default)]
    pub account: Option<String>,
    /// The relay `account` registered at; a handle means nothing at another.
    #[serde(default)]
    pub relay: Option<String>,
    /// The FCM token last registered.
    #[serde(default)]
    pub token: Option<String>,
    /// Server dial origin (the `AppState::server_origin` spelling) → handle.
    #[serde(default)]
    pub servers: BTreeMap<String, ServerHandle>,
    /// The person's category toggles; install-wide, so they outlive sign-out.
    #[serde(default = "on")]
    pub dms: bool,
    #[serde(default = "on")]
    pub mentions: bool,
}

fn on() -> bool {
    true
}

impl Default for PushState {
    fn default() -> Self {
        Self {
            account: None,
            relay: None,
            token: None,
            servers: BTreeMap::new(),
            dms: true,
            mentions: true,
        }
    }
}

impl std::fmt::Debug for PushState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PushState")
            .field("account", &self.account)
            .field("relay", &self.relay)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .field("servers", &self.servers.keys().collect::<Vec<_>>())
            .field("dms", &self.dms)
            .field("mentions", &self.mentions)
            .finish()
    }
}

impl PushState {
    pub fn load(dir: &Path) -> Self {
        std::fs::read_to_string(dir.join(STATE_FILE))
            .ok()
            .and_then(|content| serde_json::from_str(&content).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> Result<(), String> {
        let content = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        crate::identity::write_private(&dir.join(STATE_FILE), &content)
    }

    /// The origin a handle was minted for, and its key.
    pub fn server_of(&self, handle: &str) -> Option<(&str, [u8; 32])> {
        self.servers
            .iter()
            .find(|(_, entry)| entry.handle == handle)
            .and_then(|(origin, entry)| Some((origin.as_str(), entry.key_bytes()?)))
    }

    fn registered_here(&self, identity: &Identity, relay: &RelayClient) -> bool {
        self.account.as_deref() == Some(identity.pubkey.as_str())
            && self.relay.as_deref() == Some(relay.url())
    }

    /// Whether anything changed.
    pub fn set(&mut self, category: Category, on: bool) -> bool {
        let slot = match category {
            Category::Dms => &mut self.dms,
            Category::Mentions => &mut self.mentions,
        };
        std::mem::replace(slot, on) != on
    }

    pub fn wants_any(&self) -> bool {
        self.dms || self.mentions
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Dms,
    Mentions,
}

// One writer at a time: two servers' Ready can mint handles at once.
static STATE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Registers (or refreshes) this device. A new token for the same key and relay
/// is a rotation: the old one rides as `replaces` so its handles move over.
pub async fn register_device(
    dir: &Path,
    relay: &RelayClient,
    identity: &Identity,
    token: &str,
    package: &str,
    dm: bool,
    relays: Vec<String>,
) -> Result<DeviceRegistered, RelayError> {
    let _guard = STATE_LOCK.lock().await;
    let mut state = PushState::load(dir);
    let same_home = state.registered_here(identity, relay);
    let replaces = state.token.clone().filter(|old| same_home && old != token);
    let reply = relay
        .put_device(
            identity,
            &RegisterDevice {
                token: token.to_owned(),
                package: package.to_owned(),
                dm,
                relays,
                replaces,
            },
        )
        .await?;
    if !same_home {
        state.servers.clear();
    }
    state.account = Some(identity.pubkey.clone());
    state.relay = Some(relay.url().to_owned());
    state.token = Some(token.to_owned());
    state.save(dir).map_err(RelayError::State)?;
    Ok(reply)
}

/// The handle and key for `origin`: the stored pair, else a new handle from the
/// relay and a fresh random key, stored before they are returned.
pub async fn ensure_handle(
    dir: &Path,
    relay: &RelayClient,
    identity: &Identity,
    origin: &str,
) -> Result<(String, [u8; 32]), RelayError> {
    let _guard = STATE_LOCK.lock().await;
    let mut state = PushState::load(dir);
    if !state.registered_here(identity, relay) {
        return Err(RelayError::NotRegistered);
    }
    let token = state.token.clone().ok_or(RelayError::NotRegistered)?;
    if let Some(entry) = state.servers.get(origin)
        && let Some(key) = entry.key_bytes()
    {
        return Ok((entry.handle.clone(), key));
    }
    let handle = relay.create_handle(identity, &token).await?;
    let mut key = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut key);
    state.servers.insert(
        origin.to_owned(),
        ServerHandle {
            handle: handle.clone(),
            key: base64::engine::general_purpose::STANDARD.encode(key),
        },
    );
    state.save(dir).map_err(RelayError::State)?;
    Ok((handle, key))
}

/// Leaving a server. The entry stays when the relay could not be reached, so
/// the next attempt can still revoke the handle.
pub async fn forget_origin(
    dir: &Path,
    relay: &RelayClient,
    identity: &Identity,
    origin: &str,
) -> Result<(), RelayError> {
    let _guard = STATE_LOCK.lock().await;
    let mut state = PushState::load(dir);
    let Some(entry) = state.servers.get(origin).cloned() else {
        return Ok(());
    };
    if state.registered_here(identity, relay) {
        match relay.delete_handle(identity, &entry.handle).await {
            Ok(()) | Err(RelayError::Status(404, _)) => {}
            Err(e) => return Err(e),
        }
    }
    state.servers.remove(origin);
    state.save(dir).map_err(RelayError::State)
}

/// Flips one toggle in `push.json`; `Ok(true)` when it changed. What follows
/// from it is `push::device`'s, which watches `device::changes()`.
pub async fn set_category(dir: &Path, category: Category, on: bool) -> Result<bool, RelayError> {
    let _guard = STATE_LOCK.lock().await;
    let mut state = PushState::load(dir);
    if !state.set(category, on) {
        return Ok(false);
    }
    state.save(dir).map_err(RelayError::State)?;
    Ok(true)
}

/// Sign-out, account switch, notifications off: call with the outgoing key. Its
/// handles are dropped locally whatever the relay says; that key is going away.
pub async fn unregister_device(
    dir: &Path,
    relay: &RelayClient,
    identity: &Identity,
) -> Result<(), RelayError> {
    let _guard = STATE_LOCK.lock().await;
    let mut state = PushState::load(dir);
    let result = match state.token.clone() {
        Some(token) if state.registered_here(identity, relay) => {
            match relay.delete_device(identity, &token).await {
                Err(RelayError::Status(404, _)) => Ok(()),
                other => other,
            }
        }
        _ => Ok(()),
    };
    if state.account.as_deref() == Some(identity.pubkey.as_str()) {
        state.account = None;
        state.relay = None;
        state.servers.clear();
        state.save(dir).map_err(RelayError::State)?;
    }
    result
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::net::SocketAddr;
    use std::sync::Arc;

    use discordia_push_relay::limits::Limits;
    use discordia_push_relay::sender::RecordingSender;
    use discordia_push_relay::store::{Caps, Store};
    use discordia_push_relay::{Config, Relay, router};

    pub(crate) fn sandbox() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("discordia-push-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    pub(crate) fn identity(seed: u8) -> Identity {
        Identity::restore_from_private_key(hex::encode([seed; 32]), "phone").unwrap()
    }

    fn sample() -> PushState {
        PushState {
            account: Some("a".repeat(64)),
            relay: Some("https://push.example".into()),
            token: Some("tok".into()),
            servers: [(
                "quic://host@1.2.3.4:5".to_string(),
                ServerHandle {
                    handle: "hh".into(),
                    key: base64::engine::general_purpose::STANDARD.encode([9u8; 32]),
                },
            )]
            .into(),
            ..PushState::default()
        }
    }

    #[test]
    fn the_state_file_round_trips_privately() {
        let dir = sandbox();
        sample().save(&dir).unwrap();
        assert_eq!(PushState::load(&dir), sample());
        assert_eq!(
            PushState::load(&dir).server_of("hh"),
            Some(("quic://host@1.2.3.4:5", [9u8; 32]))
        );
        assert_eq!(PushState::load(&dir).server_of("other"), None);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(STATE_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn an_absent_or_corrupt_state_file_reads_as_empty() {
        let dir = sandbox();
        assert_eq!(PushState::load(&dir), PushState::default());
        for junk in ["", "{", "null", "[]", "{\"servers\":7}", "\u{0}\u{1}"] {
            std::fs::write(dir.join(STATE_FILE), junk).unwrap();
            assert_eq!(PushState::load(&dir), PushState::default(), "{junk:?}");
        }
        let mut bad_key = sample();
        bad_key
            .servers
            .values_mut()
            .for_each(|s| s.key = "short".into());
        bad_key.save(&dir).unwrap();
        assert_eq!(PushState::load(&dir).server_of("hh"), None);
    }

    #[test]
    fn a_relay_matches_itself_under_any_spelling() {
        let ours = "https://push.discordia.world";
        for same in [
            "https://push.discordia.world/",
            "HTTPS://Push.Discordia.World",
            "https://push.discordia.world:443",
            " https://push.discordia.world ",
        ] {
            assert!(same_relay(same, ours), "{same}");
        }
        for other in [
            "http://push.discordia.world",
            "https://push.discordia.world:8443",
            "https://push.example",
            "https://push.discordia.world/v2",
            "",
            "off",
        ] {
            assert!(!same_relay(other, ours), "{other}");
        }
        assert!(!same_relay("off", "off"), "only a URL is a relay");
    }

    #[test]
    fn toggles_default_on_and_report_a_change() {
        let old: PushState = serde_json::from_str(r#"{"token":"t"}"#).unwrap();
        assert!(old.dms && old.mentions, "a file from before the toggles");
        let mut state = PushState::default();
        assert!(!state.set(Category::Dms, true));
        assert!(state.set(Category::Dms, false));
        assert!(state.wants_any());
        assert!(state.set(Category::Mentions, false));
        assert!(!state.wants_any());
    }

    #[tokio::test]
    async fn a_toggle_survives_the_writers_beside_it() {
        let (client, _relay) = spawn_relay().await;
        let dir = sandbox();
        let me = identity(7);
        assert_eq!(set_category(&dir, Category::Dms, true).await, Ok(false));
        assert_eq!(set_category(&dir, Category::Dms, false).await, Ok(true));
        register_device(&dir, &client, &me, "tok", "pkg", false, vec![])
            .await
            .unwrap();
        ensure_handle(&dir, &client, &me, "origin").await.unwrap();
        unregister_device(&dir, &client, &me).await.unwrap();
        assert!(!PushState::load(&dir).dms);
    }

    #[test]
    fn debug_output_names_no_token_handle_or_key() {
        let shown = format!("{:?}", sample());
        assert!(!shown.contains("tok\"") && !shown.contains("hh") && !shown.contains("CQkJ"));
    }

    #[test]
    fn only_https_or_loopback_http_is_a_relay() {
        assert!(RelayClient::new("https://push.example/").is_ok());
        assert_eq!(
            RelayClient::new("https://push.example/").unwrap().url(),
            "https://push.example"
        );
        assert!(RelayClient::new("http://127.0.0.1:7720").is_ok());
        assert!(RelayClient::new("http://push.example").is_err());
        assert!(RelayClient::new("not a url").is_err());
        assert!(RelayClient::new("wss://push.example").is_err());
    }

    pub(crate) async fn spawn_relay() -> (RelayClient, Arc<Relay>) {
        let relay = Arc::new(Relay::new(
            Store::in_memory(Caps::default()).unwrap(),
            Arc::new(RecordingSender::default()),
            Config::default(),
            Limits::default(),
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router(relay.clone());
        tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        (RelayClient::new(&format!("http://{addr}")).unwrap(), relay)
    }

    // The relay's own router verifies every request below, so this is the signature interop test.
    #[tokio::test]
    async fn the_real_relay_accepts_what_the_phone_signs() {
        let (client, relay) = spawn_relay().await;
        let dir = sandbox();
        let me = identity(3);

        assert_eq!(
            ensure_handle(&dir, &client, &me, "origin-a").await,
            Err(RelayError::NotRegistered)
        );
        let registered = register_device(
            &dir,
            &client,
            &me,
            "token-1",
            "com.discordia.app",
            true,
            vec!["wss://relay.example".into()],
        )
        .await
        .unwrap();
        assert!(registered.expires_at > 0);
        assert!(
            relay
                .store
                .device_exists(&me.pubkey, "token-1", now_unix())
                .unwrap()
        );

        let (handle, key) = ensure_handle(&dir, &client, &me, "origin-a").await.unwrap();
        assert_eq!(
            ensure_handle(&dir, &client, &me, "origin-a").await.unwrap(),
            (handle.clone(), key),
            "a second call reuses the stored handle and key"
        );
        let (other, other_key) = ensure_handle(&dir, &client, &me, "origin-b").await.unwrap();
        assert_ne!(other, handle);
        assert_ne!(other_key, key);
        assert_eq!(
            PushState::load(&dir).server_of(&handle),
            Some(("origin-a", key))
        );

        register_device(
            &dir,
            &client,
            &me,
            "token-2",
            "com.discordia.app",
            true,
            vec![],
        )
        .await
        .unwrap();
        assert!(
            relay
                .store
                .device_exists(&me.pubkey, "token-2", now_unix())
                .unwrap()
        );
        assert_eq!(
            PushState::load(&dir).token.as_deref(),
            Some("token-2"),
            "rotation keeps the handles"
        );
        assert!(PushState::load(&dir).server_of(&handle).is_some());

        forget_origin(&dir, &client, &me, "origin-a").await.unwrap();
        assert!(PushState::load(&dir).server_of(&handle).is_none());
        assert_eq!(
            client.delete_handle(&me, &handle).await,
            Err(RelayError::Status(404, "no such handle".into())),
            "the relay already dropped it"
        );

        unregister_device(&dir, &client, &me).await.unwrap();
        assert!(
            !relay
                .store
                .device_exists(&me.pubkey, "token-2", now_unix())
                .unwrap()
        );
        let after = PushState::load(&dir);
        assert!(after.account.is_none() && after.servers.is_empty());
    }

    #[tokio::test]
    async fn a_desktop_quiets_only_a_key_that_has_a_phone() {
        let (client, relay) = spawn_relay().await;
        let dir = sandbox();
        let me = identity(5);
        assert_eq!(client.hold_attention(&me).await, Ok(false));
        register_device(
            &dir,
            &client,
            &me,
            "token-1",
            "com.discordia.app",
            true,
            vec![],
        )
        .await
        .unwrap();
        assert_eq!(client.hold_attention(&me).await, Ok(true));
        assert!(relay.attended(&me.pubkey, now_unix()));
        client.release_attention(&me).await.unwrap();
        assert!(!relay.attended(&me.pubkey, now_unix()));
    }

    #[tokio::test]
    async fn a_lease_is_held_while_in_front_and_given_up_with_the_account() {
        use crate::push::attention::{HOLD_AFTER, Lease, run_lease};
        let (client, relay) = spawn_relay().await;
        let dir = sandbox();
        let me = identity(6);
        register_device(
            &dir,
            &client,
            &me,
            "token-1",
            "com.discordia.app",
            true,
            vec![],
        )
        .await
        .unwrap();
        let (focus, focused) = tokio::sync::watch::channel(true);
        let settled = Lease::new(true, tokio::time::Instant::now() - HOLD_AFTER);
        let keeper = tokio::spawn(run_lease(settled, client, me.clone(), focused));
        let attended = |wanted: bool| {
            let (relay, key) = (relay.clone(), me.pubkey.clone());
            async move {
                for _ in 0..200 {
                    if relay.attended(&key, now_unix()) == wanted {
                        return true;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                false
            }
        };
        assert!(attended(true).await, "in front: held");
        drop(focus);
        assert!(attended(false).await, "signed out: released");
        keeper.await.unwrap();
    }

    #[tokio::test]
    async fn a_request_signed_by_another_key_or_for_another_relay_is_refused() {
        let (client, _relay) = spawn_relay().await;
        let me = identity(4);
        let device = RegisterDevice {
            token: "token-x".into(),
            package: "com.discordia.app".into(),
            dm: false,
            relays: vec![],
            replaces: None,
        };
        assert!(client.put_device(&me, &device).await.is_ok());

        let mut elsewhere = client.clone();
        elsewhere.audience = "push.example".into();
        assert!(matches!(
            elsewhere.put_device(&me, &device).await,
            Err(RelayError::Status(401, _))
        ));
    }

    #[tokio::test]
    async fn a_new_account_starts_with_no_handles() {
        let (client, _relay) = spawn_relay().await;
        let dir = sandbox();
        let (first, second) = (identity(5), identity(6));
        register_device(
            &dir,
            &client,
            &first,
            "tok",
            "com.discordia.app",
            false,
            vec![],
        )
        .await
        .unwrap();
        ensure_handle(&dir, &client, &first, "origin")
            .await
            .unwrap();
        register_device(
            &dir,
            &client,
            &second,
            "tok",
            "com.discordia.app",
            false,
            vec![],
        )
        .await
        .unwrap();
        let state = PushState::load(&dir);
        assert!(state.servers.is_empty());
        assert_eq!(state.account.as_deref(), Some(second.pubkey.as_str()));
        assert_eq!(
            ensure_handle(&dir, &client, &first, "origin").await,
            Err(RelayError::NotRegistered)
        );
    }
}
