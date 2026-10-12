use std::time::{Duration, Instant};

use crate::protocol::BotCommand;

use super::AppState;
use crate::protocol::{ArgValue, BotCommandSet, ChannelKind, CommandArg, Id, Invocation, User};

/// How long a bot may answer a press privately. Past it the id is dead, so a
/// bot cannot hoard ids and message people whenever it likes.
const INVOCATION_TTL: Duration = Duration::from_secs(15 * 60);
const MAX_PENDING_INVOCATIONS: usize = 4096;

pub struct Pending {
    bot_pubkey: String,
    invoker_pubkey: String,
    guild_id: Id,
    channel_id: Id,
    via: Vec<String>,
    at: Instant,
}

/// Calls one app may make to another in a minute, so two of them cannot
/// drive each other flat out.
const CALLS_PER_MINUTE: u32 = 20;

impl AppState {
    /// The guilds whose members must be told, which is every guild the bot is
    /// installed in.
    pub fn register_commands(
        &self,
        bot_pubkey: &str,
        commands: Vec<BotCommand>,
    ) -> Result<(Vec<Id>, Vec<BotCommand>), String> {
        let commands = crate::protocol::validate_commands(commands)?;
        self.bot_commands
            .insert(bot_pubkey.to_string(), commands.clone());
        let guilds = self
            .bot_guilds(bot_pubkey)
            .iter()
            .map(|i| i.guild_id)
            .collect();
        Ok((guilds, commands))
    }

    pub fn commands_of(&self, bot_pubkey: &str) -> Option<BotCommandSet> {
        self.bot_commands.get(bot_pubkey).map(|c| BotCommandSet {
            bot_pubkey: bot_pubkey.to_string(),
            commands: c.clone(),
        })
    }

    pub fn commands_for_guilds(&self, guild_ids: &[Id]) -> Vec<BotCommandSet> {
        let mut bots: Vec<String> = guild_ids
            .iter()
            .flat_map(|g| self.guild_installs(*g))
            .map(|i| i.bot_pubkey)
            .collect();
        bots.sort_unstable();
        bots.dedup();
        bots.iter().filter_map(|b| self.commands_of(b)).collect()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn invoke_command(
        &self,
        invoker: &User,
        guild_id: Id,
        channel_id: Id,
        bot_pubkey: &str,
        command: &str,
        args: Vec<CommandArg>,
        parent: Option<Id>,
        invoker_is_bot: bool,
    ) -> Result<Invocation, String> {
        if !self.is_guild_member(guild_id, &invoker.pubkey) {
            return Err("you're not a member of this guild".into());
        }
        if self.bot_install(guild_id, bot_pubkey).is_none() {
            return Err("that bot isn't installed here".into());
        }
        let channel = self
            .channel(channel_id)
            .filter(|c| c.guild_id == guild_id && c.kind == ChannelKind::Text)
            .ok_or("a bot's commands run in one of this guild's text channels")?;
        if !self.can_see_channel(&invoker.pubkey, &channel)
            || !self.can_see_channel(bot_pubkey, &channel)
        {
            return Err("that channel is hidden from you or from the bot".into());
        }
        let cmd = self
            .bot_commands
            .get(bot_pubkey)
            .and_then(|list| list.iter().find(|c| c.id == command).cloned())
            .ok_or("that bot has no such command")?;
        let via = if invoker_is_bot {
            self.app_call_chain(guild_id, &invoker.pubkey, bot_pubkey, &cmd, parent)?
        } else {
            if let Some(perm) = cmd.permission {
                self.require_permission(guild_id, &invoker.pubkey, perm)?;
            }
            vec![invoker.pubkey.clone()]
        };
        let args = crate::protocol::check_args(&cmd, args)?;
        let users = self.resolve_picked(guild_id, &args)?;
        if !self.has_sessions(bot_pubkey) {
            return Err("that bot is offline".into());
        }

        self.invocations
            .retain(|_, p| p.at.elapsed() < INVOCATION_TTL);
        if self.invocations.len() >= MAX_PENDING_INVOCATIONS {
            return Err("too many commands waiting on bots, try again shortly".into());
        }
        let id = Id::new_v4();
        self.invocations.insert(
            id,
            Pending {
                bot_pubkey: bot_pubkey.to_string(),
                invoker_pubkey: invoker.pubkey.clone(),
                guild_id,
                channel_id,
                via: via.clone(),
                at: Instant::now(),
            },
        );
        Ok(Invocation {
            id,
            guild_id,
            channel_id,
            invoker: invoker.clone(),
            command: cmd.id,
            args,
            users,
            via,
        })
    }

    /// An app calling an app: the guild allowed the pair, the command opted
    /// in, and the call is held to the calling install *and* to the person who
    /// started the chain — else a low app borrows a high one's powers.
    fn app_call_chain(
        &self,
        guild_id: Id,
        caller: &str,
        callee: &str,
        cmd: &BotCommand,
        parent: Option<Id>,
    ) -> Result<Vec<String>, String> {
        let install = self
            .bot_install(guild_id, caller)
            .ok_or("the calling app isn't installed here")?;
        if !install.may_call.iter().any(|k| k == callee) {
            return Err("this guild hasn't allowed that app to call this one".into());
        }
        if !cmd.callable_by_apps {
            return Err("that command can't be called by other apps".into());
        }
        let mut via = match parent {
            None => Vec::new(),
            Some(id) => self
                .invocations
                .get(&id)
                .filter(|p| p.bot_pubkey == caller && p.guild_id == guild_id)
                .map(|p| p.via.clone())
                .ok_or("that parent invocation isn't one this app is handling")?,
        };
        via.push(caller.to_string());
        if via.iter().any(|k| k == callee) {
            return Err("that call would loop back to an app already in the chain".into());
        }
        if via.len() > crate::protocol::MAX_CALL_DEPTH {
            return Err("that call chain is too deep".into());
        }
        if let Some(perm) = cmd.permission {
            if !install.has_permission(perm) {
                return Err(format!("the calling app lacks {perm:?}"));
            }
            let root = &via[0];
            if self.bot_install(guild_id, root).is_none() {
                self.require_permission(guild_id, root, perm)?;
            }
        }
        self.spend_call(caller, callee)?;
        Ok(via)
    }

    fn spend_call(&self, caller: &str, callee: &str) -> Result<(), String> {
        let now = Instant::now();
        let key = (caller.to_string(), callee.to_string());
        let mut slot = self.app_calls.entry(key).or_insert((now, 0));
        if now.duration_since(slot.0) >= Duration::from_secs(60) {
            *slot = (now, 0);
        }
        if slot.1 >= CALLS_PER_MINUTE {
            return Err("that app is calling this one too often".into());
        }
        slot.1 += 1;
        Ok(())
    }

    pub async fn set_app_calls(
        &self,
        guild_id: Id,
        bot_pubkey: &str,
        may_call: Vec<String>,
        by_pubkey: &str,
    ) -> Result<(), String> {
        let _write = self.durable_writes.lock().await;
        self.require_permission(
            guild_id,
            by_pubkey,
            crate::protocol::Permission::ManageGuild,
        )?;
        let mut install = self
            .bot_install(guild_id, bot_pubkey)
            .ok_or("that bot isn't installed here")?;
        let mut may_call: Vec<String> = may_call
            .into_iter()
            .filter(|k| k != bot_pubkey && self.bot_install(guild_id, k).is_some())
            .collect();
        may_call.sort_unstable();
        may_call.dedup();
        install.may_call = may_call;
        super::durable(self.store.upsert_bot_install(&install).await, "app calls")?;
        self.bot_installs
            .entry(install.bot_pubkey.clone())
            .or_default()
            .insert(guild_id, install);
        Ok(())
    }

    /// Spends the id: one private answer per press. Returns who to send it to.
    pub fn take_invocation(&self, bot_pubkey: &str, id: Id) -> Result<(String, Id), String> {
        let (_, pending) = self
            .invocations
            .remove_if(&id, |_, p| p.bot_pubkey == bot_pubkey)
            .ok_or("unknown or already answered invocation")?;
        if pending.at.elapsed() >= INVOCATION_TTL {
            return Err("that invocation has expired".into());
        }
        Ok((pending.invoker_pubkey, pending.channel_id))
    }

    fn resolve_picked(&self, guild_id: Id, args: &[CommandArg]) -> Result<Vec<User>, String> {
        let keys: Vec<&String> = args
            .iter()
            .filter_map(|a| match &a.value {
                ArgValue::Members(k) => Some(k),
                _ => None,
            })
            .flatten()
            .collect();
        let Some(members) = self.members.get(&guild_id) else {
            return Err("unknown guild".into());
        };
        let mut users: Vec<User> = Vec::new();
        for k in keys {
            if users.iter().any(|u| &u.pubkey == k) {
                continue;
            }
            let user = members
                .get(k)
                .map(|m| m.user.clone())
                .ok_or("someone picked isn't a member of this guild")?;
            users.push(user);
        }
        Ok(users)
    }
}
