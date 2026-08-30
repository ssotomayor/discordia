use std::sync::atomic::{AtomicU64, Ordering};

use dashmap::DashMap;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::media::MediaStore;
use crate::protocol::{
    BotInstall, Channel, Guild, GuildEmoji, GuildVisibility, Id, Intent, MAX_EMOJIS_PER_GUILD,
    Member, Message, Permission, Profile, ReplyRef, Role, ServerMessage, User, VoiceState,
    valid_shortcode,
};
use crate::store::Store;

/// A failed write is logged and ignored: the change survives the session but
/// not a restart, and is never a user-facing error.
fn persist(res: Result<(), sqlx::Error>, what: &str) {
    if let Err(e) = res {
        tracing::error!(error = %e, what, "write-through persist FAILED — state will regress on restart");
    }
}

const CONN_QUEUE_CAP: usize = 256;

pub const MAX_IMAGE_LEN: usize = 3_000_000;

struct Conn {
    tx: mpsc::Sender<ServerMessage>,
}

#[derive(Debug, Clone)]
pub struct Invite {
    pub code: String,
    pub guild_id: Id,
    pub expires_at_ms: Option<i64>,
    pub max_uses: Option<u32>,
    pub uses: u32,
    pub created_by: String,
}

const MEDIA_GRACE: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

impl Invite {
    fn is_live(&self, now_ms: i64) -> bool {
        let unexpired = self.expires_at_ms.is_none_or(|at| now_ms < at);
        let unspent = self.max_uses.is_none_or(|max| self.uses < max);
        unexpired && unspent
    }
}

pub struct AppState {
    pub store: Store,
    pub media: MediaStore,
    pub guilds: DashMap<Id, Guild>,
    pub channels: DashMap<Id, Channel>,
    pub channels_by_guild: DashMap<Id, Vec<Id>>,
    pub members: DashMap<Id, DashMap<String, Member>>,
    pub users: DashMap<String, User>,
    pub profiles: DashMap<String, Profile>,
    pub voice_states: DashMap<String, VoiceState>,
    pub bot_installs: DashMap<String, DashMap<Id, BotInstall>>,
    pub roles: DashMap<Id, Vec<Role>>,
    pub emojis: DashMap<Id, Vec<GuildEmoji>>,
    pub invites: DashMap<String, Invite>,
    pub invite_by_guild: DashMap<Id, String>,
    pub bans: DashMap<Id, std::collections::HashSet<String>>,
    pub last_post: DashMap<(Id, String), std::time::Instant>,
    pub recent_joins: DashMap<Id, Vec<std::time::Instant>>,
    pub xp: DashMap<Id, DashMap<String, u64>>,
    pub operators: std::collections::HashSet<String>,
    conns: DashMap<u64, Conn>,
    conn_ids_by_pubkey: DashMap<String, std::collections::HashSet<u64>>,
    next_conn_id: AtomicU64,
}

impl AppState {
    pub async fn load_or_seed(
        store: Store,
        media: MediaStore,
        operators: std::collections::HashSet<String>,
    ) -> Result<Self, sqlx::Error> {
        let state = Self {
            store,
            media,
            guilds: DashMap::new(),
            channels: DashMap::new(),
            channels_by_guild: DashMap::new(),
            members: DashMap::new(),
            users: DashMap::new(),
            profiles: DashMap::new(),
            bot_installs: DashMap::new(),
            roles: DashMap::new(),
            emojis: DashMap::new(),
            invites: DashMap::new(),
            invite_by_guild: DashMap::new(),
            bans: DashMap::new(),
            last_post: DashMap::new(),
            recent_joins: DashMap::new(),
            xp: DashMap::new(),
            voice_states: DashMap::new(),
            operators,
            conns: DashMap::new(),
            conn_ids_by_pubkey: DashMap::new(),
            next_conn_id: AtomicU64::new(1),
        };

        let loaded = state.store.load_all().await?;
        let fresh = loaded.guilds.is_empty();

        for u in loaded.users {
            state.users.insert(u.pubkey.clone(), u);
        }
        for p in loaded.profiles {
            state.profiles.insert(p.pubkey.clone(), p);
        }
        for (gid, pk, xp) in loaded.guild_xp {
            state.xp.entry(gid).or_default().insert(pk, xp);
        }
        for g in loaded.guilds {
            state.channels_by_guild.entry(g.id).or_default();
            state.guilds.insert(g.id, g);
        }
        for c in loaded.channels {
            state
                .channels_by_guild
                .entry(c.guild_id)
                .or_default()
                .push(c.id);
            state.channels.insert(c.id, c);
        }
        for (gid, pubkey, username, bot, role_ids) in loaded.members {
            state.members.entry(gid).or_default().insert(
                pubkey.clone(),
                Member {
                    user: User { pubkey, username },
                    guild_id: gid,
                    online: false,
                    bot,
                    roles: role_ids,
                    xp: 0,
                },
            );
        }
        for r in loaded.roles {
            state.roles.entry(r.guild_id).or_default().push(r);
        }
        for e in loaded.emojis {
            state.emojis.entry(e.guild_id).or_default().push(e);
        }
        for (gid, pk) in loaded.bans {
            state.bans.entry(gid).or_default().insert(pk);
        }
        for row in loaded.invites {
            let (code, gid) = (row.code.clone(), row.guild_id);
            state.invites.insert(
                code.clone(),
                Invite {
                    code: code.clone(),
                    guild_id: gid,
                    expires_at_ms: row.expires_at_ms,
                    max_uses: row.max_uses,
                    uses: row.uses,
                    created_by: row.created_by,
                },
            );
            state.invite_by_guild.insert(gid, code);
        }
        for i in loaded.bot_installs {
            state
                .bot_installs
                .entry(i.bot_pubkey.clone())
                .or_default()
                .insert(i.guild_id, i);
        }

        if fresh {
            state.seed_lobby().await;
        }
        Ok(state)
    }

    async fn seed_lobby(&self) {
        let lobby = Guild {
            id: Uuid::new_v4(),
            name: "Lobby".into(),
            icon: Some("LB".into()),
            owner_pubkey: String::new(),
            accent: None,
            visibility: GuildVisibility::Public,
            description: None,
            icon_image: None,
            banner: None,
            retention_days: None,
            join_gate: crate::protocol::JoinGate::Open,
            rules: None,
            panic_mode: false,
        };
        let general = Channel {
            id: Uuid::new_v4(),
            guild_id: lobby.id,
            name: "general".into(),
            kind: crate::protocol::ChannelKind::Text,
            topic: None,
            read_only: false,
            slowmode_secs: 0,
            position: 0,
        };
        let voice = Channel {
            id: Uuid::new_v4(),
            guild_id: lobby.id,
            name: "General Voice".into(),
            kind: crate::protocol::ChannelKind::Voice,
            topic: None,
            read_only: false,
            slowmode_secs: 0,
            position: 1,
        };
        persist(self.store.upsert_guild(&lobby).await, "seed guild");
        persist(self.store.upsert_channel(&general).await, "seed channel");
        persist(self.store.upsert_channel(&voice).await, "seed channel");
        self.channels_by_guild
            .insert(lobby.id, vec![general.id, voice.id]);
        for ch in [general, voice] {
            self.channels.insert(ch.id, ch);
        }
        self.guilds.insert(lobby.id, lobby);
    }

    pub fn register_conn(&self) -> (u64, mpsc::Receiver<ServerMessage>) {
        let id = self.next_conn_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel(CONN_QUEUE_CAP);
        self.conns.insert(id, Conn { tx });
        (id, rx)
    }

    pub fn identify_conn(&self, conn_id: u64, pubkey: &str) {
        self.conn_ids_by_pubkey
            .entry(pubkey.to_string())
            .or_default()
            .insert(conn_id);
    }

    pub fn unregister_conn(&self, conn_id: u64, pubkey: Option<&str>) {
        self.conns.remove(&conn_id);
        if let Some(pk) = pubkey {
            let now_empty = if let Some(mut set) = self.conn_ids_by_pubkey.get_mut(pk) {
                set.remove(&conn_id);
                set.is_empty()
            } else {
                false
            };
            if now_empty {
                self.conn_ids_by_pubkey
                    .remove_if(pk, |_, set| set.is_empty());
            }
        }
    }

    fn route(&self, conn_id: u64, msg: &ServerMessage) {
        let tx = match self.conns.get(&conn_id) {
            Some(c) => c.tx.clone(),
            None => return,
        };
        if tx.try_send(msg.clone()).is_err() {
            self.conns.remove(&conn_id);
        }
    }

    pub fn broadcast(&self, msg: ServerMessage) {
        let ids: Vec<u64> = self.conns.iter().map(|e| *e.key()).collect();
        for id in ids {
            self.route(id, &msg);
        }
    }

    pub fn deliver(&self, to: Vec<String>, msg: ServerMessage) {
        let mut ids: Vec<u64> = Vec::new();
        for pk in &to {
            if let Some(set) = self.conn_ids_by_pubkey.get(pk) {
                ids.extend(set.iter().copied());
            }
        }
        ids.sort_unstable();
        ids.dedup();
        for id in ids {
            self.route(id, &msg);
        }
    }

    pub async fn remember_user(&self, user: &User) {
        self.users.insert(user.pubkey.clone(), user.clone());
        persist(self.store.upsert_user(user).await, "user");
    }

    pub async fn set_profile(
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
        persist(self.store.upsert_profile(&profile).await, "profile");
        profile
    }

    pub fn xp_of(&self, guild_id: Id, pubkey: &str) -> u64 {
        self.xp
            .get(&guild_id)
            .and_then(|g| g.get(pubkey).map(|v| *v))
            .unwrap_or(0)
    }

    pub fn stamp_xp(&self, mut member: Member) -> Member {
        member.xp = self.xp_of(member.guild_id, &member.user.pubkey);
        member
    }

    pub async fn add_xp(&self, guild_id: Id, pubkey: &str) -> Option<Member> {
        let new_xp = {
            let guild = self.xp.entry(guild_id).or_default();
            let mut e = guild.entry(pubkey.to_string()).or_insert(0);
            *e += 1;
            *e
        };
        persist(
            self.store.upsert_guild_xp(guild_id, pubkey, new_xp).await,
            "xp",
        );
        if crate::protocol::level_progress(new_xp).0
            == crate::protocol::level_progress(new_xp - 1).0
        {
            return None;
        }
        let member = self
            .members
            .get(&guild_id)
            .and_then(|gm| gm.get(pubkey).map(|m| m.clone()))?;
        Some(self.stamp_xp(member))
    }

    pub async fn toggle_reaction(
        &self,
        channel_id: Id,
        message_id: Id,
        emoji: &str,
        pubkey: &str,
    ) -> Option<Vec<crate::protocol::Reaction>> {
        match self
            .store
            .toggle_reaction(channel_id, message_id, emoji, pubkey)
            .await
        {
            Ok(res) => res,
            Err(e) => {
                tracing::error!(error = %e, "toggle_reaction store error");
                None
            }
        }
    }

    pub fn is_owner(&self, guild_id: Id, pubkey: &str) -> bool {
        match self.guilds.get(&guild_id) {
            Some(g) if !g.owner_pubkey.is_empty() => g.owner_pubkey == pubkey,
            Some(_) => self.operators.contains(pubkey),
            None => false,
        }
    }

    pub fn effective_permissions(
        &self,
        guild_id: Id,
        pubkey: &str,
    ) -> std::collections::HashSet<Permission> {
        if self.is_owner(guild_id, pubkey) {
            return Permission::ALL.iter().copied().collect();
        }
        let Some(guild_members) = self.members.get(&guild_id) else {
            return Default::default();
        };
        let Some(member) = guild_members.get(pubkey) else {
            return Default::default();
        };
        let assigned = member.roles.clone();
        drop(member);
        drop(guild_members);
        let Some(roles) = self.roles.get(&guild_id) else {
            return Default::default();
        };
        assigned
            .iter()
            .filter_map(|rid| roles.iter().find(|r| r.id == *rid))
            .flat_map(|r| r.permissions.iter().copied())
            .collect()
    }

    pub fn has_permission(&self, guild_id: Id, pubkey: &str, perm: Permission) -> bool {
        self.effective_permissions(guild_id, pubkey).contains(&perm)
    }

    pub fn require_permission(
        &self,
        guild_id: Id,
        pubkey: &str,
        perm: Permission,
    ) -> Result<(), String> {
        if !self.guilds.contains_key(&guild_id) {
            return Err("unknown guild".into());
        }
        if self.has_permission(guild_id, pubkey, perm) {
            Ok(())
        } else {
            Err(format!(
                "you need the {} permission to do that here",
                serde_variant_name(perm)
            ))
        }
    }

    pub async fn set_guild_accent(
        &self,
        guild_id: Id,
        accent: Option<String>,
        by_pubkey: &str,
    ) -> Result<Guild, String> {
        self.require_permission(guild_id, by_pubkey, Permission::ManageGuild)?;
        let updated = {
            let mut guild = self
                .guilds
                .get_mut(&guild_id)
                .ok_or_else(|| "unknown guild".to_string())?;
            guild.accent = accent;
            guild.clone()
        };
        persist(self.store.upsert_guild(&updated).await, "guild accent");
        Ok(updated)
    }

    pub fn update_screen_share(&self, user_pubkey: &str, sharing: bool) -> Option<VoiceState> {
        let mut entry = self.voice_states.get_mut(user_pubkey)?;
        if entry.screen_sharing == sharing {
            return None;
        }
        entry.screen_sharing = sharing;
        Some(entry.clone())
    }

    pub fn screen_sharers_in(&self, channel_id: Id) -> Vec<String> {
        let mut list: Vec<String> = self
            .voice_states
            .iter()
            .filter(|v| v.screen_sharing && v.channel_id == Some(channel_id))
            .map(|v| v.user_pubkey.clone())
            .collect();
        list.sort();
        list
    }

    pub fn voice_states_in(&self, guild_id: Id) -> Vec<VoiceState> {
        self.voice_states
            .iter()
            .filter(|v| v.guild_id == guild_id)
            .map(|v| v.value().clone())
            .collect()
    }

    pub fn profiles_snapshot(&self) -> Vec<Profile> {
        self.profiles.iter().map(|p| p.value().clone()).collect()
    }

    fn resolve_user(&self, pubkey: &str) -> User {
        self.users
            .get(pubkey)
            .map(|u| u.clone())
            .unwrap_or_else(|| User {
                pubkey: pubkey.to_string(),
                username: pubkey.chars().take(6).collect(),
            })
    }

    pub async fn create_guild(
        &self,
        name: &str,
        template: Option<&str>,
        creator: &User,
    ) -> (Guild, Vec<Channel>, Member, Vec<Role>) {
        let spec = GuildTemplate::resolve(template);
        let gid = Uuid::new_v4();
        let guild = Guild {
            id: gid,
            name: name.to_string(),
            icon: Some(guild_initials(name)),
            owner_pubkey: creator.pubkey.clone(),
            accent: None,
            visibility: spec.visibility,
            description: None,
            icon_image: None,
            banner: None,
            retention_days: None,
            join_gate: spec.join_gate,
            rules: None,
            panic_mode: false,
        };

        let mut channels = Vec::new();
        for (pos, (cname, kind, read_only)) in spec.channels.iter().enumerate() {
            channels.push(Channel {
                id: Uuid::new_v4(),
                guild_id: gid,
                name: (*cname).into(),
                kind: *kind,
                topic: None,
                read_only: *read_only,
                slowmode_secs: 0,
                position: pos as u32,
            });
        }
        let mut roles = Vec::new();
        for (pos, (rname, perms)) in spec.roles.iter().enumerate() {
            roles.push(Role {
                id: Uuid::new_v4(),
                guild_id: gid,
                name: (*rname).into(),
                color: None,
                permissions: perms.to_vec(),
                position: pos as u32,
            });
        }

        self.guilds.insert(gid, guild.clone());
        self.channels_by_guild
            .insert(gid, channels.iter().map(|c| c.id).collect());
        for ch in &channels {
            self.channels.insert(ch.id, ch.clone());
        }
        if !roles.is_empty() {
            self.roles.insert(gid, roles.clone());
        }

        let member = Member {
            user: creator.clone(),
            guild_id: gid,
            online: true,
            bot: false,
            roles: Vec::new(),
            xp: 0,
        };
        self.members
            .entry(gid)
            .or_default()
            .insert(creator.pubkey.clone(), member.clone());

        persist(self.store.upsert_guild(&guild).await, "guild create");
        for ch in &channels {
            persist(self.store.upsert_channel(ch).await, "channel create");
        }
        for r in &roles {
            persist(self.store.upsert_role(r).await, "role create");
        }
        persist(self.store.upsert_member(&member).await, "member create");

        (guild, channels, member, roles)
    }

    pub async fn snapshot_for(&self, user: &User) -> ServerMessage {
        let system_guilds: Vec<Id> = self
            .guilds
            .iter()
            .filter(|g| g.owner_pubkey.is_empty())
            .map(|g| g.id)
            .collect();
        for gid in system_guilds {
            self.add_member(gid, user).await;
        }

        let my_guild_ids: Vec<Id> = self
            .members
            .iter()
            .filter(|e| e.value().contains_key(&user.pubkey))
            .map(|e| *e.key())
            .collect();
        for gid in &my_guild_ids {
            if let Some(guild_members) = self.members.get(gid)
                && let Some(mut m) = guild_members.get_mut(&user.pubkey)
            {
                m.online = true;
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
                    members.push(self.stamp_xp(m.value().clone()));
                }
            }
        }
        let voice_states: Vec<VoiceState> = self
            .voice_states
            .iter()
            .filter(|v| my_guild_ids.contains(&v.guild_id))
            .map(|v| v.value().clone())
            .collect();

        let catalog = self.guild_catalog();
        let profiles = self.profiles_snapshot();
        let roles = self.roles_for_guilds(&my_guild_ids);
        let emojis = self.emojis_for_guilds(&my_guild_ids);

        ServerMessage::Ready {
            user: user.clone(),
            guilds,
            channels,
            members,
            voice_states,
            catalog,
            profiles,
            roles,
            emojis,
            operator: self.operators.contains(&user.pubkey),
        }
    }

    pub fn roles_for_guilds(&self, guild_ids: &[Id]) -> Vec<crate::protocol::Role> {
        guild_ids
            .iter()
            .flat_map(|gid| self.guild_roles(*gid))
            .collect()
    }

    pub fn guild_roles(&self, guild_id: Id) -> Vec<crate::protocol::Role> {
        self.roles
            .get(&guild_id)
            .map(|r| r.clone())
            .unwrap_or_default()
    }

    const MAX_ROLES_PER_GUILD: usize = 50;

    fn authorize_role_touch(
        &self,
        guild_id: Id,
        by_pubkey: &str,
        role_perms: &[&[Permission]],
    ) -> Result<(), String> {
        self.require_permission(guild_id, by_pubkey, Permission::ManageRoles)?;
        if self.is_owner(guild_id, by_pubkey) {
            return Ok(());
        }
        let mine = self.effective_permissions(guild_id, by_pubkey);
        for perms in role_perms {
            if perms
                .iter()
                .any(|p| matches!(p, Permission::ManageRoles | Permission::ManageGuild))
            {
                return Err("roles carrying manage_roles or manage_guild are owner-only".into());
            }
            if let Some(missing) = perms.iter().find(|p| !mine.contains(p)) {
                return Err(format!(
                    "you can't grant {} — you don't hold it yourself",
                    serde_variant_name(*missing)
                ));
            }
        }
        Ok(())
    }

    fn sanitize_role(
        name: &str,
        color: Option<String>,
    ) -> Result<(String, Option<String>), String> {
        let name = crate::protocol::sanitize_name("role", name, 32)?;
        let color = color.filter(|c| is_hex_color(c));
        Ok((name, color))
    }

    pub async fn create_role(
        &self,
        guild_id: Id,
        name: &str,
        color: Option<String>,
        permissions: Vec<Permission>,
        by_pubkey: &str,
    ) -> Result<Role, String> {
        let permissions = unique(permissions);
        self.authorize_role_touch(guild_id, by_pubkey, &[&permissions])?;
        let (name, color) = Self::sanitize_role(name, color)?;
        let role = {
            let mut roles = self.roles.entry(guild_id).or_default();
            if roles.len() >= Self::MAX_ROLES_PER_GUILD {
                return Err("role limit reached for this guild".into());
            }
            let role = Role {
                id: Uuid::new_v4(),
                guild_id,
                name,
                color,
                permissions,
                position: roles.len() as u32,
            };
            roles.push(role.clone());
            role
        };
        persist(self.store.upsert_role(&role).await, "role create");
        Ok(role)
    }

    pub async fn update_role(
        &self,
        guild_id: Id,
        role_id: Id,
        name: &str,
        color: Option<String>,
        permissions: Vec<Permission>,
        by_pubkey: &str,
    ) -> Result<Role, String> {
        let permissions = unique(permissions);
        let current = self
            .roles
            .get(&guild_id)
            .and_then(|r| {
                r.iter()
                    .find(|r| r.id == role_id)
                    .map(|r| r.permissions.clone())
            })
            .ok_or_else(|| "unknown role".to_string())?;
        self.authorize_role_touch(guild_id, by_pubkey, &[&current, &permissions])?;
        let (name, color) = Self::sanitize_role(name, color)?;
        let updated = {
            let mut roles = self.roles.get_mut(&guild_id).ok_or("unknown role")?;
            let role = roles
                .iter_mut()
                .find(|r| r.id == role_id)
                .ok_or("unknown role")?;
            role.name = name;
            role.color = color;
            role.permissions = permissions;
            role.clone()
        };
        persist(self.store.upsert_role(&updated).await, "role update");
        Ok(updated)
    }

    pub fn emojis_of(&self, guild_id: Id) -> Vec<GuildEmoji> {
        self.emojis
            .get(&guild_id)
            .map(|e| e.clone())
            .unwrap_or_default()
    }

    pub fn emojis_for_guilds(&self, guild_ids: &[Id]) -> Vec<GuildEmoji> {
        guild_ids.iter().flat_map(|g| self.emojis_of(*g)).collect()
    }

    pub async fn create_emoji(
        &self,
        guild_id: Id,
        shortcode: &str,
        image: String,
        by_pubkey: &str,
    ) -> Result<GuildEmoji, String> {
        self.require_permission(guild_id, by_pubkey, Permission::ManageEmojis)?;
        let shortcode = shortcode.trim().trim_matches(':').to_ascii_lowercase();
        if !valid_shortcode(&shortcode) {
            return Err("shortcode must be 2-32 chars of a-z, 0-9 or _".into());
        }
        let emoji = {
            let mut list = self.emojis.entry(guild_id).or_default();
            if list.len() >= MAX_EMOJIS_PER_GUILD {
                return Err(format!(
                    "emoji limit reached ({MAX_EMOJIS_PER_GUILD} per guild)"
                ));
            }
            if list.iter().any(|e| e.shortcode == shortcode) {
                return Err(format!(":{shortcode}: already exists in this guild"));
            }
            let emoji = GuildEmoji {
                id: Uuid::new_v4(),
                guild_id,
                shortcode,
                image,
                added_by: by_pubkey.to_string(),
                created_ms: chrono::Utc::now().timestamp_millis(),
            };
            list.push(emoji.clone());
            emoji
        };
        persist(self.store.upsert_emoji(&emoji).await, "emoji create");
        Ok(emoji)
    }

    pub async fn rename_emoji(
        &self,
        guild_id: Id,
        emoji_id: Id,
        shortcode: &str,
        by_pubkey: &str,
    ) -> Result<GuildEmoji, String> {
        self.require_permission(guild_id, by_pubkey, Permission::ManageEmojis)?;
        let shortcode = shortcode.trim().trim_matches(':').to_ascii_lowercase();
        if !valid_shortcode(&shortcode) {
            return Err("shortcode must be 2-32 chars of a-z, 0-9 or _".into());
        }
        let updated = {
            let mut list = self.emojis.get_mut(&guild_id).ok_or("unknown emoji")?;
            if list
                .iter()
                .any(|e| e.shortcode == shortcode && e.id != emoji_id)
            {
                return Err(format!(":{shortcode}: already exists in this guild"));
            }
            let e = list
                .iter_mut()
                .find(|e| e.id == emoji_id)
                .ok_or("unknown emoji")?;
            e.shortcode = shortcode;
            e.clone()
        };
        persist(self.store.upsert_emoji(&updated).await, "emoji rename");
        Ok(updated)
    }

    pub async fn delete_emoji(
        &self,
        guild_id: Id,
        emoji_id: Id,
        by_pubkey: &str,
    ) -> Result<(), String> {
        self.require_permission(guild_id, by_pubkey, Permission::ManageEmojis)?;
        let existed = {
            let mut list = self.emojis.get_mut(&guild_id).ok_or("unknown emoji")?;
            let before = list.len();
            list.retain(|e| e.id != emoji_id);
            before != list.len()
        };
        if !existed {
            return Err("unknown emoji".into());
        }
        persist(self.store.delete_emoji(emoji_id).await, "emoji delete");
        Ok(())
    }

    pub async fn delete_role(
        &self,
        guild_id: Id,
        role_id: Id,
        by_pubkey: &str,
    ) -> Result<Vec<Member>, String> {
        let current = self
            .roles
            .get(&guild_id)
            .and_then(|r| {
                r.iter()
                    .find(|r| r.id == role_id)
                    .map(|r| r.permissions.clone())
            })
            .ok_or_else(|| "unknown role".to_string())?;
        self.authorize_role_touch(guild_id, by_pubkey, &[&current])?;
        if let Some(mut roles) = self.roles.get_mut(&guild_id) {
            roles.retain(|r| r.id != role_id);
        }
        let mut changed = Vec::new();
        if let Some(guild_members) = self.members.get(&guild_id) {
            for mut m in guild_members.iter_mut() {
                if m.roles.contains(&role_id) {
                    m.roles.retain(|r| *r != role_id);
                    changed.push(self.stamp_xp(m.clone()));
                }
            }
        }
        persist(self.store.delete_role(role_id).await, "role delete");
        for m in &changed {
            persist(self.store.upsert_member(m).await, "member role strip");
        }
        Ok(changed)
    }

    pub async fn set_member_role(
        &self,
        guild_id: Id,
        role_id: Id,
        target_pubkey: &str,
        assign: bool,
        by_pubkey: &str,
    ) -> Result<Member, String> {
        let role_perms = self
            .roles
            .get(&guild_id)
            .and_then(|r| {
                r.iter()
                    .find(|r| r.id == role_id)
                    .map(|r| r.permissions.clone())
            })
            .ok_or_else(|| "unknown role".to_string())?;
        self.authorize_role_touch(guild_id, by_pubkey, &[&role_perms])?;
        let updated = {
            let guild_members = self
                .members
                .get(&guild_id)
                .ok_or_else(|| "unknown guild".to_string())?;
            let mut member = guild_members
                .get_mut(target_pubkey)
                .ok_or_else(|| "that user isn't a member of this guild".to_string())?;
            if member.bot {
                return Err("roles don't apply to bots — edit the install's grants instead".into());
            }
            if assign {
                if !member.roles.contains(&role_id) {
                    member.roles.push(role_id);
                }
            } else {
                member.roles.retain(|r| *r != role_id);
            }
            member.clone()
        };
        persist(self.store.upsert_member(&updated).await, "member role");
        Ok(self.stamp_xp(updated))
    }

    pub async fn add_member(&self, guild_id: Id, user: &User) -> Member {
        let (member, is_new) = {
            let guild_members = self.members.entry(guild_id).or_default();
            if let Some(mut existing) = guild_members.get_mut(&user.pubkey) {
                existing.online = true;
                (existing.clone(), false)
            } else {
                let member = Member {
                    user: user.clone(),
                    guild_id,
                    online: true,
                    bot: false,
                    roles: Vec::new(),
                    xp: 0,
                };
                guild_members.insert(user.pubkey.clone(), member.clone());
                (member, true)
            }
        };
        if is_new {
            persist(self.store.upsert_member(&member).await, "member add");
        }
        self.stamp_xp(member)
    }

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

    pub fn channel_guild(&self, channel_id: Id) -> Option<Id> {
        self.channels.get(&channel_id).map(|c| c.guild_id)
    }

    pub fn guild_catalog(&self) -> Vec<crate::protocol::GuildSummary> {
        self.guilds
            .iter()
            .filter(|g| matches!(g.visibility, crate::protocol::GuildVisibility::Public))
            .map(|g| crate::protocol::GuildSummary {
                id: g.id,
                name: g.name.clone(),
                icon: g.icon.clone(),
                member_count: self.members.get(&g.id).map(|m| m.len() as u32).unwrap_or(0),
            })
            .collect()
    }

    pub fn guild_catalog_page(
        &self,
        offset: u32,
        limit: u32,
    ) -> (Vec<crate::protocol::GuildSummary>, u32) {
        let mut all = self.guild_catalog();
        all.sort_by(|a, b| {
            b.member_count
                .cmp(&a.member_count)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        let total = all.len() as u32;
        let page = all
            .into_iter()
            .skip(offset as usize)
            .take(limit as usize)
            .collect();
        (page, total)
    }

    pub fn is_banned(&self, guild_id: Id, pubkey: &str) -> bool {
        self.bans
            .get(&guild_id)
            .map(|b| b.contains(pubkey))
            .unwrap_or(false)
    }

    pub async fn join_guild(
        &self,
        guild_id: Id,
        user: &User,
    ) -> Result<(Guild, Vec<Channel>, Vec<Member>, Vec<Role>), String> {
        let guild = self
            .guilds
            .get(&guild_id)
            .map(|g| g.clone())
            .ok_or_else(|| "unknown guild".to_string())?;
        if self.is_banned(guild_id, &user.pubkey) {
            return Err("you are banned from this guild".into());
        }
        if matches!(guild.visibility, crate::protocol::GuildVisibility::Private)
            && !self.is_guild_member(guild_id, &user.pubkey)
        {
            return Err("this guild is invite-only".into());
        }
        Ok(self.admit_member(guild, user).await)
    }

    pub async fn join_by_invite(
        &self,
        code: &str,
        user: &User,
    ) -> Result<(Guild, Vec<Channel>, Vec<Member>, Vec<Role>), String> {
        let code = code.trim();
        let guild_id = {
            let mut entry = self
                .invites
                .get_mut(code)
                .ok_or_else(|| "unknown or expired invite code".to_string())?;
            if !entry.is_live(now_ms()) {
                return Err("unknown or expired invite code".into());
            }
            entry.uses += 1;
            entry.guild_id
        };
        let guild = self
            .guilds
            .get(&guild_id)
            .map(|g| g.clone())
            .ok_or_else(|| "unknown or expired invite code".to_string())?;
        if self.is_banned(guild_id, &user.pubkey) {
            if let Some(mut entry) = self.invites.get_mut(code) {
                entry.uses = entry.uses.saturating_sub(1);
            }
            return Err("you are banned from this guild".into());
        }
        persist(self.store.bump_invite_uses(code).await, "invite uses");
        Ok(self.admit_member(guild, user).await)
    }

    async fn admit_member(
        &self,
        guild: Guild,
        user: &User,
    ) -> (Guild, Vec<Channel>, Vec<Member>, Vec<Role>) {
        let guild_id = guild.id;
        self.add_member(guild_id, user).await;
        let channels: Vec<Channel> = self
            .channels
            .iter()
            .filter(|c| c.guild_id == guild_id)
            .map(|c| c.clone())
            .collect();
        let members: Vec<Member> = self
            .members
            .get(&guild_id)
            .map(|m| m.iter().map(|e| self.stamp_xp(e.value().clone())).collect())
            .unwrap_or_default();
        let roles = self.guild_roles(guild_id);
        (guild, channels, members, roles)
    }

    pub async fn set_guild_visibility(
        &self,
        guild_id: Id,
        visibility: crate::protocol::GuildVisibility,
        by_pubkey: &str,
    ) -> Result<Guild, String> {
        self.require_permission(guild_id, by_pubkey, Permission::ManageGuild)?;
        let updated = {
            let mut guild = self
                .guilds
                .get_mut(&guild_id)
                .ok_or_else(|| "unknown guild".to_string())?;
            guild.visibility = visibility;
            guild.clone()
        };
        persist(self.store.upsert_guild(&updated).await, "guild visibility");
        Ok(updated)
    }

    pub async fn get_or_create_invite(
        &self,
        guild_id: Id,
        rotate: bool,
        expires_in_secs: Option<u64>,
        max_uses: Option<u32>,
        by_pubkey: &str,
    ) -> Result<Invite, String> {
        if self
            .require_permission(guild_id, by_pubkey, Permission::CreateInvite)
            .is_err()
        {
            self.require_permission(guild_id, by_pubkey, Permission::ManageGuild)?;
        }
        if !rotate
            && let Some(existing) = self.invite_by_guild.get(&guild_id)
            && let Some(invite) = self.invites.get(existing.value())
            && invite.is_live(now_ms())
        {
            return Ok(invite.clone());
        }
        if let Some((_, old)) = self.invite_by_guild.remove(&guild_id) {
            self.invites.remove(&old);
        }
        let code = loop {
            let candidate = random_invite_code();
            if !self.invites.contains_key(&candidate) {
                break candidate;
            }
        };
        let invite = Invite {
            code: code.clone(),
            guild_id,
            expires_at_ms: expires_in_secs.map(|s| now_ms() + (s as i64) * 1000),
            max_uses,
            uses: 0,
            created_by: by_pubkey.to_string(),
        };
        self.invites.insert(code.clone(), invite.clone());
        self.invite_by_guild.insert(guild_id, code.clone());
        persist(
            self.store
                .set_invite(
                    guild_id,
                    &code,
                    invite.expires_at_ms,
                    invite.max_uses,
                    by_pubkey,
                )
                .await,
            "invite",
        );
        Ok(invite)
    }

    fn validate_moderation_target(
        &self,
        guild_id: Id,
        target_pubkey: &str,
        by_pubkey: &str,
    ) -> Result<(), String> {
        if target_pubkey == by_pubkey {
            return Err("you can't moderate yourself (leave the guild instead)".into());
        }
        if self.is_owner(guild_id, target_pubkey) {
            return Err("the owner can't be kicked or banned".into());
        }
        let target_is_bot = self
            .members
            .get(&guild_id)
            .and_then(|m| m.get(target_pubkey).map(|t| t.bot))
            .unwrap_or(false)
            || self.bot_install(guild_id, target_pubkey).is_some();
        if target_is_bot {
            return Err("bots are removed by uninstalling them, not kick/ban".into());
        }
        if !self.is_owner(guild_id, by_pubkey) {
            let target_perms = self.effective_permissions(guild_id, target_pubkey);
            let protected = [
                Permission::KickMembers,
                Permission::BanMembers,
                Permission::ManageRoles,
                Permission::ManageGuild,
            ];
            if protected.iter().any(|p| target_perms.contains(p)) {
                return Err("only the owner can moderate another moderator".into());
            }
        }
        Ok(())
    }

    fn remove_membership(&self, guild_id: Id, target_pubkey: &str) -> Option<VoiceState> {
        if let Some(gm) = self.members.get(&guild_id) {
            gm.remove(target_pubkey);
        }
        let in_this_guild = self
            .voice_states
            .get(target_pubkey)
            .map(|v| v.guild_id == guild_id)
            .unwrap_or(false);
        if in_this_guild {
            self.clear_voice(target_pubkey)
        } else {
            None
        }
    }

    pub async fn kick_member(
        &self,
        guild_id: Id,
        target_pubkey: &str,
        by_pubkey: &str,
    ) -> Result<Option<VoiceState>, String> {
        self.require_permission(guild_id, by_pubkey, Permission::KickMembers)?;
        self.validate_moderation_target(guild_id, target_pubkey, by_pubkey)?;
        if !self.is_guild_member(guild_id, target_pubkey) {
            return Err("that user isn't a member of this guild".into());
        }
        let cleared = self.remove_membership(guild_id, target_pubkey);
        persist(
            self.store.delete_member(guild_id, target_pubkey).await,
            "member kick",
        );
        Ok(cleared)
    }

    pub async fn ban_member(
        &self,
        guild_id: Id,
        target_pubkey: &str,
        by_pubkey: &str,
    ) -> Result<Option<VoiceState>, String> {
        self.require_permission(guild_id, by_pubkey, Permission::BanMembers)?;
        self.validate_moderation_target(guild_id, target_pubkey, by_pubkey)?;
        // Ban row before the member removal: a crash in between must not restart
        // into a removed-but-unbanned member.
        persist(
            self.store.insert_ban(guild_id, target_pubkey).await,
            "ban insert",
        );
        self.bans
            .entry(guild_id)
            .or_default()
            .insert(target_pubkey.to_string());
        let cleared = self.remove_membership(guild_id, target_pubkey);
        persist(
            self.store.delete_member(guild_id, target_pubkey).await,
            "member ban-remove",
        );
        Ok(cleared)
    }

    pub async fn unban_member(
        &self,
        guild_id: Id,
        target_pubkey: &str,
        by_pubkey: &str,
    ) -> Result<(), String> {
        self.require_permission(guild_id, by_pubkey, Permission::BanMembers)?;
        let removed = self
            .bans
            .get_mut(&guild_id)
            .map(|mut b| b.remove(target_pubkey))
            .unwrap_or(false);
        if removed {
            persist(
                self.store.delete_ban(guild_id, target_pubkey).await,
                "unban",
            );
            Ok(())
        } else {
            Err("that user isn't banned here".into())
        }
    }

    pub fn ban_list(&self, guild_id: Id, by_pubkey: &str) -> Result<Vec<User>, String> {
        self.require_permission(guild_id, by_pubkey, Permission::BanMembers)?;
        Ok(self
            .bans
            .get(&guild_id)
            .map(|b| b.iter().map(|pk| self.resolve_user(pk)).collect())
            .unwrap_or_default())
    }

    pub async fn leave_guild(
        &self,
        guild_id: Id,
        pubkey: &str,
    ) -> Result<Option<VoiceState>, String> {
        let owner = self
            .guilds
            .get(&guild_id)
            .map(|g| g.owner_pubkey.clone())
            .ok_or_else(|| "unknown guild".to_string())?;
        if owner.is_empty() {
            return Err("you can't leave a system guild".into());
        }
        if owner == pubkey {
            return Err("the owner can't leave — transfer ownership or delete the guild".into());
        }
        if !self.is_guild_member(guild_id, pubkey) {
            return Err("you're not a member of this guild".into());
        }
        let cleared = self.remove_membership(guild_id, pubkey);
        persist(
            self.store.delete_member(guild_id, pubkey).await,
            "member leave",
        );
        Ok(cleared)
    }

    pub async fn create_channel(
        &self,
        guild_id: Id,
        name: &str,
        kind: crate::protocol::ChannelKind,
        topic: Option<String>,
        by_pubkey: &str,
    ) -> Result<Channel, String> {
        self.require_permission(guild_id, by_pubkey, Permission::ManageChannels)?;
        let name = sanitize_channel_name(name)?;
        let next_pos = self
            .channels
            .iter()
            .filter(|c| c.guild_id == guild_id)
            .map(|c| c.position + 1)
            .max()
            .unwrap_or(0);
        let channel = Channel {
            id: Uuid::new_v4(),
            guild_id,
            name,
            kind,
            topic: topic
                .filter(|t| !t.trim().is_empty())
                .map(|t| t.chars().take(120).collect()),
            read_only: false,
            slowmode_secs: 0,
            position: next_pos,
        };
        self.channels.insert(channel.id, channel.clone());
        self.channels_by_guild
            .entry(guild_id)
            .or_default()
            .push(channel.id);
        persist(self.store.upsert_channel(&channel).await, "channel create");
        Ok(channel)
    }

    pub async fn reorder_channels(
        &self,
        guild_id: Id,
        positions: &[(Id, u32)],
        by_pubkey: &str,
    ) -> Result<Vec<Channel>, String> {
        self.require_permission(guild_id, by_pubkey, Permission::ManageChannels)?;
        for (id, _) in positions {
            if self.channel_guild(*id) != Some(guild_id) {
                return Err("that channel is not in this guild".into());
            }
        }
        let mut updated = Vec::with_capacity(positions.len());
        for (id, position) in positions {
            let Some(mut entry) = self.channels.get_mut(id) else {
                continue;
            };
            if entry.position == *position {
                continue;
            }
            entry.position = *position;
            updated.push(entry.clone());
        }
        for channel in &updated {
            persist(self.store.upsert_channel(channel).await, "channel position");
        }
        Ok(updated)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn update_channel(
        &self,
        channel_id: Id,
        name: &str,
        topic: Option<String>,
        read_only: bool,
        position: u32,
        slowmode_secs: u32,
        by_pubkey: &str,
    ) -> Result<Channel, String> {
        let guild_id = self
            .channel_guild(channel_id)
            .ok_or_else(|| "unknown channel".to_string())?;
        self.require_permission(guild_id, by_pubkey, Permission::ManageChannels)?;
        let name = sanitize_channel_name(name)?;
        let updated = {
            let mut channel = self
                .channels
                .get_mut(&channel_id)
                .ok_or_else(|| "unknown channel".to_string())?;
            channel.name = name;
            channel.topic = topic
                .filter(|t| !t.trim().is_empty())
                .map(|t| t.chars().take(120).collect());
            channel.read_only =
                read_only && matches!(channel.kind, crate::protocol::ChannelKind::Text);
            channel.slowmode_secs = slowmode_secs.min(21_600);
            channel.position = position;
            channel.clone()
        };
        persist(self.store.upsert_channel(&updated).await, "channel update");
        Ok(updated)
    }

    pub async fn delete_channel(
        &self,
        channel_id: Id,
        by_pubkey: &str,
    ) -> Result<(Id, Vec<VoiceState>), String> {
        let (guild_id, kind) = self
            .channels
            .get(&channel_id)
            .map(|c| (c.guild_id, c.kind))
            .ok_or_else(|| "unknown channel".to_string())?;
        self.require_permission(guild_id, by_pubkey, Permission::ManageChannels)?;
        if matches!(kind, crate::protocol::ChannelKind::Text) {
            let remaining_text = self
                .channels
                .iter()
                .filter(|c| {
                    c.guild_id == guild_id && matches!(c.kind, crate::protocol::ChannelKind::Text)
                })
                .count();
            if remaining_text <= 1 {
                return Err("a guild needs at least one text channel".into());
            }
        }
        let occupants: Vec<String> = self
            .voice_states
            .iter()
            .filter(|v| v.channel_id == Some(channel_id))
            .map(|v| v.user_pubkey.clone())
            .collect();
        let cleared: Vec<VoiceState> = occupants
            .iter()
            .filter_map(|pk| self.clear_voice(pk))
            .collect();
        self.channels.remove(&channel_id);
        if let Some(mut ids) = self.channels_by_guild.get_mut(&guild_id) {
            ids.retain(|c| *c != channel_id);
        }
        persist(
            self.store.delete_channel(channel_id).await,
            "channel delete",
        );
        Ok((guild_id, cleared))
    }

    pub async fn transfer_ownership(
        &self,
        guild_id: Id,
        new_owner_pubkey: &str,
        by_pubkey: &str,
    ) -> Result<Guild, String> {
        let owner = self
            .guilds
            .get(&guild_id)
            .map(|g| g.owner_pubkey.clone())
            .ok_or_else(|| "unknown guild".to_string())?;
        if owner.is_empty() {
            return Err("system guilds have no transferable ownership".into());
        }
        if owner != by_pubkey {
            return Err("only the owner can transfer ownership".into());
        }
        if new_owner_pubkey == by_pubkey {
            return Err("you already own this guild".into());
        }
        let target_is_bot = self
            .members
            .get(&guild_id)
            .and_then(|m| m.get(new_owner_pubkey).map(|t| t.bot))
            .unwrap_or(true); // absent member -> handled below, default safe
        if !self.is_guild_member(guild_id, new_owner_pubkey) {
            return Err("the new owner must already be a member".into());
        }
        if target_is_bot || self.bot_install(guild_id, new_owner_pubkey).is_some() {
            return Err("a bot can't own a guild".into());
        }
        let updated = {
            let mut guild = self
                .guilds
                .get_mut(&guild_id)
                .ok_or_else(|| "unknown guild".to_string())?;
            guild.owner_pubkey = new_owner_pubkey.to_string();
            guild.clone()
        };
        persist(
            self.store.upsert_guild(&updated).await,
            "ownership transfer",
        );
        Ok(updated)
    }

    pub async fn set_guild_profile(
        &self,
        guild_id: Id,
        description: Option<String>,
        icon_image: Option<String>,
        banner: Option<String>,
        by_pubkey: &str,
    ) -> Result<Guild, String> {
        self.require_permission(guild_id, by_pubkey, Permission::ManageGuild)?;
        let valid_image = |img: &String| {
            let is_url =
                (img.starts_with("https://") || img.starts_with("http://")) && img.len() <= 2048;
            let is_data = img.starts_with("data:image/") && img.len() <= MAX_IMAGE_LEN;
            is_url || is_data
        };
        if icon_image.as_ref().is_some_and(|i| !valid_image(i))
            || banner.as_ref().is_some_and(|i| !valid_image(i))
        {
            return Err(format!(
                "Guild icon and banner must be an image (PNG, JPEG, GIF or WebP) \
                 under {} MB, or a link to one.",
                MAX_IMAGE_LEN / 1_000_000
            ));
        }
        let description = description
            .map(|d| d.trim().chars().take(280).collect::<String>())
            .filter(|d| !d.is_empty());
        let updated = {
            let mut guild = self
                .guilds
                .get_mut(&guild_id)
                .ok_or_else(|| "unknown guild".to_string())?;
            guild.description = description;
            guild.icon_image = icon_image;
            guild.banner = banner;
            guild.clone()
        };
        persist(self.store.upsert_guild(&updated).await, "guild profile");
        Ok(updated)
    }

    /// Runs after the retention sweep, which is what *creates* unreferenced
    /// blobs. A failed query means we do not know what is referenced — so keep.
    pub async fn sweep_media(&self) -> crate::media::SweepReport {
        match self.store.referenced_media().await {
            Ok(referenced) => self.media.sweep(&referenced, MEDIA_GRACE),
            Err(e) => {
                tracing::error!(error = %e, "media sweep skipped: could not read references");
                crate::media::SweepReport::default()
            }
        }
    }

    pub async fn set_guild_retention(
        &self,
        guild_id: Id,
        days: Option<u32>,
        by_pubkey: &str,
    ) -> Result<Guild, String> {
        self.require_permission(guild_id, by_pubkey, Permission::ManageGuild)?;
        let days = days.map(|d| d.clamp(1, 3650));
        let updated = {
            let mut guild = self
                .guilds
                .get_mut(&guild_id)
                .ok_or_else(|| "unknown guild".to_string())?;
            guild.retention_days = days;
            guild.clone()
        };
        persist(self.store.upsert_guild(&updated).await, "guild retention");
        Ok(updated)
    }

    pub async fn set_join_gate(
        &self,
        guild_id: Id,
        gate: crate::protocol::JoinGate,
        rules: Option<String>,
        by_pubkey: &str,
    ) -> Result<Guild, String> {
        self.require_permission(guild_id, by_pubkey, Permission::ManageGuild)?;
        let updated = {
            let mut guild = self
                .guilds
                .get_mut(&guild_id)
                .ok_or_else(|| "unknown guild".to_string())?;
            guild.join_gate = gate;
            guild.rules = rules.map(|r| r.chars().take(4000).collect());
            guild.clone()
        };
        persist(self.store.upsert_guild(&updated).await, "join gate");
        self.audit(
            guild_id,
            by_pubkey,
            "set_join_gate",
            "",
            &format!("{gate:?}"),
        )
        .await;
        Ok(updated)
    }

    pub async fn set_panic_mode(
        &self,
        guild_id: Id,
        on: bool,
        by_pubkey: &str,
    ) -> Result<Guild, String> {
        self.require_permission(guild_id, by_pubkey, Permission::ManageGuild)?;
        let updated = self.set_panic_flag(guild_id, on).await?;
        self.audit(guild_id, by_pubkey, "set_panic_mode", "", &on.to_string())
            .await;
        Ok(updated)
    }

    async fn set_panic_flag(&self, guild_id: Id, on: bool) -> Result<Guild, String> {
        let updated = {
            let mut guild = self
                .guilds
                .get_mut(&guild_id)
                .ok_or_else(|| "unknown guild".to_string())?;
            guild.panic_mode = on;
            guild.clone()
        };
        persist(self.store.upsert_guild(&updated).await, "panic mode");
        Ok(updated)
    }

    pub fn invite_guild(&self, code: &str) -> Option<Id> {
        let now = now_ms();
        self.invites
            .get(code.trim())
            .filter(|i| i.is_live(now))
            .map(|i| i.guild_id)
    }

    pub fn join_requirements(
        &self,
        guild_id: Id,
    ) -> Option<(crate::protocol::JoinGate, Option<String>, bool)> {
        self.guilds
            .get(&guild_id)
            .map(|g| (g.join_gate, g.rules.clone(), g.panic_mode))
    }

    pub async fn note_join_and_maybe_panic(&self, guild_id: Id) -> Option<Guild> {
        const WINDOW: std::time::Duration = std::time::Duration::from_secs(60);
        const THRESHOLD: usize = 10; // >10 joins/min → lock down
        let now = std::time::Instant::now();
        let count = {
            let mut v = self.recent_joins.entry(guild_id).or_default();
            v.retain(|t| now.duration_since(*t) < WINDOW);
            v.push(now);
            v.len()
        };
        let already = self
            .guilds
            .get(&guild_id)
            .map(|g| g.panic_mode)
            .unwrap_or(false);
        if count > THRESHOLD && !already {
            tracing::warn!(%guild_id, count, "mass-join detected — auto panic mode");
            let g = self.set_panic_flag(guild_id, true).await.ok();
            if let Some(g) = &g {
                self.audit(
                    guild_id,
                    "",
                    "auto_panic",
                    "",
                    &format!("{count} joins/min"),
                )
                .await;
                return Some(g.clone());
            }
        }
        None
    }

    pub fn slowmode_check(&self, channel_id: Id, pubkey: &str) -> Result<(), u64> {
        let secs = self
            .channels
            .get(&channel_id)
            .map(|c| c.slowmode_secs)
            .unwrap_or(0);
        if secs == 0 {
            return Ok(());
        }
        let now = std::time::Instant::now();
        let key = (channel_id, pubkey.to_string());
        if let Some(last) = self.last_post.get(&key) {
            let elapsed = now.duration_since(*last).as_secs();
            if elapsed < secs as u64 {
                return Err(secs as u64 - elapsed);
            }
        }
        self.last_post.insert(key, now);
        Ok(())
    }

    pub async fn audit(&self, guild_id: Id, actor: &str, action: &str, target: &str, detail: &str) {
        let entry = crate::protocol::AuditEntry {
            at_ms: chrono::Utc::now().timestamp_millis(),
            actor_pubkey: actor.to_string(),
            action: action.to_string(),
            target: target.to_string(),
            detail: detail.to_string(),
        };
        persist(self.store.append_audit(guild_id, &entry).await, "audit");
    }

    pub async fn audit_log(
        &self,
        guild_id: Id,
        by_pubkey: &str,
    ) -> Result<Vec<crate::protocol::AuditEntry>, String> {
        self.require_permission(guild_id, by_pubkey, Permission::ManageGuild)?;
        self.store
            .audit_log(guild_id, 100)
            .await
            .map_err(|e| format!("store error: {e}"))
    }

    pub async fn sweep_retention(&self) -> u64 {
        let targets: Vec<(Id, u32)> = self
            .guilds
            .iter()
            .filter_map(|g| g.retention_days.map(|d| (g.id, d)))
            .collect();
        let mut total = 0u64;
        for (gid, days) in targets {
            let cutoff = chrono::Utc::now().timestamp_millis() - (days as i64) * 86_400_000;
            match self.store.sweep_guild_messages(gid, cutoff).await {
                Ok(n) => total += n,
                Err(e) => tracing::error!(error = %e, %gid, "retention sweep failed"),
            }
        }
        total
    }

    pub fn channel_read_only(&self, channel_id: Id) -> bool {
        self.channels
            .get(&channel_id)
            .map(|c| c.read_only)
            .unwrap_or(false)
    }

    pub async fn delete_message(
        &self,
        channel_id: Id,
        message_id: Id,
        by_pubkey: &str,
    ) -> Result<(), String> {
        let author = self
            .store
            .message_author(channel_id, message_id)
            .await
            .map_err(|e| format!("store error: {e}"))?
            .ok_or_else(|| "unknown message".to_string())?;
        if author != by_pubkey {
            match self.channel_guild(channel_id) {
                Some(gid) => self.require_permission(gid, by_pubkey, Permission::ManageMessages)?,
                None => return Err("only the author can delete a DM message".into()),
            }
        }
        self.store
            .delete_message(channel_id, message_id)
            .await
            .map_err(|e| format!("store error: {e}"))
    }

    pub async fn delete_guild(&self, guild_id: Id, by_pubkey: &str) -> Result<(), String> {
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
        self.roles.remove(&guild_id);
        self.bans.remove(&guild_id);
        if let Some((_, code)) = self.invite_by_guild.remove(&guild_id) {
            self.invites.remove(&code);
        }
        if let Some((_, channel_ids)) = self.channels_by_guild.remove(&guild_id) {
            for cid in channel_ids {
                self.channels.remove(&cid);
            }
        }
        persist(self.store.delete_guild(guild_id).await, "guild delete");
        Ok(())
    }

    pub async fn history(
        &self,
        channel_id: Id,
        limit: u32,
        before_ms: Option<i64>,
    ) -> Vec<Message> {
        match self.store.history(channel_id, limit, before_ms).await {
            Ok(mut messages) => {
                for m in &mut messages {
                    if let Some(img) = &m.image {
                        m.image = self.media.inline(img);
                    }
                }
                messages
            }
            Err(e) => {
                tracing::error!(error = %e, "history query failed");
                Vec::new()
            }
        }
    }

    pub async fn push_message(
        &self,
        channel_id: Id,
        author: User,
        content: String,
        image: Option<String>,
        reply_to: Option<Id>,
    ) -> Option<Message> {
        let author = self.attributed_author(channel_id, author);
        let kind_ok = self
            .channels
            .get(&channel_id)
            .map(|c| matches!(c.kind, crate::protocol::ChannelKind::Text))
            .unwrap_or(false);
        if !kind_ok {
            return None;
        }
        let reply_ref = match reply_to {
            Some(id) => self.store.reply_ref(channel_id, id).await.unwrap_or(None),
            None => None,
        };
        Some(
            self.append_message(channel_id, author, content, image, reply_ref)
                .await,
        )
    }

    fn attributed_author(&self, channel_id: Id, author: User) -> User {
        let Some(guild_id) = self.channel_guild(channel_id) else {
            return author;
        };
        match self.bot_install(guild_id, &author.pubkey) {
            Some(install) => User {
                username: install.name,
                ..author
            },
            None => author,
        }
    }

    async fn append_message(
        &self,
        channel_id: Id,
        author: User,
        content: String,
        image: Option<String>,
        reply_to: Option<ReplyRef>,
    ) -> Message {
        let stored_image = image.as_ref().map(|img| {
            if img.starts_with("data:") {
                self.media
                    .store_data_url(img)
                    .unwrap_or_else(|| img.clone())
            } else {
                img.clone()
            }
        });
        let message = Message {
            id: Uuid::new_v4(),
            channel_id,
            author,
            content,
            image: stored_image,
            reactions: Vec::new(),
            reply_to,
            created_at: chrono::Utc::now(),
        };
        persist(self.store.insert_message(&message).await, "message insert");
        Message { image, ..message }
    }

    pub async fn rename_user(&self, pubkey: &str, username: &str) -> Vec<Member> {
        if let Some(mut u) = self.users.get_mut(pubkey) {
            u.username = username.to_string();
        }
        let user = User {
            pubkey: pubkey.to_string(),
            username: username.to_string(),
        };
        persist(self.store.upsert_user(&user).await, "user rename");

        let mut changed = Vec::new();
        for entry in self.members.iter() {
            let Some(mut m) = entry.value().get_mut(pubkey) else {
                continue;
            };
            if m.bot || m.user.username == username {
                continue;
            }
            m.user.username = username.to_string();
            changed.push(m.clone());
        }
        for m in &changed {
            persist(self.store.upsert_member(m).await, "member rename");
        }
        changed
    }

    pub fn mark_offline(&self, user_pubkey: &str) -> Vec<(Id, String)> {
        let mut affected = Vec::new();
        for entry in self.members.iter() {
            let guild_id = *entry.key();
            if let Some(mut m) = entry.value().get_mut(user_pubkey)
                && m.online
            {
                m.online = false;
                affected.push((guild_id, user_pubkey.to_string()));
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
            screen_sharing: false,
            camera_on: false,
        };
        if channel_id.is_some() {
            self.voice_states
                .insert(user_pubkey.to_string(), state.clone());
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
        entry.muted = muted || deafened;
        entry.deafened = deafened;
        Some(entry.clone())
    }

    pub fn update_camera(&self, user_pubkey: &str, on: bool) -> Option<VoiceState> {
        let mut entry = self.voice_states.get_mut(user_pubkey)?;
        if entry.camera_on == on {
            return None;
        }
        entry.camera_on = on;
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

    pub fn clear_voice(&self, user_pubkey: &str) -> Option<VoiceState> {
        let prev = self.voice_states.remove(user_pubkey)?.1;
        Some(VoiceState {
            channel_id: None,
            speaking: false,
            camera_on: false,
            screen_sharing: false,
            ..prev
        })
    }

    pub fn bot_install(&self, guild_id: Id, bot_pubkey: &str) -> Option<BotInstall> {
        self.bot_installs
            .get(bot_pubkey)
            .and_then(|g| g.get(&guild_id).map(|i| i.clone()))
    }

    pub fn bot_guilds(&self, bot_pubkey: &str) -> Vec<BotInstall> {
        self.bot_installs
            .get(bot_pubkey)
            .map(|g| g.iter().map(|e| e.value().clone()).collect())
            .unwrap_or_default()
    }

    pub fn guild_installs(&self, guild_id: Id) -> Vec<BotInstall> {
        self.bot_installs
            .iter()
            .filter_map(|e| e.value().get(&guild_id).map(|i| i.clone()))
            .collect()
    }

    pub async fn install_bot(
        &self,
        guild_id: Id,
        bot_pubkey: &str,
        name: &str,
        permissions: Vec<Permission>,
        intents: Vec<Intent>,
        by_pubkey: &str,
    ) -> Result<(BotInstall, Member), String> {
        self.require_permission(guild_id, by_pubkey, Permission::ManageGuild)?;
        if bot_pubkey.trim().is_empty() {
            return Err("bot pubkey is required".into());
        }
        if bot_pubkey == by_pubkey {
            return Err("a bot must have its own identity, distinct from yours".into());
        }
        if let Some(bad) = permissions
            .iter()
            .find(|p| !Permission::BOT_INSTALLABLE.contains(p))
        {
            return Err(format!(
                "{} isn't grantable to bots",
                serde_variant_name(*bad)
            ));
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

        let member = {
            let guild_members = self.members.entry(guild_id).or_default();
            if let Some(mut existing) = guild_members.get_mut(bot_pubkey) {
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
                    roles: Vec::new(),
                    xp: 0,
                };
                guild_members.insert(bot_pubkey.to_string(), member.clone());
                member
            }
        };
        persist(self.store.upsert_bot_install(&install).await, "bot install");
        persist(self.store.upsert_member(&member).await, "bot member");
        Ok((install, member))
    }

    pub async fn uninstall_bot(
        &self,
        guild_id: Id,
        bot_pubkey: &str,
        by_pubkey: &str,
    ) -> Result<(), String> {
        self.require_permission(guild_id, by_pubkey, Permission::ManageGuild)?;
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
        persist(
            self.store.delete_bot_install(guild_id, bot_pubkey).await,
            "bot uninstall",
        );
        persist(
            self.store.delete_member(guild_id, bot_pubkey).await,
            "bot member remove",
        );
        Ok(())
    }

    pub fn snapshot_for_bot(&self, bot: &User) -> ServerMessage {
        let installs = self.bot_guilds(&bot.pubkey);
        let my_guild_ids: Vec<Id> = installs.iter().map(|i| i.guild_id).collect();

        for gid in &my_guild_ids {
            if let Some(gm) = self.members.get(gid)
                && let Some(mut m) = gm.get_mut(&bot.pubkey)
            {
                m.online = true;
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
        for install in &installs {
            if !install.has_intent(Intent::Members) {
                continue;
            }
            if let Some(gm) = self.members.get(&install.guild_id) {
                for m in gm.iter() {
                    members.push(self.stamp_xp(m.value().clone()));
                }
            }
        }

        ServerMessage::Ready {
            user: bot.clone(),
            guilds,
            channels,
            members,
            voice_states: Vec::new(),
            catalog: Vec::new(),
            profiles: Vec::new(),
            // Roles never apply to a bot connection, and operators are never
            // bots: sending either would leak a guild's structure to it.
            roles: Vec::new(),
            emojis: Vec::new(),
            operator: false,
        }
    }
}

struct GuildTemplate {
    channels: Vec<(&'static str, crate::protocol::ChannelKind, bool)>,
    roles: Vec<(&'static str, Vec<Permission>)>,
    visibility: crate::protocol::GuildVisibility,
    join_gate: crate::protocol::JoinGate,
}

impl GuildTemplate {
    fn resolve(template: Option<&str>) -> GuildTemplate {
        use crate::protocol::ChannelKind::{Text, Voice};
        use crate::protocol::GuildVisibility::{Private, Public};
        use crate::protocol::JoinGate;
        use crate::protocol::Permission::*;
        match template {
            Some("foss") => GuildTemplate {
                channels: vec![
                    ("announcements", Text, true),
                    ("general", Text, false),
                    ("dev", Text, false),
                    ("support", Text, false),
                    ("Voice", Voice, false),
                ],
                roles: vec![
                    (
                        "Maintainer",
                        vec![
                            ManageChannels,
                            ManageMessages,
                            KickMembers,
                            BanMembers,
                            ManageRoles,
                            ManageGuild,
                        ],
                    ),
                    ("Contributor", vec![]),
                ],
                visibility: Public,
                join_gate: JoinGate::Open,
            },
            Some("community") => GuildTemplate {
                channels: vec![
                    ("rules", Text, true),
                    ("announcements", Text, true),
                    ("general", Text, false),
                    ("off-topic", Text, false),
                    ("Lounge", Voice, false),
                ],
                roles: vec![
                    (
                        "Admin",
                        vec![
                            ManageChannels,
                            ManageMessages,
                            KickMembers,
                            BanMembers,
                            ManageRoles,
                            ManageGuild,
                            CreateInvite,
                        ],
                    ),
                    ("Moderator", vec![ManageMessages, KickMembers, BanMembers]),
                ],
                visibility: Private,
                join_gate: JoinGate::Rules,
            },
            _ => GuildTemplate {
                channels: vec![("general", Text, false), ("General Voice", Voice, false)],
                roles: vec![],
                visibility: Public,
                join_gate: JoinGate::Open,
            },
        }
    }
}

fn sanitize_channel_name(raw: &str) -> Result<String, String> {
    crate::protocol::sanitize_name("channel", raw, 64)
}

pub(crate) fn random_invite_code() -> String {
    use rand::Rng;
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut rng = rand::rngs::OsRng;
    (0..12)
        .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
        .collect()
}

pub fn is_hex_color(s: &str) -> bool {
    let s = s.trim();
    let Some(hex) = s.strip_prefix('#') else {
        return false;
    };
    (hex.len() == 3 || hex.len() == 6) && hex.chars().all(|c| c.is_ascii_hexdigit())
}

fn serde_variant_name<T: serde::Serialize>(v: T) -> String {
    serde_json::to_string(&v)
        .unwrap_or_default()
        .trim_matches('"')
        .to_string()
}

fn unique<T: PartialEq>(items: Vec<T>) -> Vec<T> {
    let mut out: Vec<T> = Vec::with_capacity(items.len());
    for item in items {
        if !out.contains(&item) {
            out.push(item);
        }
    }
    out
}

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
