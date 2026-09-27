use std::time::{Duration, Instant};

use super::AppState;
use crate::protocol::{
    ArgValue, BotCommand, BotCommandSet, ChannelKind, CommandArg, Id, Invocation, User,
};

/// How long a bot may answer a press privately. Past it the id is dead, so a
/// bot cannot hoard ids and message people whenever it likes.
const INVOCATION_TTL: Duration = Duration::from_secs(15 * 60);
const MAX_PENDING_INVOCATIONS: usize = 4096;

pub struct Pending {
    bot_pubkey: String,
    invoker_pubkey: String,
    channel_id: Id,
    at: Instant,
}

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

    pub fn invoke_command(
        &self,
        invoker: &User,
        guild_id: Id,
        channel_id: Id,
        bot_pubkey: &str,
        command: &str,
        args: Vec<CommandArg>,
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
        if let Some(perm) = cmd.permission {
            self.require_permission(guild_id, &invoker.pubkey, perm)?;
        }
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
                channel_id,
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
        })
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
