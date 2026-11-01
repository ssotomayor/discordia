//! Agents the operator runs in this process (#454, trap 39). Each one dials
//! this gateway over loopback as a bot, so every check a bot meets applies.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, Weak};

use discordia_agent::{AgentConfig, ApiKey, Http, Limits, ModelConfig, Provider, Status};
use discordia_bot::BotIdentity;

use crate::AppContext;
use crate::protocol::{
    AgentProvider, Id, MAX_SERVER_AGENTS, ServerAgentSpec, ServerAgentView, ServerMessage,
    validate_server_agent,
};
use crate::state::AppState;

#[derive(Clone)]
pub struct ServerAgent {
    pub spec: ServerAgentSpec,
    pub secret: String,
    pub api_key: String,
    pub version: u64,
}

impl ServerAgent {
    pub fn config(&self) -> AgentConfig {
        let s = &self.spec;
        AgentConfig {
            name: s.name.clone(),
            prompt: s.prompt.clone(),
            model: ModelConfig {
                provider: match s.provider {
                    AgentProvider::Anthropic => Provider::Anthropic,
                    AgentProvider::OpenAiCompatible => Provider::OpenAiCompatible,
                },
                provider_label: s.provider_label.clone(),
                base_url: s.base_url.clone(),
                model: s.model.clone(),
                api_key: ApiKey::new(self.api_key.clone()),
                max_output_tokens: 4096,
            },
            limits: Limits {
                per_member_per_hour: s.per_member_per_hour,
                daily_tokens: s.daily_tokens,
                ..Limits::default()
            },
            manifest_version: self.version,
        }
    }

    fn identity(&self) -> Result<BotIdentity, String> {
        BotIdentity::from_base58_secret(&self.secret).map_err(|e| e.to_string())
    }
}

/// What a host shows: a change to any of it makes a new manifest version.
fn shown(s: &ServerAgentSpec) -> (&str, &str, &str) {
    (&s.name, &s.provider_label, &s.model)
}

/// The gateway's own plain socket, which only accepts loopback (trap 15).
pub fn loopback_gateway(addr: SocketAddr) -> Option<String> {
    let ip = addr.ip();
    let ip = if ip.is_unspecified() {
        match ip {
            std::net::IpAddr::V4(_) => std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            std::net::IpAddr::V6(_) => std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST),
        }
    } else if ip.is_loopback() {
        ip
    } else {
        return None;
    };
    Some(format!("ws://{}", SocketAddr::new(ip, addr.port())))
}

#[derive(Default)]
struct Inner {
    url: Option<String>,
    running: HashMap<String, tokio::task::AbortHandle>,
    status: HashMap<String, Status>,
}

/// Owns the tasks; `AppState` owns it. Tasks hold the state weakly, so a
/// dropped server is not kept alive by its own agents.
#[derive(Default)]
pub struct AgentHost {
    inner: Mutex<Inner>,
}

impl AgentHost {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Once the gateway listens. Before that there is nothing to dial.
    pub fn start(&self, state: &Arc<AppState>, url: String) {
        self.lock().url = Some(url);
        let keys: Vec<String> = state
            .server_agents
            .iter()
            .map(|a| a.key().clone())
            .collect();
        for key in keys {
            self.restart(state, &key);
        }
    }

    pub fn restart(&self, state: &Arc<AppState>, pubkey: &str) {
        self.stop(pubkey);
        let Some(url) = self.lock().url.clone() else {
            return;
        };
        let Some(agent) = state.server_agents.get(pubkey).map(|a| a.clone()) else {
            return;
        };
        let identity = match agent.identity() {
            Ok(id) => id,
            Err(e) => {
                tracing::error!(agent = pubkey, error = %e, "server agent key unreadable");
                return;
            }
        };
        let config = agent.config();
        let harness = Http::new(config.model.clone());
        let weak: Weak<AppState> = Arc::downgrade(state);
        let key = pubkey.to_string();
        let report = move |status: Status| {
            if let Some(state) = weak.upgrade() {
                state.agent_host.lock().status.insert(key.clone(), status);
                share(&state);
            }
        };
        let task = tokio::spawn(discordia_agent::run_forever(
            format!("{url}/gateway"),
            identity,
            config,
            harness,
            report,
        ));
        self.lock()
            .running
            .insert(pubkey.to_string(), task.abort_handle());
    }

    pub fn stop(&self, pubkey: &str) {
        let mut inner = self.lock();
        if let Some(task) = inner.running.remove(pubkey) {
            task.abort();
        }
        inner.status.remove(pubkey);
    }

    /// With the gateway: a stopped host leaves nothing dialling it (trap 17).
    pub fn stop_all(&self) {
        let mut inner = self.lock();
        inner.url = None;
        for (_, task) in inner.running.drain() {
            task.abort();
        }
        inner.status.clear();
    }

    fn status_of(&self, pubkey: &str) -> (bool, String) {
        match self.lock().status.get(pubkey) {
            Some(Status::Online) => (true, "online".into()),
            Some(Status::Connecting) => (false, "connecting".into()),
            Some(Status::Retrying { error, in_secs }) => {
                (false, format!("retrying in {in_secs}s: {error}"))
            }
            None => (false, "not running".into()),
        }
    }
}

pub fn views(state: &AppState) -> Vec<ServerAgentView> {
    let mut out: Vec<ServerAgentView> = state
        .server_agents
        .iter()
        .map(|a| {
            let (online, status) = state.agent_host.status_of(a.key());
            let tail: String = a
                .api_key
                .chars()
                .rev()
                .take(4)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            ServerAgentView {
                pubkey: a.key().clone(),
                spec: a.spec.clone(),
                key_hint: if a.api_key.is_empty() {
                    String::new()
                } else {
                    tail
                },
                online,
                status,
            }
        })
        .collect();
    out.sort_by(|a, b| a.spec.name.cmp(&b.spec.name));
    out
}

pub fn share(state: &AppState) {
    let operators: Vec<String> = state.operators.iter().cloned().collect();
    state.deliver(
        operators,
        ServerMessage::ServerAgents {
            agents: views(state),
        },
    );
}

fn require_operator(state: &AppState, by: &str) -> Result<(), String> {
    state
        .operators
        .contains(by)
        .then_some(())
        .ok_or_else(|| "only this server's operator runs agents in it".to_string())
}

pub async fn save(
    ctx: &Arc<AppContext>,
    by: &str,
    pubkey: Option<String>,
    spec: ServerAgentSpec,
    api_key: Option<String>,
) -> Result<String, String> {
    let state = &ctx.state;
    require_operator(state, by)?;
    let spec = validate_server_agent(spec)?;
    for guild_id in &spec.guilds {
        state.require_permission(*guild_id, by, crate::protocol::Permission::ManageGuild)?;
    }
    let previous = match &pubkey {
        Some(k) => Some(
            state
                .server_agents
                .get(k)
                .map(|a| a.clone())
                .ok_or("there is no such agent on this server")?,
        ),
        None => None,
    };
    if previous.is_none() && state.server_agents.len() >= MAX_SERVER_AGENTS {
        return Err(format!("a server runs at most {MAX_SERVER_AGENTS} agents"));
    }
    let (pubkey, secret) = match (&pubkey, &previous) {
        (Some(k), Some(p)) => (k.clone(), p.secret.clone()),
        _ => {
            let id = BotIdentity::generate();
            (id.pubkey().to_string(), id.secret_base58())
        }
    };
    let api_key = match (
        api_key
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty()),
        &previous,
    ) {
        (Some(k), _) => k,
        (None, Some(p)) => p.api_key.clone(),
        (None, None) if spec.provider == AgentProvider::Anthropic => {
            return Err("paste an API key".into());
        }
        (None, None) => String::new(),
    };
    let version = match &previous {
        Some(p) if shown(&p.spec) == shown(&spec) => p.version,
        Some(p) => p.version + 1,
        None => 1,
    };
    let agent = ServerAgent {
        spec,
        secret,
        api_key,
        version,
    };
    let spec_json = serde_json::to_string(&agent.spec).map_err(|e| e.to_string())?;
    state
        .store
        .save_server_agent(&pubkey, &spec_json, &agent.secret, &agent.api_key, version)
        .await
        .map_err(|e| format!("couldn't save the agent: {e}"))?;
    state.server_agents.insert(pubkey.clone(), agent.clone());

    let config = agent.config();
    let manifest = config.manifest();
    let event = agent.identity()?.sign_manifest(&manifest);
    match state.register_manifest(&pubkey, &event).await {
        Ok(crate::state::ManifestOutcome::Recorded { changed }) => {
            for (guild_id, seat) in changed {
                crate::gateway::connection::announce_seat(ctx, guild_id, &pubkey, seat);
                crate::gateway::connection::share_guild_apps(state, guild_id);
            }
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(agent = %pubkey, error = %e, "server agent manifest not recorded"),
    }

    // One guild refusing must not leave the rest, or the agent, half done.
    let mut failures = Vec::new();
    let want: HashSet<Id> = agent.spec.guilds.iter().copied().collect();
    let have: HashSet<Id> = state
        .bot_guilds(&pubkey)
        .iter()
        .map(|i| i.guild_id)
        .collect();
    for guild_id in want.intersection(&have) {
        if state
            .bot_install(*guild_id, &pubkey)
            .is_some_and(|i| i.pending.is_some())
        {
            match state.approve_app_update(*guild_id, &pubkey, by).await {
                Ok(seat) => {
                    crate::gateway::connection::announce_seat(ctx, *guild_id, &pubkey, seat);
                    crate::gateway::connection::share_guild_apps(state, *guild_id);
                }
                Err(e) => failures.push(e),
            }
        }
    }
    for guild_id in want.difference(&have) {
        match state
            .install_bot(
                *guild_id,
                &pubkey,
                "",
                manifest.permissions.clone(),
                manifest.intents.clone(),
                Some(event.clone()),
                by,
            )
            .await
        {
            Ok((_, seat)) => {
                crate::gateway::connection::announce_install(ctx, *guild_id, &pubkey, seat)
            }
            Err(e) => failures.push(e),
        }
    }
    for guild_id in have.difference(&want) {
        match state.uninstall_bot(*guild_id, &pubkey, by).await {
            Ok(seat) => {
                crate::gateway::connection::announce_uninstall(ctx, *guild_id, &pubkey, seat)
            }
            Err(e) => failures.push(e),
        }
    }

    state.agent_host.restart(state, &pubkey);
    share(state);
    if failures.is_empty() {
        Ok(pubkey)
    } else {
        Err(format!("saved, but {}", failures.join("; ")))
    }
}

pub async fn delete(ctx: &Arc<AppContext>, by: &str, pubkey: &str) -> Result<(), String> {
    let state = &ctx.state;
    require_operator(state, by)?;
    if !state.server_agents.contains_key(pubkey) {
        return Err("there is no such agent on this server".into());
    }
    state.agent_host.stop(pubkey);
    for install in state.bot_guilds(pubkey) {
        match state.uninstall_bot(install.guild_id, pubkey, by).await {
            Ok(seat) => {
                crate::gateway::connection::announce_uninstall(ctx, install.guild_id, pubkey, seat)
            }
            // Offline from now on; that guild's admins can still remove its row.
            Err(e) => {
                tracing::warn!(agent = pubkey, guild = %install.guild_id, error = %e, "server agent left installed")
            }
        }
    }
    state
        .store
        .delete_server_agent(pubkey)
        .await
        .map_err(|e| format!("couldn't delete the agent: {e}"))?;
    state.server_agents.remove(pubkey);
    share(state);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agents_dial_the_loopback_face_of_whatever_was_bound() {
        assert_eq!(
            loopback_gateway("0.0.0.0:9000".parse().unwrap()).as_deref(),
            Some("ws://127.0.0.1:9000")
        );
        assert_eq!(
            loopback_gateway("[::]:9000".parse().unwrap()).as_deref(),
            Some("ws://[::1]:9000")
        );
        assert_eq!(
            loopback_gateway("127.0.0.1:5000".parse().unwrap()).as_deref(),
            Some("ws://127.0.0.1:5000")
        );
        assert_eq!(loopback_gateway("192.168.1.4:9000".parse().unwrap()), None);
    }
}
