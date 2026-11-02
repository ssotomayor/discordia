//! Runs this account's agents while the app is open. Every task is aborted
//! when the account's view goes, so nothing outlives a sign-out.

use std::collections::HashMap;
use std::sync::Arc;

use dioxus::prelude::*;
use discordia_agent::{ApiKey, Http, Status};
use discordia_bot::BotIdentity;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use super::store::{self, AgentEntry, Secrets, ServerRef};
use crate::state::{AppState, SessionMode};

/// Each running agent is one more connection the host charges to this
/// address (trap 4), so a person cannot crowd out their own session.
pub const MAX_RUNNING: usize = 8;

pub type RunKey = (String, ServerRef);

#[derive(Debug, Clone, PartialEq)]
pub enum RunState {
    Agent(Status),
    /// Not started: missing keys, the cap, or no such server right now.
    Stopped(String),
}

#[derive(Clone)]
pub struct Agents {
    pub owner: String,
    pub list: Signal<Vec<AgentEntry>>,
    pub runs: Signal<HashMap<RunKey, RunState>>,
    tx: UnboundedSender<Cmd>,
}

impl PartialEq for Agents {
    fn eq(&self, other: &Self) -> bool {
        self.owner == other.owner
    }
}

enum Cmd {
    Sync {
        agents: Vec<AgentEntry>,
        self_host: Option<String>,
        dms: DmSetup,
    },
    /// Its keys changed on disk: drop what is cached and restart its runs.
    Rekeyed(String),
}

impl Agents {
    pub fn save(&self, agents: Vec<AgentEntry>) -> Result<(), String> {
        store::save(&self.owner, &agents)?;
        let mut list = self.list;
        list.set(agents);
        Ok(())
    }

    pub fn rekeyed(&self, agent: &str) {
        let _ = self.tx.send(Cmd::Rekeyed(agent.to_string()));
    }
}

/// What a DM run needs from the account: its relays and who its creator is.
#[derive(Clone, PartialEq)]
struct DmSetup {
    relays: Vec<String>,
    owner_name: String,
}

pub fn use_agents(
    owner: String,
    owner_name: String,
    state: Signal<AppState>,
    settings: Signal<crate::settings::ClientSettings>,
) -> Agents {
    let list = use_signal({
        let owner = owner.clone();
        move || store::load(&owner)
    });
    let runs = use_signal(HashMap::new);
    let agents = use_hook(move || {
        let (tx, rx) = unbounded_channel();
        crate::service_task::spawn(serve(owner.clone(), rx, runs));
        Agents {
            owner,
            list,
            runs,
            tx,
        }
    });
    // A hosted server's loopback address changes with each launch.
    let self_host = use_memo(move || state.read().host_info.as_ref().map(|h| h.local_url.clone()));
    let relays = use_memo(move || {
        let saved = settings.read().dm_relays.clone();
        if saved.is_empty() {
            crate::nostr::relay::DEFAULT_RELAYS
                .iter()
                .map(|r| r.to_string())
                .collect()
        } else {
            saved
        }
    });
    let tx = agents.tx.clone();
    use_effect(move || {
        let _ = tx.send(Cmd::Sync {
            agents: list(),
            self_host: self_host(),
            dms: DmSetup {
                relays: relays(),
                owner_name: owner_name.clone(),
            },
        });
    });
    agents
}

/// Where an agent added from the open session would connect.
pub fn current_server(mode: Option<&SessionMode>, s: &AppState) -> Option<ServerRef> {
    match mode? {
        SessionMode::SelfHost { .. } => Some(ServerRef::SelfHost),
        SessionMode::Remote { server_url } => Some(ServerRef::Dial {
            url: server_url.clone(),
            label: label_of(server_url),
        }),
        SessionMode::ByCode { code, .. } => s.resolved_share.clone().map(|url| ServerRef::Dial {
            url,
            label: code.clone(),
        }),
    }
}

fn label_of(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| url.chars().take(40).collect())
}

fn dm_secret(identity_b58: &str) -> Result<secp256k1::SecretKey, String> {
    let bytes = bs58::decode(identity_b58.trim())
        .into_vec()
        .map_err(|e| e.to_string())?;
    secp256k1::SecretKey::from_slice(&bytes).map_err(|e| e.to_string())
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct Running {
    _task: AbortOnDrop,
    fingerprint: String,
}

/// What a run depends on: a change to any of it restarts the run.
fn fingerprint(url: &str, e: &AgentEntry) -> String {
    let bare = AgentEntry {
        servers: Vec::new(),
        ..e.clone()
    };
    format!(
        "{url}\n{}",
        serde_json::to_string(&bare).unwrap_or_default()
    )
}

pub(crate) fn desired(agents: &[AgentEntry], self_host: Option<&str>) -> Vec<(RunKey, String)> {
    let mut out = Vec::new();
    for e in agents.iter().filter(|e| e.enabled && !e.pubkey.is_empty()) {
        for server in &e.servers {
            let url = match server {
                ServerRef::SelfHost => match self_host {
                    Some(u) => u.to_string(),
                    None => continue,
                },
                ServerRef::Dial { url, .. } => url.clone(),
                ServerRef::Dms => continue,
            };
            out.push(((e.pubkey.clone(), server.clone()), url));
        }
        if e.answer_dms {
            out.push(((e.pubkey.clone(), ServerRef::Dms), String::new()));
        }
    }
    out
}

async fn serve(
    owner: String,
    mut rx: UnboundedReceiver<Cmd>,
    mut runs: Signal<HashMap<RunKey, RunState>>,
) {
    let (status_tx, mut status_rx) = unbounded_channel::<(RunKey, Status)>();
    let mut running: HashMap<RunKey, Running> = HashMap::new();
    let mut secrets: HashMap<String, Arc<Secrets>> = HashMap::new();
    let mut wanted: (Vec<AgentEntry>, Option<String>, Option<DmSetup>) = (Vec::new(), None, None);
    loop {
        tokio::select! {
            cmd = rx.recv() => match cmd {
                None => return,
                Some(Cmd::Sync { agents, self_host, dms }) => wanted = (agents, self_host, Some(dms)),
                Some(Cmd::Rekeyed(agent)) => {
                    secrets.remove(&agent);
                    running.retain(|(a, _), _| *a != agent);
                }
            },
            Some((key, status)) = status_rx.recv() => {
                if running.contains_key(&key) {
                    runs.write().insert(key, RunState::Agent(status));
                }
                continue;
            }
        }
        let (agents, self_host, dm_setup) = &wanted;
        let want = desired(agents, self_host.as_deref());
        let by_key: HashMap<&RunKey, (&String, &AgentEntry)> = want
            .iter()
            .filter_map(|(key, url)| {
                agents
                    .iter()
                    .find(|e| e.pubkey == key.0)
                    .map(|e| (key, (url, e)))
            })
            .collect();
        running.retain(|key, run| {
            by_key
                .get(key)
                .is_some_and(|(url, e)| run.fingerprint == fingerprint(url, e))
        });
        runs.write().retain(|key, _| by_key.contains_key(key));

        for (key, url) in &want {
            if running.contains_key(key) {
                continue;
            }
            let Some((_, entry)) = by_key.get(key) else {
                continue;
            };
            if running.len() >= MAX_RUNNING {
                runs.write().insert(
                    key.clone(),
                    RunState::Stopped(crate::i18n::text(
                        "Too many agents running at once on this device.",
                        &[],
                    )),
                );
                continue;
            }
            let keys = match secrets.get(&entry.pubkey) {
                Some(s) => s.clone(),
                None => {
                    let (owner, agent) = (owner.clone(), entry.pubkey.clone());
                    match tokio::task::spawn_blocking(move || store::unseal(&owner, &agent)).await {
                        Ok(Ok(s)) => {
                            let s = Arc::new(s);
                            secrets.insert(entry.pubkey.clone(), s.clone());
                            s
                        }
                        Ok(Err(e)) => {
                            runs.write().insert(key.clone(), RunState::Stopped(e));
                            continue;
                        }
                        Err(e) => {
                            runs.write()
                                .insert(key.clone(), RunState::Stopped(e.to_string()));
                            continue;
                        }
                    }
                }
            };
            let identity = match BotIdentity::from_base58_secret(&keys.identity) {
                Ok(id) => id,
                Err(e) => {
                    runs.write()
                        .insert(key.clone(), RunState::Stopped(e.to_string()));
                    continue;
                }
            };
            let config = entry.config(ApiKey::new(keys.api_key.as_str()));
            if key.1 == ServerRef::Dms {
                let Some(setup) = dm_setup.clone() else {
                    continue;
                };
                let secret = match dm_secret(&keys.identity) {
                    Ok(s) => s,
                    Err(e) => {
                        runs.write().insert(key.clone(), RunState::Stopped(e));
                        continue;
                    }
                };
                let mut allowed: HashMap<String, String> = entry
                    .dm_allow
                    .iter()
                    .map(|k| {
                        (
                            k.clone(),
                            crate::identity::truncate_pubkey(&crate::identity::npub_of(k))
                                .to_string(),
                        )
                    })
                    .collect();
                allowed.insert(owner.clone(), setup.owner_name.clone());
                let report = {
                    let (tx, key) = (status_tx.clone(), key.clone());
                    move |status| {
                        let _ = tx.send((key.clone(), status));
                    }
                };
                let task = tokio::spawn(super::dms::answer_dms(
                    secret,
                    config,
                    allowed,
                    setup.relays.clone(),
                    report,
                ));
                runs.write()
                    .insert(key.clone(), RunState::Agent(Status::Connecting));
                running.insert(
                    key.clone(),
                    Running {
                        _task: AbortOnDrop(task),
                        fingerprint: format!("{}\n{:?}", fingerprint(url, entry), setup.relays),
                    },
                );
                continue;
            }
            let harness = Http::new(config.model.clone());
            let report = {
                let (tx, key) = (status_tx.clone(), key.clone());
                move |status| {
                    let _ = tx.send((key.clone(), status));
                }
            };
            let task = tokio::spawn(discordia_agent::run_forever(
                url.clone(),
                identity,
                config,
                harness,
                report,
            ));
            runs.write()
                .insert(key.clone(), RunState::Agent(Status::Connecting));
            running.insert(
                key.clone(),
                Running {
                    _task: AbortOnDrop(task),
                    fingerprint: fingerprint(url, entry),
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(key: &str, servers: Vec<ServerRef>, enabled: bool) -> AgentEntry {
        AgentEntry {
            pubkey: key.into(),
            name: "A".into(),
            servers,
            enabled,
            ..AgentEntry::draft()
        }
    }

    fn remote(url: &str) -> ServerRef {
        ServerRef::Dial {
            url: url.into(),
            label: url.into(),
        }
    }

    #[test]
    fn only_enabled_agents_run_and_self_host_waits_for_hosting() {
        let list = vec![
            agent("a", vec![ServerRef::SelfHost, remote("wss://x")], true),
            agent("b", vec![remote("wss://y")], false),
        ];
        let away = desired(&list, None);
        assert_eq!(away.len(), 1);
        assert_eq!(away[0].1, "wss://x");
        let hosting = desired(&list, Some("ws://127.0.0.1:5000"));
        assert_eq!(hosting.len(), 2);
        assert_eq!(hosting[0].1, "ws://127.0.0.1:5000");
    }

    #[test]
    fn a_settings_change_restarts_a_run_but_adding_a_server_does_not() {
        let a = agent("a", vec![remote("wss://x")], true);
        let mut more = a.clone();
        more.servers.push(remote("wss://y"));
        assert_eq!(fingerprint("wss://x", &a), fingerprint("wss://x", &more));
        let mut changed = a.clone();
        changed.prompt = "new".into();
        assert_ne!(fingerprint("wss://x", &a), fingerprint("wss://x", &changed));
    }
}
