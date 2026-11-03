//! Server Settings: what only the operator changes, about the whole server.
//! The launch configuration (flags, environment, config file, the self-host
//! form) wins over what the dialog saved, which wins over the defaults.

use std::sync::Arc;

use crate::AppContext;
use crate::protocol::rendezvous::GeoPoint;
use crate::protocol::{
    LaunchSource, ServerInfo, ServerLimits, ServerMessage, ServerSettingPins, ServerSettings,
    ServerSettingsView,
};
use crate::rendezvous::PublishOptions;
use crate::state::AppState;

/// `Some` fixes a field for this run. The outer `Option` of an optional field
/// is the pin, so a self-host form that left the name blank still pins it.
#[derive(Debug, Clone, Default)]
pub struct LaunchSettings {
    pub name: Option<Option<String>>,
    pub description: Option<Option<String>>,
    pub listed_public: Option<bool>,
    pub location: Option<Option<GeoPoint>>,
    pub media_max_bytes: Option<u64>,
    pub source: LaunchSource,
    /// Whether anything registers on a rendezvous.
    pub publishes: bool,
}

impl LaunchSettings {
    pub fn pins(&self) -> ServerSettingPins {
        ServerSettingPins {
            name: self.name.is_some(),
            description: self.description.is_some(),
            listed_public: self.listed_public.is_some(),
            location: self.location.is_some(),
            media_max_bytes: self.media_max_bytes.is_some(),
        }
    }

    fn apply(&self, saved: &ServerSettings) -> ServerSettings {
        let blank = |s: Option<String>| s.filter(|s| !s.trim().is_empty());
        ServerSettings {
            name: self.name.clone().map_or(saved.name.clone(), blank),
            description: self
                .description
                .clone()
                .map_or(saved.description.clone(), blank),
            listed_public: self.listed_public.unwrap_or(saved.listed_public),
            location: self.location.unwrap_or(saved.location),
            media_max_bytes: self.media_max_bytes.unwrap_or(saved.media_max_bytes),
            limits: saved.limits.clone(),
        }
    }
}

/// `saved` is only ever what the dialog sent, so dropping a variable later
/// brings back the operator's own choice rather than the variable's.
#[derive(Debug, Default)]
pub struct Settings {
    saved: ServerSettings,
    launch: LaunchSettings,
}

impl Settings {
    pub fn effective(&self) -> ServerSettings {
        self.launch.apply(&self.saved)
    }
}

pub fn effective(state: &AppState) -> ServerSettings {
    lock(state).effective()
}

pub fn limits(state: &AppState) -> ServerLimits {
    lock(state).saved.limits.clone()
}

pub fn info(state: &AppState) -> ServerInfo {
    let settings = effective(state);
    ServerInfo {
        name: settings.name,
        limits: settings.limits,
    }
}

pub fn view(state: &AppState) -> ServerSettingsView {
    let s = lock(state);
    ServerSettingsView {
        settings: s.effective(),
        pinned: s.launch.pins(),
        source: s.launch.source,
        publishes: s.launch.publishes,
        media_used_bytes: state.media.used_bytes(),
    }
}

fn lock(state: &AppState) -> std::sync::MutexGuard<'_, Settings> {
    state
        .server_settings
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn publish_options(settings: &ServerSettings) -> PublishOptions {
    PublishOptions {
        publish_name: settings.name.clone(),
        description: settings.description.clone(),
        publish_public: settings.listed_public,
        location: settings.location,
    }
}

/// Once at start, before the first connection. A saved value that no longer
/// validates is dropped whole for the defaults rather than half-applied.
pub async fn load(state: &AppState, launch: LaunchSettings) -> Result<(), sqlx::Error> {
    let saved = match state.store.server_settings().await? {
        Some(json) => match serde_json::from_str(&json)
            .map_err(|e| e.to_string())
            .and_then(crate::protocol::validate_server_settings)
        {
            Ok(saved) => saved,
            Err(error) => {
                tracing::warn!(%error, "saved server settings unreadable; using the defaults");
                ServerSettings::default()
            }
        },
        None => ServerSettings::default(),
    };
    let effective = {
        let mut s = lock(state);
        *s = Settings { saved, launch };
        s.effective()
    };
    state.media.set_max_bytes(effective.media_max_bytes);
    state
        .publish_options
        .send_replace(publish_options(&effective));
    Ok(())
}

pub async fn set(ctx: &Arc<AppContext>, by: &str, next: ServerSettings) -> Result<(), String> {
    let state = &ctx.state;
    if !state.operators.contains(by) {
        return Err("only this server's operator changes its settings".into());
    }
    let next = crate::protocol::validate_server_settings(next)?;
    let _write = state.durable_writes.lock().await;
    let (saved, pins) = {
        let s = lock(state);
        (s.saved.clone(), s.launch.pins())
    };
    let next = ServerSettings {
        name: if pins.name { saved.name } else { next.name },
        description: if pins.description {
            saved.description
        } else {
            next.description
        },
        listed_public: if pins.listed_public {
            saved.listed_public
        } else {
            next.listed_public
        },
        location: if pins.location {
            saved.location
        } else {
            next.location
        },
        media_max_bytes: if pins.media_max_bytes {
            saved.media_max_bytes
        } else {
            next.media_max_bytes
        },
        limits: next.limits,
    };
    let json = serde_json::to_string(&next).map_err(|e| e.to_string())?;
    state
        .store
        .set_server_settings(&json)
        .await
        .map_err(|e| format!("couldn't save the settings: {e}"))?;
    let effective = {
        let mut s = lock(state);
        s.saved = next;
        s.effective()
    };
    state.media.set_max_bytes(effective.media_max_bytes);
    let options = publish_options(&effective);
    state.publish_options.send_if_modified(|current| {
        let changed = current.publish_name != options.publish_name
            || current.description != options.description
            || current.publish_public != options.publish_public
            || current.location != options.location;
        *current = options;
        changed
    });
    state.deliver(
        state.connected_pubkeys(),
        ServerMessage::ServerInfo(info(state)),
    );
    share(state);
    Ok(())
}

pub fn share(state: &AppState) {
    let operators: Vec<String> = state.operators.iter().cloned().collect();
    state.deliver(
        operators,
        ServerMessage::ServerSettings { view: view(state) },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_launch_configuration_wins_and_a_blank_pin_clears() {
        let saved = ServerSettings {
            name: Some("Saved".into()),
            description: Some("From the dialog".into()),
            media_max_bytes: 64 << 20,
            ..ServerSettings::default()
        };
        let launch = LaunchSettings {
            name: Some(Some("From env".into())),
            description: Some(Some("  ".into())),
            ..LaunchSettings::default()
        };
        let effective = launch.apply(&saved);
        assert_eq!(effective.name.as_deref(), Some("From env"));
        assert_eq!(effective.description, None);
        assert_eq!(effective.media_max_bytes, 64 << 20);
        let pins = launch.pins();
        assert!(pins.name && pins.description && !pins.media_max_bytes);
    }
}
