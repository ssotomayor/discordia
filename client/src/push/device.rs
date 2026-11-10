//! What the relay should know about this install while an account is mounted:
//! the device under the platform's token, or nothing. Without a token (every
//! desktop) it never makes a call.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::watch;
use tokio::time::Instant;

use crate::identity::Identity;
use crate::push::registration::{self, Category, PushState, RelayClient, RelayError};

/// The relay expires a device 30 days after its last PUT.
pub const REFRESH: Duration = Duration::from_secs(24 * 60 * 60);
/// Long enough to fold a burst of changes into one call.
const SETTLE: Duration = Duration::from_millis(500);
const FIRST_RETRY: Duration = Duration::from_secs(5);
const LAST_RETRY: Duration = Duration::from_secs(30 * 60);

/// What only the platform knows. Android's FCM service and notification
/// settings feed it through `set_platform`; elsewhere it stays empty.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Platform {
    pub token: Option<String>,
    /// The OS lets the app post and the person's notification switch is on.
    pub enabled: bool,
    /// The application id the token was issued to.
    pub package: String,
}

impl std::fmt::Debug for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Platform")
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .field("enabled", &self.enabled)
            .field("package", &self.package)
            .finish()
    }
}

pub fn platform() -> &'static watch::Sender<Platform> {
    static PLATFORM: OnceLock<watch::Sender<Platform>> = OnceLock::new();
    PLATFORM.get_or_init(|| watch::channel(Platform::default()).0)
}

/// For the platform layer: a new token, a rotated one, or the switch flipped.
pub fn set_platform(next: Platform) {
    platform().send_if_modified(|current| {
        let changed = *current != next;
        *current = next;
        changed
    });
}

/// Bumped whenever `push.json` changes in a way a running session acts on.
pub fn changes() -> &'static watch::Sender<u64> {
    static CHANGES: OnceLock<watch::Sender<u64>> = OnceLock::new();
    CHANGES.get_or_init(|| watch::channel(0).0)
}

pub(crate) fn changed() {
    changes().send_modify(|revision| *revision = revision.wrapping_add(1));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preferences {
    pub dms: bool,
    pub mentions: bool,
}

pub fn preferences() -> Preferences {
    let state = PushState::load(&crate::identity::config_dir());
    Preferences {
        dms: state.dms,
        mentions: state.mentions,
    }
}

/// The DMs toggle. The mounted account re-registers with the new `dm` flag,
/// or unregisters the device when both toggles are off.
pub async fn set_dms(on: bool) -> Result<(), String> {
    set(Category::Dms, on).await
}

/// The Mentions toggle. Off: the live session withdraws its handle and every
/// server's handle is revoked at the relay; on: the next Ready registers again.
pub async fn set_mentions(on: bool) -> Result<(), String> {
    set(Category::Mentions, on).await
}

async fn set(category: Category, on: bool) -> Result<(), String> {
    let dir = crate::identity::config_dir();
    if registration::set_category(&dir, category, on)
        .await
        .map_err(|e| e.to_string())?
    {
        changed();
    }
    Ok(())
}

#[derive(Clone, PartialEq, Eq)]
pub enum Want {
    Register {
        token: String,
        package: String,
        dm: bool,
        relays: Vec<String>,
    },
    Unregister,
    Nothing,
}

impl std::fmt::Debug for Want {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Want::Register { dm, relays, .. } => f
                .debug_struct("Register")
                .field("dm", dm)
                .field("relays", &relays.len())
                .finish(),
            Want::Unregister => f.write_str("Unregister"),
            Want::Nothing => f.write_str("Nothing"),
        }
    }
}

pub fn wanted(platform: &Platform, state: &PushState, account: &str, relays: &[String]) -> Want {
    match platform.token.as_deref().filter(|token| !token.is_empty()) {
        Some(token) if platform.enabled && state.wants_any() => Want::Register {
            token: token.to_owned(),
            package: platform.package.clone(),
            dm: state.dms,
            relays: relays.to_vec(),
        },
        _ if state.account.as_deref() == Some(account) => Want::Unregister,
        _ => Want::Nothing,
    }
}

/// With mentions off, no server should keep a way to push to this install.
pub fn stale_origins(state: &PushState, account: &str) -> Vec<String> {
    if state.account.as_deref() == Some(account) && !state.mentions {
        state.servers.keys().cloned().collect()
    } else {
        Vec::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    /// `Register` or `Unregister`; `None` when the relay already has it.
    pub device: Option<Want>,
    pub forget: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    Run(Action),
    At(Instant),
    Idle,
}

/// When the relay last heard what, and when a failed call may be tried again.
#[derive(Debug, Default)]
pub struct Schedule {
    registered: Option<(Want, Instant)>,
    failed: Option<(Want, Vec<String>, u32, Instant)>,
}

impl Schedule {
    pub fn plan(&self, want: &Want, forget: &[String], now: Instant) -> Plan {
        if let Some((failed, forgetting, _, retry)) = &self.failed
            && failed == want
            && forgetting == forget
            && now < *retry
        {
            return Plan::At(*retry);
        }
        let refresh_at = match &self.registered {
            Some((done, at)) if done == want => Some(*at + REFRESH),
            _ => None,
        };
        let device = match want {
            Want::Register { .. } if refresh_at.is_some_and(|at| now < at) => None,
            Want::Register { .. } | Want::Unregister => Some(want.clone()),
            Want::Nothing => None,
        };
        let forget = if matches!(want, Want::Unregister) {
            Vec::new()
        } else {
            forget.to_vec()
        };
        match (device, refresh_at) {
            (None, _) if !forget.is_empty() => Plan::Run(Action {
                device: None,
                forget,
            }),
            (None, Some(at)) => Plan::At(at),
            (None, None) => Plan::Idle,
            (device, _) => Plan::Run(Action { device, forget }),
        }
    }

    pub fn record(&mut self, want: &Want, forget: &[String], ok: bool, now: Instant) {
        if ok {
            self.failed = None;
            match want {
                Want::Register { .. } => self.registered = Some((want.clone(), now)),
                _ => self.registered = None,
            }
            return;
        }
        let attempts = match &self.failed {
            Some((failed, forgetting, attempts, _)) if failed == want && forgetting == forget => {
                attempts + 1
            }
            _ => 1,
        };
        self.failed = Some((
            want.clone(),
            forget.to_vec(),
            attempts,
            now + backoff(attempts),
        ));
    }
}

pub(crate) fn backoff(attempts: u32) -> Duration {
    FIRST_RETRY
        .saturating_mul(1u32 << attempts.saturating_sub(1).min(12))
        .min(LAST_RETRY)
}

#[derive(Debug)]
pub enum Outcome {
    Ran(Action, Result<(), RelayError>),
    At(Instant),
    Idle,
}

/// One account's device on one relay. `step` is the whole lifecycle; `run`
/// only decides when to call it.
pub struct Device {
    dir: PathBuf,
    relay: RelayClient,
    identity: Identity,
    schedule: Schedule,
}

impl Device {
    pub fn new(dir: PathBuf, relay: RelayClient, identity: Identity) -> Self {
        Self {
            dir,
            relay,
            identity,
            schedule: Schedule::default(),
        }
    }

    pub async fn step(&mut self, platform: &Platform, relays: &[String], now: Instant) -> Outcome {
        let state = PushState::load(&self.dir);
        let want = wanted(platform, &state, &self.identity.pubkey, relays);
        let forget = stale_origins(&state, &self.identity.pubkey);
        let action = match self.schedule.plan(&want, &forget, now) {
            Plan::Run(action) => action,
            Plan::At(at) => return Outcome::At(at),
            Plan::Idle => return Outcome::Idle,
        };
        let result = self.perform(&action).await;
        self.schedule.record(&want, &forget, result.is_ok(), now);
        Outcome::Ran(action, result)
    }

    async fn perform(&self, action: &Action) -> Result<(), RelayError> {
        let (dir, relay, identity) = (&self.dir, &self.relay, &self.identity);
        match &action.device {
            Some(Want::Register {
                token,
                package,
                dm,
                relays,
            }) => {
                registration::register_device(
                    dir,
                    relay,
                    identity,
                    token,
                    package,
                    *dm,
                    relays.clone(),
                )
                .await?;
            }
            Some(Want::Unregister) => registration::unregister_device(dir, relay, identity).await?,
            Some(Want::Nothing) | None => {}
        }
        for origin in &action.forget {
            registration::forget_origin(dir, relay, identity, origin).await?;
        }
        Ok(())
    }
}

static MOUNTS: AtomicU64 = AtomicU64::new(0);

/// Retires every running loop. A loop outlives its account's view by up to an
/// await, and must not re-register a key that just signed out.
pub fn retire() -> u64 {
    MOUNTS.fetch_add(1, Ordering::SeqCst) + 1
}

/// The mounted account's loop, until `retire`. Never blocks the UI: every wait
/// is async, and a failure waits out its backoff unless an input changes.
pub async fn run(
    dir: PathBuf,
    relay: RelayClient,
    identity: Identity,
    mut relays: watch::Receiver<Vec<String>>,
) {
    let mount = retire();
    let mut device = Device::new(dir, relay, identity);
    let mut platform = platform().subscribe();
    let mut changes = changes().subscribe();
    while MOUNTS.load(Ordering::SeqCst) == mount {
        let current = platform.borrow_and_update().clone();
        changes.borrow_and_update();
        let list = relays.borrow_and_update().clone();
        let wake = match device.step(&current, &list, Instant::now()).await {
            Outcome::Ran(_, Ok(())) => {
                changed();
                continue;
            }
            Outcome::Ran(action, Err(error)) => {
                tracing::warn!(%error, ?action, "push: the relay was not updated; retrying later");
                continue;
            }
            Outcome::At(at) => Some(at),
            Outcome::Idle => None,
        };
        let sleep = async move {
            match wake {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            _ = platform.changed() => {}
            _ = changes.changed() => {}
            changed = relays.changed() => if changed.is_err() { return },
            _ = sleep => {}
        }
        tokio::time::sleep(SETTLE).await;
    }
}

/// The outgoing account's last call, made before its identity is dropped.
/// `unregister_device` clears the local registration even when offline.
pub async fn sign_out(dir: &Path, identity: &Identity) {
    retire();
    if PushState::load(dir).account.as_deref() != Some(identity.pubkey.as_str()) {
        return;
    }
    let result = match RelayClient::from_env() {
        Ok(relay) => registration::unregister_device(dir, &relay, identity).await,
        Err(error) => Err(error),
    };
    if let Err(error) = result {
        tracing::warn!(%error, "push: the relay did not hear the sign-out");
    }
    changed();
}

#[cfg(test)]
#[path = "device_tests.rs"]
mod tests;
