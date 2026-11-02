//! The operator's levers over what guilds install (#455, trap 39): a policy,
//! approved and banned keys, every install at a glance, and offered agents.

use std::sync::Arc;

use crate::AppContext;
use crate::protocol::{AppsPolicy, OfferedAgent, ServerAppInstall, ServerMessage};
use crate::state::AppState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppRule {
    Approved,
    Banned,
}

impl AppRule {
    fn stored(self) -> &'static str {
        match self {
            AppRule::Approved => "approved",
            AppRule::Banned => "banned",
        }
    }

    pub fn from_stored(s: &str) -> Option<Self> {
        match s {
            "approved" => Some(AppRule::Approved),
            "banned" => Some(AppRule::Banned),
            _ => None,
        }
    }
}

pub fn policy(state: &AppState) -> AppsPolicy {
    *state.apps_policy.lock().unwrap_or_else(|e| e.into_inner())
}

fn rule(state: &AppState, pubkey: &str) -> Option<AppRule> {
    state.app_rules.get(pubkey).map(|r| *r)
}

/// Checked inside `install_bot`, after the guild's own permission check.
pub fn may_install(state: &AppState, bot_pubkey: &str, by: &str) -> Result<(), String> {
    if rule(state, bot_pubkey) == Some(AppRule::Banned) {
        return Err("this server's operator has banned that app".into());
    }
    let operator = state.operators.contains(by);
    let offered = state.server_agents.get(bot_pubkey).map(|a| a.spec.offered);
    match offered {
        Some(true) => return Ok(()),
        Some(false) if !operator => {
            return Err(
                "that agent belongs to this server's operator, who hasn't offered it to other guilds"
                    .into(),
            );
        }
        _ => {}
    }
    if operator {
        return Ok(());
    }
    match policy(state) {
        AppsPolicy::Open => Ok(()),
        AppsPolicy::Approved if rule(state, bot_pubkey) == Some(AppRule::Approved) => Ok(()),
        AppsPolicy::Approved => Err(format!(
            "this server only runs apps its operator approved; ask them to approve {}",
            &bot_pubkey[..bot_pubkey.len().min(12)]
        )),
        AppsPolicy::Off => Err("this server doesn't allow installing apps".into()),
    }
}

pub fn snapshot(state: &AppState) -> ServerMessage {
    let mut approved = Vec::new();
    let mut banned = Vec::new();
    for r in state.app_rules.iter() {
        match *r.value() {
            AppRule::Approved => approved.push(r.key().clone()),
            AppRule::Banned => banned.push(r.key().clone()),
        }
    }
    approved.sort();
    banned.sort();
    let mut installs: Vec<ServerAppInstall> = state
        .bot_installs
        .iter()
        .flat_map(|per_guild| {
            per_guild
                .iter()
                .map(|i| i.value().clone())
                .collect::<Vec<_>>()
        })
        .map(|install| ServerAppInstall {
            guild_name: state
                .guilds
                .get(&install.guild_id)
                .map(|g| g.name.clone())
                .unwrap_or_default(),
            install,
        })
        .collect();
    installs.sort_by(|a, b| {
        (a.guild_name.as_str(), a.install.name.as_str())
            .cmp(&(b.guild_name.as_str(), b.install.name.as_str()))
    });
    ServerMessage::ServerApps {
        policy: policy(state),
        approved,
        banned,
        installs,
    }
}

pub fn share(state: &AppState) {
    let operators: Vec<String> = state.operators.iter().cloned().collect();
    state.deliver(operators, snapshot(state));
}

pub fn offered(state: &AppState) -> ServerMessage {
    let mut agents: Vec<OfferedAgent> = state
        .server_agents
        .iter()
        .filter(|a| a.spec.offered)
        .map(|a| OfferedAgent {
            pubkey: a.key().clone(),
            name: a.spec.name.clone(),
            provider_label: a.spec.provider_label.clone(),
            model: a.spec.model.clone(),
            guild_daily_tokens: a.spec.guild_daily_tokens,
        })
        .collect();
    agents.sort_by(|a, b| a.name.cmp(&b.name));
    ServerMessage::OfferedAgents { agents }
}

fn require_operator(state: &AppState, by: &str) -> Result<(), String> {
    state
        .operators
        .contains(by)
        .then_some(())
        .ok_or_else(|| "only this server's operator sets which apps it allows".to_string())
}

pub async fn set_policy(ctx: &Arc<AppContext>, by: &str, next: AppsPolicy) -> Result<(), String> {
    let state = &ctx.state;
    require_operator(state, by)?;
    let stored = serde_json::to_string(&next).map_err(|e| e.to_string())?;
    state
        .store
        .set_apps_policy(&stored)
        .await
        .map_err(|e| format!("couldn't save the policy: {e}"))?;
    *state.apps_policy.lock().unwrap_or_else(|e| e.into_inner()) = next;
    share(state);
    Ok(())
}

pub async fn set_approved(
    ctx: &Arc<AppContext>,
    by: &str,
    pubkey: &str,
    approved: bool,
) -> Result<(), String> {
    let state = &ctx.state;
    require_operator(state, by)?;
    if !crate::protocol::is_canonical_pubkey(pubkey) {
        return Err("an app's key is 64 lowercase hex characters".into());
    }
    let next = approved.then_some(AppRule::Approved);
    match (rule(state, pubkey), approved) {
        (Some(AppRule::Banned), true) => return Err("lift the ban before approving it".into()),
        (Some(AppRule::Approved), true) | (Some(AppRule::Banned) | None, false) => return Ok(()),
        _ => {}
    }
    write_rule(state, pubkey, next).await?;
    share(state);
    Ok(())
}

pub async fn set_banned(
    ctx: &Arc<AppContext>,
    by: &str,
    pubkey: &str,
    banned: bool,
) -> Result<(), String> {
    let state = &ctx.state;
    require_operator(state, by)?;
    if !crate::protocol::is_canonical_pubkey(pubkey) {
        return Err("an app's key is 64 lowercase hex characters".into());
    }
    if state.operators.contains(pubkey) {
        return Err("that key is an operator's, not an app's".into());
    }
    if !banned && rule(state, pubkey) != Some(AppRule::Banned) {
        return Ok(());
    }
    write_rule(state, pubkey, banned.then_some(AppRule::Banned)).await?;
    if banned {
        state.agent_host.stop(pubkey);
        for install in state.bot_guilds(pubkey) {
            match state.evict_app(install.guild_id, pubkey).await {
                Ok(seat) => crate::gateway::connection::announce_uninstall(
                    ctx,
                    install.guild_id,
                    pubkey,
                    seat,
                ),
                Err(e) => {
                    tracing::warn!(app = pubkey, guild = %install.guild_id, error = %e, "banned app not removed")
                }
            }
        }
    }
    share(state);
    Ok(())
}

async fn write_rule(state: &AppState, pubkey: &str, next: Option<AppRule>) -> Result<(), String> {
    state
        .store
        .set_app_rule(pubkey, next.map(AppRule::stored))
        .await
        .map_err(|e| format!("couldn't save that: {e}"))?;
    match next {
        Some(r) => {
            state.app_rules.insert(pubkey.to_string(), r);
        }
        None => {
            state.app_rules.remove(pubkey);
        }
    }
    Ok(())
}
