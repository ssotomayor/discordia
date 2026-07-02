mod seed;

use std::sync::RwLock;

use dashmap::DashMap;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::protocol::{
    BotInstall, Channel, DmInfo, Guild, Id, Intent, Member, Message, Permission, Profile,
    ServerMessage, User, VoiceState,
};

const BROADCAST_CAPACITY: usize = 256;

/// Max length of an inline image data URL (~2.2 MB of raw bytes once base64
/// is accounted for). Keeps a single broadcast frame from getting absurd.
pub const MAX_IMAGE_LEN: usize = 3_000_000;

/// A hub payload plus optional addressing. `to == None` is a normal broadcast
/// to every connected client; `to == Some(pubkeys)` is delivered only to the
/// connections whose identified user is in the set (used for direct messages
/// so DM frames never reach non-participants).
#[derive(Clone)]
pub struct Envelope {
    pub msg: ServerMessage,
    pub to: Option<Vec<String>>,
}

/// A one-to-one direct-message channel. Participants are stored as sorted
/// pubkeys so the pair maps to a single channel regardless of who opened it.
#[derive(Clone)]
pub struct DmChannel {
    pub id: Id,
    pub participants: [String; 2],
}

pub struct AppState {
    pub guilds: DashMap<Id, Guild>,
    pub channels: DashMap<Id, Channel>,
    pub channels_by_guild: DashMap<Id, Vec<Id>>,
    pub messages: DashMap<Id, RwLock<Vec<Message>>>,
    /// Members per guild, by user pubkey.
    pub members: DashMap<Id, DashMap<String, Member>>,
    /// Every user we've ever seen identify, by pubkey. Lets us resolve a
    /// display name for a DM partner who may not share a guild with us.
    pub users: DashMap<String, User>,
    /// Public profiles (avatar/bio) by pubkey. Owned by the client, uploaded
    /// on connect; cached here so we can hand them to everyone.
    pub profiles: DashMap<String, Profile>,
    /// DM channels by channel id.
    pub dms: DashMap<Id, DmChannel>,
    /// Index from a sorted "pubkeyA|pubkeyB" pair to its DM channel id.
    pub dm_index: DashMap<String, Id>,
    /// Voice state per user pubkey (global; a user can only be in one voice
    /// channel at a time across all guilds, same as Discord).
    pub voice_states: DashMap<String, VoiceState>,
    /// Pubkeys currently screen-sharing, per channel.
    pub screen_shares: DashMap<Id, std::collections::HashSet<String>>,
    /// Installed bots, indexed by bot pubkey then guild. A pubkey present here
    /// with ≥1 guild is treated as an installed application ("bot") when it
    /// connects: its gateway connection is intent-filtered and its actions are
    /// permission-gated per the `BotInstall` grants. Owner grants the install.
    pub bot_installs: DashMap<String, DashMap<Id, BotInstall>>,
    pub hub: broadcast::Sender<Envelope>,
}

impl AppState {
    pub fn seeded() -> Self {
        let (hub, _) = broadcast::channel(BROADCAST_CAPACITY);
        let state = Self {
            guilds: DashMap::new(),
            channels: DashMap::new(),
            channels_by_guild: DashMap::new(),
            messages: DashMap::new(),
            members: DashMap::new(),
            users: DashMap::new(),
            profiles: DashMap::new(),
            screen_shares: DashMap::new(),
            bot_installs: DashMap::new(),
            dms: DashMap::new(),
            dm_index: DashMap::new(),
            voice_states: DashMap::new(),
            hub,
        };
        seed::populate(&state);
        state
    }

    /// Broadcast a message to every connected client.
    pub fn broadcast(&self, msg: ServerMessage) {
        let _ = self.hub.send(Envelope { msg, to: None });
    }

    /// Deliver a message only to the connections whose user is in `to`.
    pub fn deliver(&self, to: Vec<String>, msg: ServerMessage) {
        let _ = self.hub.send(Envelope { msg, to: Some(to) });
    }

    /// Record (or refresh) a user in the global directory.
    pub fn remember_user(&self, user: &User) {
        self.users.insert(user.pubkey.clone(), user.clone());
    }

    /// Store/overwrite a user's profile and return it.
    pub fn set_profile(
        &self,
        pubkey: &str,
        avatar: Option<String>,
        banner: Option<String>,
        bio: Option<String>,
        status: Option<String>,
        custom_status: Option<String>,
    ) -> Profile {
        let profile = Profile {
            pubkey: pubkey.to_string(),
            avatar,
            banner,
            bio,
            status,
            custom_status,
        };
        self.profiles.insert(pubkey.to_string(), profile.clone());
        profile
    }

    /// Toggle a user's emoji reaction on a message. Returns the message's full
    /// reaction set afterwards (and whether the message was found).
    pub fn toggle_reaction(
        &self,
        channel_id: Id,
        message_id: Id,
        emoji: &str,
        pubkey: &str,
    ) -> Option<Vec<crate::protocol::Reaction>> {
        let entry = self.messages.get(&channel_id)?;
        let mut guard = entry.write().unwrap();
        let msg = guard.iter_mut().find(|m| m.id == message_id)?;
        if let Some(reaction) = msg.reactions.iter_mut().find(|r| r.emoji == emoji) {
            if let Some(pos) = reaction.users.iter().position(|u| u == pubkey) {
                reaction.users.remove(pos);
            } else {
                reaction.users.push(pubkey.to_string());
            }
        } else {
            msg.reactions.push(crate::protocol::Reaction {
                emoji: emoji.to_string(),
                users: vec![pubkey.to_string()],
            });
        }
        // Drop any emoji that no longer has users.
        msg.reactions.retain(|r| !r.users.is_empty());
        Some(msg.reactions.clone())
    }

    /// Owner-only: set/clear a guild's accent. Returns the updated guild on
    /// success, or `Err` if the guild is unknown or the user isn't the owner.
    pub fn set_guild_accent(
        &self,
        guild_id: Id,
        accent: Option<String>,
        by_pubkey: &str,
    ) -> Result<Guild, String> {
        let mut guild = self
            .guilds
            .get_mut(&guild_id)
            .ok_or_else(|| "unknown guild".to_string())?;
        if guild.owner_pubkey.is_empty() || guild.owner_pubkey != by_pubkey {
            return Err("only the owner can restyle this guild".into());
        }
        guild.accent = accent;
        Ok(guild.clone())
    }

    /// Mark a user sharing (or not) in a channel; returns the channel's current
    /// sorted sharer list.
    pub fn set_screen_share(&self, channel_id: Id, pubkey: &str, sharing: bool) -> Vec<String> {
        let mut set = self.screen_shares.entry(channel_id).or_default();
        if sharing {
            set.insert(pubkey.to_string());
        } else {
            set.remove(pubkey);
        }
        let mut list: Vec<String> = set.iter().cloned().collect();
        list.sort();
        list
    }

    /// Remove a user from every screen-share set (on leave/disconnect).
    /// Returns the affected `(channel, new sorted list)` pairs to broadcast.
    pub fn clear_user_screen_shares(&self, pubkey: &str) -> Vec<(Id, Vec<String>)> {
        let mut affected = Vec::new();
        for mut entry in self.screen_shares.iter_mut() {
            if entry.value_mut().remove(pubkey) {
                let cid = *entry.key();
                let mut list: Vec<String> = entry.value().iter().cloned().collect();
                list.sort();
                affected.push((cid, list));
            }
        }
        affected
    }

    /// Snapshot of all known profiles.
    pub fn profiles_snapshot(&self) -> Vec<Profile> {
        self.profiles.iter().map(|p| p.value().clone()).collect()
    }

    /// Resolve a display `User` for a pubkey, falling back to a truncated
    /// pubkey label if we've never seen them identify.
    fn resolve_user(&self, pubkey: &str) -> User {
        self.users
            .get(pubkey)
            .map(|u| u.clone())
            .unwrap_or_else(|| User {
                pubkey: pubkey.to_string(),
                username: pubkey.chars().take(6).collect(),
            })
    }

    /// Create a new guild seeded with a default text + voice channel and make
    /// `creator` its first (online) member. Returns the guild, its channels,
    /// and the creator's membership so the caller can broadcast them.
    pub fn create_guild(&self, name: &str, creator: &User) -> (Guild, Vec<Channel>, Member) {
        let guild = Guild {
            id: Uuid::new_v4(),
            name: name.to_string(),
            icon: Some(guild_initials(name)),
            owner_pubkey: creator.pubkey.clone(),
            accent: None,
        };
        let general = Channel {
            id: Uuid::new_v4(),
            guild_id: guild.id,
            name: "general".into(),
            kind: crate::protocol::ChannelKind::Text,
            topic: None,
        };
        let voice = Channel {
            id: Uuid::new_v4(),
            guild_id: guild.id,
            name: "General Voice".into(),
            kind: crate::protocol::ChannelKind::Voice,
            topic: None,
        };

        self.guilds.insert(guild.id, guild.clone());
        self.channels_by_guild
            .insert(guild.id, vec![general.id, voice.id]);
        for ch in [&general, &voice] {
            self.channels.insert(ch.id, ch.clone());
            self.messages.insert(ch.id, RwLock::new(Vec::new()));
        }

        let member = Member {
            user: creator.clone(),
            guild_id: guild.id,
            online: true,
            bot: false,
        };
        self.members
            .entry(guild.id)
            .or_insert_with(DashMap::new)
            .insert(creator.pubkey.clone(), member.clone());

        (guild, vec![general, voice], member)
    }

    pub fn snapshot_for(&self, user: &User) -> ServerMessage {
        // Everyone belongs to the shared system guild(s) (empty owner, e.g. the
        // seeded Lobby) — a landing space so a fresh user isn't staring at an
        // empty rail. All other guilds are private and joined explicitly.
        let system_guilds: Vec<Id> = self
            .guilds
            .iter()
            .filter(|g| g.owner_pubkey.is_empty())
            .map(|g| g.id)
            .collect();
        for gid in system_guilds {
            self.add_member(gid, user);
        }

        // Mark the user online in every guild they belong to, and collect the
        // ids of those guilds — the snapshot is scoped to exactly these.
        let my_guild_ids: Vec<Id> = self
            .members
            .iter()
            .filter(|e| e.value().contains_key(&user.pubkey))
            .map(|e| *e.key())
            .collect();
        for gid in &my_guild_ids {
            if let Some(guild_members) = self.members.get(gid) {
                if let Some(mut m) = guild_members.get_mut(&user.pubkey) {
                    m.online = true;
                }
            }
        }

        let guilds: Vec<Guild> = my_guild_ids
            .iter()
            .filter_map(|id| self.guilds.get(id).map(|g| g.clone()))
            .collect();
        let channels: Vec<Channel> = self
            .channels
            .iter()
            .filter(|c| my_guild_ids.contains(&c.guild_id))
            .map(|c| c.clone())
            .collect();
        let mut members: Vec<Member> = Vec::new();
        for gid in &my_guild_ids {
            if let Some(guild_members) = self.members.get(gid) {
                for m in guild_members.iter() {
                    members.push(m.value().clone());
                }
            }
        }
        // Voice state is only visible within a guild you're in.
        let voice_states: Vec<VoiceState> = self
            .voice_states
            .iter()
            .filter(|v| my_guild_ids.contains(&v.guild_id))
            .map(|v| v.value().clone())
            .collect();

        let dms = self.dms_for(&user.pubkey);
        let catalog = self.guild_catalog();
        let profiles = self.profiles_snapshot();

        ServerMessage::Ready {
            user: user.clone(),
            guilds,
            channels,
            members,
            voice_states,
            dms,
            catalog,
            profiles,
        }
    }

    /// Insert `user` as an online member of `guild_id` if not already present;
    /// otherwise just flip them online. Returns the resulting membership.
    pub fn add_member(&self, guild_id: Id, user: &User) -> Member {
        let guild_members = self.members.entry(guild_id).or_insert_with(DashMap::new);
        if let Some(mut existing) = guild_members.get_mut(&user.pubkey) {
            existing.online = true;
            return existing.clone();
        }
        let member = Member {
            user: user.clone(),
            guild_id,
            online: true,
            bot: false,
        };
        guild_members.insert(user.pubkey.clone(), member.clone());
        member
    }

    /// Pubkeys of every member of a guild (for member-scoped delivery).
    pub fn guild_member_pubkeys(&self, guild_id: Id) -> Vec<String> {
        self.members
            .get(&guild_id)
            .map(|m| m.iter().map(|e| e.key().clone()).collect())
            .unwrap_or_default()
    }

    pub fn is_guild_member(&self, guild_id: Id, pubkey: &str) -> bool {
        self.members
            .get(&guild_id)
            .map(|m| m.contains_key(pubkey))
            .unwrap_or(false)
    }

    /// The guild a channel belongs to, if it's a known guild channel.
    pub fn channel_guild(&self, channel_id: Id) -> Option<Id> {
        self.channels.get(&channel_id).map(|c| c.guild_id)
    }

    /// Snapshot of all guilds for the public directory.
    pub fn guild_catalog(&self) -> Vec<crate::protocol::GuildSummary> {
        self.guilds
            .iter()
            .map(|g| crate::protocol::GuildSummary {
                id: g.id,
                name: g.name.clone(),
                icon: g.icon.clone(),
                member_count: self
                    .members
                    .get(&g.id)
                    .map(|m| m.len() as u32)
                    .unwrap_or(0),
            })
            .collect()
    }

    /// Add `user` to an existing guild. Returns the guild, its channels, and
    /// its full member list (after the join) so the joiner's client can render
    /// it. `None` if the guild doesn't exist.
    pub fn join_guild(&self, guild_id: Id, user: &User) -> Option<(Guild, Vec<Channel>, Vec<Member>)> {
        let guild = self.guilds.get(&guild_id).map(|g| g.clone())?;
        self.add_member(guild_id, user);
        let channels: Vec<Channel> = self
            .channels
            .iter()
            .filter(|c| c.guild_id == guild_id)
            .map(|c| c.clone())
            .collect();
        let members: Vec<Member> = self
            .members
            .get(&guild_id)
            .map(|m| m.iter().map(|e| e.value().clone()).collect())
            .unwrap_or_default();
        Some((guild, channels, members))
    }

    /// The DM conversations `pubkey` participates in, each described from their
    /// point of view (so `other` is the partner).
    pub fn dms_for(&self, pubkey: &str) -> Vec<DmInfo> {
        self.dms
            .iter()
            .filter_map(|entry| {
                let dm = entry.value();
                let other = if dm.participants[0] == pubkey {
                    &dm.participants[1]
                } else if dm.participants[1] == pubkey {
                    &dm.participants[0]
                } else {
                    return None;
                };
                Some(DmInfo {
                    channel_id: dm.id,
                    other: self.resolve_user(other),
                })
            })
            .collect()
    }

    /// Sorted pair key for the DM index, so (a,b) and (b,a) collide.
    fn dm_key(a: &str, b: &str) -> String {
        if a <= b {
            format!("{a}|{b}")
        } else {
            format!("{b}|{a}")
        }
    }

    /// Get the existing DM channel between two users, creating it (and its
    /// message log) if absent. Returns the channel id.
    pub fn get_or_create_dm(&self, a: &str, b: &str) -> Id {
        let key = Self::dm_key(a, b);
        if let Some(existing) = self.dm_index.get(&key) {
            return *existing;
        }
        let id = Uuid::new_v4();
        let mut participants = [a.to_string(), b.to_string()];
        participants.sort();
        self.dms.insert(id, DmChannel { id, participants });
        self.dm_index.insert(key, id);
        self.messages.insert(id, RwLock::new(Vec::new()));
        id
    }

    pub fn dm_participants(&self, channel_id: Id) -> Option<[String; 2]> {
        self.dms.get(&channel_id).map(|d| d.participants.clone())
    }

    /// True if `channel_id` is a DM that `pubkey` is part of.
    pub fn is_dm_participant(&self, channel_id: Id, pubkey: &str) -> bool {
        self.dms
            .get(&channel_id)
            .map(|d| d.participants.iter().any(|p| p == pubkey))
            .unwrap_or(false)
    }

    /// Delete a guild and everything under it. Returns `Err` with a reason if
    /// the guild is unknown or `by_pubkey` is not its owner.
    pub fn delete_guild(&self, guild_id: Id, by_pubkey: &str) -> Result<(), String> {
        let owner = self
            .guilds
            .get(&guild_id)
            .map(|g| g.owner_pubkey.clone())
            .ok_or_else(|| "unknown guild".to_string())?;
        if owner.is_empty() || owner != by_pubkey {
            return Err("only the owner can delete this guild".into());
        }
        self.guilds.remove(&guild_id);
        self.members.remove(&guild_id);
        if let Some((_, channel_ids)) = self.channels_by_guild.remove(&guild_id) {
            for cid in channel_ids {
                self.channels.remove(&cid);
                self.messages.remove(&cid);
            }
        }
        Ok(())
    }

    pub fn history(&self, channel_id: Id, limit: u32) -> Vec<Message> {
        let Some(entry) = self.messages.get(&channel_id) else {
            return Vec::new();
        };
        let guard = entry.read().unwrap();
        let limit = limit as usize;
        if guard.len() <= limit {
            guard.clone()
        } else {
            guard[guard.len() - limit..].to_vec()
        }
    }

    /// Append a message to a guild text channel. Returns `None` if the channel
    /// isn't a known text channel.
    pub fn push_message(
        &self,
        channel_id: Id,
        author: User,
        content: String,
        image: Option<String>,
    ) -> Option<Message> {
        let kind_ok = self
            .channels
            .get(&channel_id)
            .map(|c| matches!(c.kind, crate::protocol::ChannelKind::Text))
            .unwrap_or(false);
        if !kind_ok {
            return None;
        }
        Some(self.append_message(channel_id, author, content, image))
    }

    /// Append a message to a DM channel `author` participates in. Returns
    /// `None` if the channel isn't a DM the author belongs to.
    pub fn push_dm_message(
        &self,
        channel_id: Id,
        author: User,
        content: String,
        image: Option<String>,
    ) -> Option<Message> {
        if !self.is_dm_participant(channel_id, &author.pubkey) {
            return None;
        }
        Some(self.append_message(channel_id, author, content, image))
    }

    fn append_message(
        &self,
        channel_id: Id,
        author: User,
        content: String,
        image: Option<String>,
    ) -> Message {
        let message = Message {
            id: Uuid::new_v4(),
            channel_id,
            author,
            content,
            image,
            reactions: Vec::new(),
            created_at: chrono::Utc::now(),
        };
        self.messages
            .entry(channel_id)
            .or_insert_with(|| RwLock::new(Vec::new()))
            .write()
            .unwrap()
            .push(message.clone());
        message
    }

    /// Mark a user offline in every guild they're a member of. Returns the
    /// guild_id + pubkey pairs that were actually flipped (so callers know
    /// which broadcasts to send).
    pub fn mark_offline(&self, user_pubkey: &str) -> Vec<(Id, String)> {
        let mut affected = Vec::new();
        for entry in self.members.iter() {
            let guild_id = *entry.key();
            if let Some(mut m) = entry.value().get_mut(user_pubkey) {
                if m.online {
                    m.online = false;
                    affected.push((guild_id, user_pubkey.to_string()));
                }
            }
        }
        affected
    }

    pub fn voice_channel_guild(&self, channel_id: Id) -> Option<Id> {
        self.channels.get(&channel_id).and_then(|c| {
            matches!(c.kind, crate::protocol::ChannelKind::Voice).then_some(c.guild_id)
        })
    }

    pub fn set_voice_channel(
        &self,
        user_pubkey: &str,
        guild_id: Id,
        channel_id: Option<Id>,
    ) -> VoiceState {
        let prev = self.voice_states.get(user_pubkey).map(|v| v.clone());
        let state = VoiceState {
            user_pubkey: user_pubkey.to_string(),
            guild_id,
            channel_id,
            muted: prev.as_ref().map(|p| p.muted).unwrap_or(false),
            deafened: prev.as_ref().map(|p| p.deafened).unwrap_or(false),
            speaking: false,
        };
        if channel_id.is_some() {
            self.voice_states.insert(user_pubkey.to_string(), state.clone());
        } else {
            self.voice_states.remove(user_pubkey);
        }
        state
    }

    pub fn update_voice_flags(
        &self,
        user_pubkey: &str,
        muted: bool,
        deafened: bool,
    ) -> Option<VoiceState> {
        let mut entry = self.voice_states.get_mut(user_pubkey)?;
        entry.muted = muted;
        entry.deafened = deafened || muted;
        Some(entry.clone())
    }

    pub fn update_speaking(&self, user_pubkey: &str, speaking: bool) -> Option<VoiceState> {
        let mut entry = self.voice_states.get_mut(user_pubkey)?;
        if entry.speaking == speaking {
            return None;
        }
        entry.speaking = speaking;
        Some(entry.clone())
    }

    /// Tombstone voice state for a disconnecting user; returns the cleared state if any.
    pub fn clear_voice(&self, user_pubkey: &str) -> Option<VoiceState> {
        let prev = self.voice_states.remove(user_pubkey)?.1;
        Some(VoiceState {
            channel_id: None,
            speaking: false,
            ..prev
        })
    }

    // ----- Bot platform (Tier 1) -------------------------------------------

    /// True if `pubkey` is an installed bot in at least one guild.
    pub fn is_bot(&self, pubkey: &str) -> bool {
        self.bot_installs
            .get(pubkey)
            .map(|g| !g.is_empty())
            .unwrap_or(false)
    }

    /// The grants a bot has in a specific guild, if installed there.
    pub fn bot_install(&self, guild_id: Id, bot_pubkey: &str) -> Option<BotInstall> {
        self.bot_installs
            .get(bot_pubkey)
            .and_then(|g| g.get(&guild_id).map(|i| i.clone()))
    }

    /// Every guild a bot is installed in, with its grants. Used to scope a bot
    /// connection's `Ready` and its inbound event stream.
    pub fn bot_guilds(&self, bot_pubkey: &str) -> Vec<BotInstall> {
        self.bot_installs
            .get(bot_pubkey)
            .map(|g| g.iter().map(|e| e.value().clone()).collect())
            .unwrap_or_default()
    }

    /// All bot installs in a guild (for the owner's Integrations panel).
    pub fn guild_installs(&self, guild_id: Id) -> Vec<BotInstall> {
        self.bot_installs
            .iter()
            .filter_map(|e| e.value().get(&guild_id).map(|i| i.clone()))
            .collect()
    }

    /// Owner-only: install (or update the grants of) a bot in a guild. Adds the
    /// bot as a `bot: true` guild member so it shows in the roster. Returns the
    /// stored install and the bot's membership for broadcasting.
    pub fn install_bot(
        &self,
        guild_id: Id,
        bot_pubkey: &str,
        name: &str,
        permissions: Vec<Permission>,
        intents: Vec<Intent>,
        by_pubkey: &str,
    ) -> Result<(BotInstall, Member), String> {
        let owner = self
            .guilds
            .get(&guild_id)
            .map(|g| g.owner_pubkey.clone())
            .ok_or_else(|| "unknown guild".to_string())?;
        if owner.is_empty() || owner != by_pubkey {
            return Err("only the owner can manage this guild's integrations".into());
        }
        if bot_pubkey.trim().is_empty() {
            return Err("bot pubkey is required".into());
        }
        if bot_pubkey == by_pubkey {
            return Err("a bot must have its own identity, distinct from yours".into());
        }
        let name = {
            let n = name.trim();
            if n.is_empty() {
                "Bot".to_string()
            } else {
                n.chars().take(32).collect()
            }
        };
        let install = BotInstall {
            guild_id,
            bot_pubkey: bot_pubkey.to_string(),
            name: name.clone(),
            permissions: unique(permissions),
            intents: unique(intents),
        };
        self.bot_installs
            .entry(bot_pubkey.to_string())
            .or_default()
            .insert(guild_id, install.clone());

        // Surface the bot in the roster. Keep an already-connected bot's online
        // flag; a fresh install starts offline until the bot process connects.
        let guild_members = self.members.entry(guild_id).or_insert_with(DashMap::new);
        let member = if let Some(mut existing) = guild_members.get_mut(bot_pubkey) {
            existing.bot = true;
            existing.user.username = name;
            existing.clone()
        } else {
            let member = Member {
                user: User {
                    pubkey: bot_pubkey.to_string(),
                    username: name,
                },
                guild_id,
                online: false,
                bot: true,
            };
            guild_members.insert(bot_pubkey.to_string(), member.clone());
            member
        };
        Ok((install, member))
    }

    /// Owner-only: remove a bot from a guild (drops its grants and roster row).
    pub fn uninstall_bot(
        &self,
        guild_id: Id,
        bot_pubkey: &str,
        by_pubkey: &str,
    ) -> Result<(), String> {
        let owner = self
            .guilds
            .get(&guild_id)
            .map(|g| g.owner_pubkey.clone())
            .ok_or_else(|| "unknown guild".to_string())?;
        if owner.is_empty() || owner != by_pubkey {
            return Err("only the owner can manage this guild's integrations".into());
        }
        // Drop the per-guild grant; if it was the bot's last install, forget the
        // bot entirely so `is_bot` flips false.
        let now_empty = if let Some(g) = self.bot_installs.get(bot_pubkey) {
            g.remove(&guild_id);
            g.is_empty()
        } else {
            false
        };
        if now_empty {
            self.bot_installs.remove(bot_pubkey);
        }
        if let Some(gm) = self.members.get(&guild_id) {
            gm.remove(bot_pubkey);
        }
        Ok(())
    }

    /// Scoped `Ready` for a bot connection: only the guilds it's installed in,
    /// their channels, and — gated behind the `Members` intent — their rosters.
    /// No DMs, no profiles, no public catalog (a bot doesn't browse).
    pub fn snapshot_for_bot(&self, bot: &User) -> ServerMessage {
        let installs = self.bot_guilds(&bot.pubkey);
        let my_guild_ids: Vec<Id> = installs.iter().map(|i| i.guild_id).collect();

        // Flip the bot online in each installed guild.
        for gid in &my_guild_ids {
            if let Some(gm) = self.members.get(gid) {
                if let Some(mut m) = gm.get_mut(&bot.pubkey) {
                    m.online = true;
                }
            }
        }

        let guilds: Vec<Guild> = my_guild_ids
            .iter()
            .filter_map(|id| self.guilds.get(id).map(|g| g.clone()))
            .collect();
        let channels: Vec<Channel> = self
            .channels
            .iter()
            .filter(|c| my_guild_ids.contains(&c.guild_id))
            .map(|c| c.clone())
            .collect();
        // Roster is sensitive: only hand it over for guilds the bot was granted
        // the Members intent in.
        let mut members: Vec<Member> = Vec::new();
        for install in &installs {
            if !install.has_intent(Intent::Members) {
                continue;
            }
            if let Some(gm) = self.members.get(&install.guild_id) {
                for m in gm.iter() {
                    members.push(m.value().clone());
                }
            }
        }

        ServerMessage::Ready {
            user: bot.clone(),
            guilds,
            channels,
            members,
            voice_states: Vec::new(),
            dms: Vec::new(),
            catalog: Vec::new(),
            profiles: Vec::new(),
        }
    }
}

/// Order-preserving dedup for small grant vectors.
fn unique<T: PartialEq>(items: Vec<T>) -> Vec<T> {
    let mut out: Vec<T> = Vec::with_capacity(items.len());
    for item in items {
        if !out.contains(&item) {
            out.push(item);
        }
    }
    out
}

/// Derive a short (≤2 char) uppercase icon label from a guild name.
fn guild_initials(name: &str) -> String {
    let initials: String = name
        .split_whitespace()
        .filter_map(|w| w.chars().next())
        .take(2)
        .collect::<String>()
        .to_uppercase();
    if initials.is_empty() {
        "?".into()
    } else {
        initials
    }
}
