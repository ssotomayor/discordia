use std::sync::atomic::{AtomicU64, Ordering};

use dashmap::DashMap;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::media::MediaStore;
use crate::protocol::{
    BotInstall, Channel, Guild, GuildEmoji, GuildSound, GuildVisibility, Id, Intent,
    MAX_EMOJIS_PER_GUILD, MAX_SOUNDS_PER_GUILD, Member, Message, Permission, Profile, ReplyRef,
    Role, ServerMessage, User, VoiceState, valid_shortcode,
};
use crate::store::Store;

mod apps;
mod blocklists;
mod chat_tools;
mod commands;
mod instances;
pub use apps::ManifestOutcome;

/// A failed write is logged and ignored: the change survives the session but
/// not a restart, and is never a user-facing error.
fn persist(res: Result<(), sqlx::Error>, what: &str) {
    if let Err(e) = res {
        tracing::error!(error = %e, what, "write-through persist FAILED — state will regress on restart");
    }
}

fn durable<T>(res: Result<T, sqlx::Error>, what: &str) -> Result<T, String> {
    res.map_err(|e| {
        tracing::error!(error = %e, what, "durable write failed");
        "The server could not save this change. Please try again.".to_string()
    })
}

const CONN_QUEUE_CAP: usize = 1024;
/// Sockets, not people: a client at rest holds one. Past these, a new socket
/// is told so and closed before it costs a queue or a Schnorr verification.
pub const MAX_CONNECTIONS: usize = 4096;
pub const MAX_CONNECTIONS_PER_IP: u32 = 32;
pub const MAX_SESSIONS_PER_PUBKEY: usize = 8;
pub const MAX_GUILDS_PER_OWNER: usize = 10;
const CATALOG_FIRST_PAGE: u32 = 100;

/// Counted on the data URL a client sends, per pubkey, over a sliding day.
/// The disk quota is the real ceiling; this stops one member reaching it alone.
pub const UPLOAD_BUDGET_BYTES: u64 = 200 * 1024 * 1024;
const UPLOAD_WINDOW: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

pub const MAX_IMAGE_LEN: usize = 3_000_000;

#[derive(Debug)]
pub enum ProfileWrite {
    Saved(Profile),
    Stale(Profile),
}

struct Conn {
    tx: mpsc::Sender<ServerMessage>,
    ip: Option<std::net::IpAddr>,
    host: Option<String>,
    route: crate::connection_routes::RouteSource,
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
    /// Deliberately not in `profiles`: an activity is ephemeral, so it must not
    /// reach `store` and must not survive the socket that set it.
    pub activities: DashMap<String, crate::protocol::Activity>,
    pub voice_states: DashMap<String, VoiceState>,
    voice_owners: std::sync::Mutex<std::collections::HashMap<String, u64>>,
    pub bot_installs: DashMap<String, DashMap<Id, BotInstall>>,
    /// Memory only: a bot declares its commands on every connect.
    pub bot_commands: DashMap<String, Vec<crate::protocol::BotCommand>>,
    /// The newest manifest event each bot has handed this host, by version.
    pub app_manifests: DashMap<String, (u64, String)>,
    activity_instances: instances::Instances,
    /// The curators each guild follows, and the newest list each curator signed.
    followed_blocklists: DashMap<Id, Vec<String>>,
    app_blocklists: DashMap<String, crate::protocol::AppBlocklist>,
    /// Per caller and callee: when the minute started and calls made in it.
    app_calls: DashMap<(String, String), (std::time::Instant, u32)>,
    invocations: DashMap<Id, commands::Pending>,
    pub roles: DashMap<Id, Vec<Role>>,
    pub emojis: DashMap<Id, Vec<GuildEmoji>>,
    pub sounds: DashMap<Id, Vec<GuildSound>>,
    pub invites: DashMap<String, Invite>,
    pub invite_by_guild: DashMap<Id, String>,
    pub bans: DashMap<Id, std::collections::HashSet<String>>,
    pub last_post: DashMap<(Id, String), std::time::Instant>,
    pub recent_joins: DashMap<Id, Vec<std::time::Instant>>,
    pub xp: DashMap<Id, DashMap<String, u64>>,
    /// Memory only, and deliberately: losing it across a restart grants one
    /// extra point, which is cheaper than a disk write per message.
    pub xp_cooldown: DashMap<Id, DashMap<String, std::time::Instant>>,
    pub operators: std::collections::HashSet<String>,
    default_guild: std::sync::Mutex<Option<Id>>,
    conns: DashMap<u64, Conn>,
    conns_by_ip: DashMap<std::net::IpAddr, u32>,
    conn_ids_by_pubkey: DashMap<String, std::collections::HashSet<u64>>,
    session_revisions: DashMap<String, u64>,
    next_conn_id: AtomicU64,
    upload_budget: DashMap<String, (std::time::Instant, u64)>,
    durable_writes: tokio::sync::Mutex<()>,
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
            activities: DashMap::new(),
            bot_installs: DashMap::new(),
            bot_commands: DashMap::new(),
            app_manifests: DashMap::new(),
            activity_instances: Default::default(),
            followed_blocklists: DashMap::new(),
            app_blocklists: DashMap::new(),
            app_calls: DashMap::new(),
            invocations: DashMap::new(),
            roles: DashMap::new(),
            emojis: DashMap::new(),
            sounds: DashMap::new(),
            invites: DashMap::new(),
            invite_by_guild: DashMap::new(),
            bans: DashMap::new(),
            last_post: DashMap::new(),
            recent_joins: DashMap::new(),
            xp: DashMap::new(),
            xp_cooldown: DashMap::new(),
            voice_states: DashMap::new(),
            voice_owners: std::sync::Mutex::new(Default::default()),
            operators,
            default_guild: std::sync::Mutex::new(None),
            conns: DashMap::new(),
            conns_by_ip: DashMap::new(),
            conn_ids_by_pubkey: DashMap::new(),
            session_revisions: DashMap::new(),
            next_conn_id: AtomicU64::new(1),
            upload_budget: DashMap::new(),
            durable_writes: tokio::sync::Mutex::new(()),
        };

        let mut loaded = state.store.load_all().await?;
        crate::sanitize::loaded(&mut loaded);
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
                    look: Default::default(),
                },
            );
        }
        for r in loaded.roles {
            state.roles.entry(r.guild_id).or_default().push(r);
        }
        for mut roles in state.roles.iter_mut() {
            roles.sort_by_key(|r| r.position);
        }
        for e in loaded.emojis {
            state.emojis.entry(e.guild_id).or_default().push(e);
        }
        for s in loaded.sounds {
            state.sounds.entry(s.guild_id).or_default().push(s);
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
            // Rows from before activity-only apps went unseated.
            if let crate::store::Seat::Vacated = state.seat_for(&i) {
                state.store.delete_member(i.guild_id, &i.bot_pubkey).await?;
                state.apply_seat(&i, &crate::store::Seat::Vacated);
            }
            // The members table keeps no look: the install is its one copy.
            if let Some(g) = state.members.get(&i.guild_id)
                && let Some(mut m) = g.get_mut(&i.bot_pubkey)
                && m.bot
            {
                m.look = i.look.clone();
            }
            state
                .bot_installs
                .entry(i.bot_pubkey.clone())
                .or_default()
                .insert(i.guild_id, i);
        }
        for (bot, version, event) in state.store.app_manifests().await? {
            state.app_manifests.insert(bot, (version, event));
        }
        for (guild_id, curator) in state.store.guild_blocklists().await? {
            state
                .followed_blocklists
                .entry(guild_id)
                .or_default()
                .push(curator);
        }
        for list in state.store.app_blocklists().await? {
            state.app_blocklists.insert(list.curator.clone(), list);
        }

        *state.default_guild_slot() = state.store.default_guild().await?;
        state.detach_inline_images().await;
        if fresh {
            state.seed_lobby().await?;
        }
        Ok(state)
    }

    async fn seed_lobby(&self) -> Result<(), sqlx::Error> {
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
            leveling: Default::default(),
        };
        let general = Channel {
            divider: Default::default(),
            id: Uuid::new_v4(),
            guild_id: lobby.id,
            name: "general".into(),
            kind: crate::protocol::ChannelKind::Text,
            topic: None,
            read_only: false,
            slowmode_secs: 0,
            markdown: false,
            position: 0,
            access: None,
        };
        let voice = Channel {
            divider: Default::default(),
            id: Uuid::new_v4(),
            guild_id: lobby.id,
            name: "General Voice".into(),
            kind: crate::protocol::ChannelKind::Voice,
            topic: None,
            read_only: false,
            slowmode_secs: 0,
            markdown: false,
            position: 1,
            access: None,
        };
        self.store
            .create_guild(&lobby, &[general.clone(), voice.clone()], &[], None)
            .await?;
        self.channels_by_guild
            .insert(lobby.id, vec![general.id, voice.id]);
        for ch in [general, voice] {
            self.channels.insert(ch.id, ch);
        }
        self.guilds.insert(lobby.id, lobby);
        Ok(())
    }

    pub fn register_conn(
        &self,
        ip: Option<std::net::IpAddr>,
    ) -> Result<(u64, mpsc::Receiver<ServerMessage>), &'static str> {
        if self.conns.len() >= MAX_CONNECTIONS {
            return Err("this server is full");
        }
        if let Some(ip) = ip {
            let mut count = self.conns_by_ip.entry(ip).or_default();
            if *count >= MAX_CONNECTIONS_PER_IP {
                return Err("too many connections from your address");
            }
            *count += 1;
        }
        let id = self.next_conn_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel(CONN_QUEUE_CAP);
        self.conns.insert(
            id,
            Conn {
                tx,
                ip,
                host: None,
                route: crate::connection_routes::RouteSource::Socket(ip),
            },
        );
        Ok((id, rx))
    }

    pub(crate) fn set_connection_host(&self, conn_id: u64, host: Option<String>) {
        if let Some(mut conn) = self.conns.get_mut(&conn_id) {
            conn.host = host;
        }
    }

    pub(crate) fn connection_voice_address(
        &self,
        conn_id: u64,
    ) -> (Option<String>, Option<std::net::IpAddr>) {
        self.conns
            .get(&conn_id)
            .map(|c| (c.host.clone(), c.ip))
            .unwrap_or_default()
    }

    pub fn identify_conn(&self, conn_id: u64, pubkey: &str) -> Result<(), String> {
        let mut set = self
            .conn_ids_by_pubkey
            .entry(pubkey.to_string())
            .or_default();
        if set.len() >= MAX_SESSIONS_PER_PUBKEY {
            return Err(format!(
                "this key already has {MAX_SESSIONS_PER_PUBKEY} open sessions"
            ));
        }
        set.insert(conn_id);
        self.bump_session_revision(pubkey);
        Ok(())
    }

    pub(crate) fn set_connection_route(
        &self,
        id: u64,
        route: crate::connection_routes::RouteSource,
    ) {
        if let Some(mut conn) = self.conns.get_mut(&id) {
            conn.route = route;
        }
    }

    pub fn connection_routes(&self) -> Vec<crate::connection_routes::HostConnection> {
        let sessions: Vec<_> = self
            .conn_ids_by_pubkey
            .iter()
            .map(|entry| {
                (
                    entry.key().clone(),
                    entry.value().iter().copied().collect::<Vec<_>>(),
                )
            })
            .collect();
        let mut result = Vec::new();
        for (pubkey, ids) in sessions {
            let name = self
                .users
                .get(&pubkey)
                .map(|user| user.username.clone())
                .unwrap_or_else(|| pubkey.clone());
            for id in ids {
                let source = self.conns.get(&id).map(|conn| conn.route.clone());
                if let Some(source) = source {
                    result.push(crate::connection_routes::HostConnection {
                        id,
                        pubkey: pubkey.clone(),
                        name: name.clone(),
                        route: source.current(),
                    });
                }
            }
        }
        result.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        result
    }

    /// The only way out of `conns`, so the per-address count follows the
    /// socket whether it left by closing or by `route` dropping it.
    fn drop_conn(&self, conn_id: u64) {
        if let Some((_, conn)) = self.conns.remove(&conn_id)
            && let Some(ip) = conn.ip
        {
            self.conns_by_ip.remove_if_mut(&ip, |_, count| {
                *count = count.saturating_sub(1);
                *count == 0
            });
        }
    }

    /// Whether this key still holds an identified socket. A teardown reads it
    /// before saying someone went away: a second session closing is not that.
    pub fn has_sessions(&self, pubkey: &str) -> bool {
        self.conn_ids_by_pubkey
            .get(pubkey)
            .is_some_and(|set| !set.is_empty())
    }

    fn bump_session_revision(&self, pubkey: &str) {
        let revision = self.next_conn_id.fetch_add(1, Ordering::Relaxed);
        self.session_revisions.insert(pubkey.to_string(), revision);
    }

    pub fn session_revision(&self, pubkey: &str) -> u64 {
        self.session_revisions.get(pubkey).map(|r| *r).unwrap_or(0)
    }

    pub fn unregister_conn(&self, conn_id: u64, pubkey: Option<&str>) {
        self.drop_conn(conn_id);
        if let Some(pk) = pubkey {
            self.bump_session_revision(pk);
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

    /// A full queue drops the socket, since a client that missed state has to
    /// re-snapshot anyway — except for typing and activity frames, which nobody
    /// needs replayed and which a hostile guild could use to knock members offline.
    fn route(&self, conn_id: u64, msg: &ServerMessage) {
        let tx = match self.conns.get(&conn_id) {
            Some(c) => c.tx.clone(),
            None => return,
        };
        if tx.try_send(msg.clone()).is_err()
            && !matches!(
                msg,
                ServerMessage::TypingUpdate { .. } | ServerMessage::ActivityEvent { .. }
            )
        {
            tracing::warn!(conn_id, "outbound queue full — dropping the connection");
            self.drop_conn(conn_id);
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

    /// One socket, not a key: the answer to a request goes back to the
    /// connection that asked, not to every session the key holds.
    pub fn deliver_to_conn(&self, conn_id: u64, msg: &ServerMessage) {
        self.route(conn_id, msg);
    }

    pub async fn remember_user(&self, user: &User) -> Result<(), String> {
        let _write = self.durable_writes.lock().await;
        durable(self.store.upsert_user(user).await, "user")?;
        self.users.insert(user.pubkey.clone(), user.clone());
        Ok(())
    }

    /// `Stale` carries what is held instead, for the device that sent the old one.
    pub async fn set_profile(&self, profile: Profile) -> Result<ProfileWrite, String> {
        let _write = self.durable_writes.lock().await;
        if let Some(held) = self.newer_profile(&profile.pubkey, profile.updated_at) {
            return Ok(ProfileWrite::Stale(held));
        }
        durable(self.store.upsert_profile(&profile).await, "profile")?;
        self.profiles
            .insert(profile.pubkey.clone(), profile.clone());
        Ok(ProfileWrite::Saved(self.shown_profile(profile)))
    }

    /// Equal stamps are accepted: two unstamped copies are both 0.
    pub fn newer_profile(&self, pubkey: &str, updated_at: i64) -> Option<Profile> {
        let held = self.profiles.get(pubkey).map(|p| p.value().clone())?;
        (held.updated_at > updated_at).then(|| self.shown_profile(held))
    }

    fn shown_profile(&self, mut p: Profile) -> Profile {
        p.operator = !p.hide_operator && self.operators.contains(&p.pubkey);
        p
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

    /// One award per cooldown, not per message: without it experience is a
    /// message count, and the way to the top of it is to say nothing at length.
    /// A zero cooldown is a guild that asked for exactly that.
    fn xp_off_cooldown(&self, guild_id: Id, pubkey: &str, cooldown_secs: u32) -> bool {
        if cooldown_secs == 0 {
            return true;
        }
        let guild = self.xp_cooldown.entry(guild_id).or_default();
        mark_if_off_cooldown(
            &guild,
            pubkey,
            std::time::Duration::from_secs(cooldown_secs as u64),
        )
    }

    pub fn leveling_of(&self, guild_id: Id) -> crate::protocol::Leveling {
        self.guilds
            .get(&guild_id)
            .map(|g| g.leveling.clone())
            .unwrap_or_default()
    }

    /// `Some` on every point actually awarded, not only on a level change: the
    /// bar the client draws under a level is fed by this and nothing else.
    ///
    /// `channel_id` names where it was earned, so a guild that restricts which
    /// channels pay out can be honoured — voice minutes name theirs too.
    pub async fn award_xp(
        &self,
        guild_id: Id,
        channel_id: Option<Id>,
        pubkey: &str,
        action: crate::protocol::XpAction,
    ) -> Option<Member> {
        let _write = self.durable_writes.lock().await;
        let rules = self.leveling_of(guild_id);
        if !rules.enabled {
            return None;
        }
        let amount = rules.amount_for(action);
        if amount == 0 {
            return None;
        }
        if let Some(cid) = channel_id
            && !rules.channel_earns(cid)
        {
            return None;
        }
        // Voice is already once a minute by the shape of the sweep; charging it
        // a cooldown as well could only cancel awards the guild asked for.
        let cooldown = match action {
            crate::protocol::XpAction::VoiceMinute => 0,
            _ => rules.cooldown_secs,
        };
        if !self.xp_off_cooldown(guild_id, pubkey, cooldown) {
            return None;
        }
        let new_xp = self.xp_of(guild_id, pubkey).saturating_add(amount as u64);
        durable(
            self.store.upsert_guild_xp(guild_id, pubkey, new_xp).await,
            "xp",
        )
        .ok()?;
        self.xp
            .entry(guild_id)
            .or_default()
            .insert(pubkey.to_string(), new_xp);
        let member = self
            .members
            .get(&guild_id)
            .and_then(|gm| gm.get(pubkey).map(|m| m.clone()))?;
        Some(self.stamp_xp(member))
    }

    /// The one way XP moves other than `award_xp`, and like it never for a bot.
    pub async fn adjust_xp(
        &self,
        guild_id: Id,
        target_pubkey: &str,
        change: crate::protocol::XpChange,
        by_pubkey: &str,
    ) -> Result<Member, String> {
        let _write = self.durable_writes.lock().await;
        self.require_acting(guild_id, by_pubkey, Permission::ManageXp)?;
        let member = self
            .members
            .get(&guild_id)
            .and_then(|gm| gm.get(target_pubkey).map(|m| m.clone()))
            .ok_or_else(|| "that user isn't a member of this guild".to_string())?;
        if member.bot {
            return Err("bots don't earn experience".into());
        }
        let xp = match change {
            crate::protocol::XpChange::Set(xp) => xp,
            crate::protocol::XpChange::Add(delta) => self
                .xp_of(guild_id, target_pubkey)
                .saturating_add_signed(delta),
        };
        durable(
            self.store
                .upsert_guild_xp(guild_id, target_pubkey, xp)
                .await,
            "xp",
        )?;
        self.xp
            .entry(guild_id)
            .or_default()
            .insert(target_pubkey.to_string(), xp);
        Ok(self.stamp_xp(member))
    }

    pub async fn set_guild_leveling(
        &self,
        guild_id: Id,
        leveling: crate::protocol::Leveling,
        by_pubkey: &str,
    ) -> Result<Guild, String> {
        let _write = self.durable_writes.lock().await;
        self.require_permission(guild_id, by_pubkey, Permission::ManageGuild)?;
        let leveling = crate::protocol::sanitize_leveling(leveling);
        let updated = {
            let mut guild = self
                .guilds
                .get(&guild_id)
                .map(|v| v.clone())
                .ok_or_else(|| "unknown guild".to_string())?;
            guild.leveling = leveling;
            guild.clone()
        };
        durable(self.store.upsert_guild(&updated).await, "guild leveling")?;
        self.guilds.insert(guild_id, updated.clone());
        Ok(updated)
    }

    /// One tick of voice experience for everyone currently in a voice channel,
    /// paired with the guild that has to be told about each.
    pub async fn award_voice_minute(&self) -> Vec<(Id, Member)> {
        let in_voice: Vec<(Id, Id, String)> = self
            .voice_states
            .iter()
            .filter_map(|v| {
                v.channel_id
                    .map(|cid| (v.guild_id, cid, v.user_pubkey.clone()))
            })
            .collect();
        let mut moved = Vec::new();
        for (guild_id, channel_id, pubkey) in in_voice {
            if let Some(member) = self
                .award_xp(
                    guild_id,
                    Some(channel_id),
                    &pubkey,
                    crate::protocol::XpAction::VoiceMinute,
                )
                .await
            {
                moved.push((guild_id, member));
            }
        }
        moved
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

    /// For the few commands a bot may also send: roles never apply to a bot,
    /// so what it may do is what its install was granted.
    pub fn acting_permissions(
        &self,
        guild_id: Id,
        pubkey: &str,
    ) -> std::collections::HashSet<Permission> {
        match self.bot_install(guild_id, pubkey) {
            Some(install) if !self.is_owner(guild_id, pubkey) => {
                install.permissions.into_iter().collect()
            }
            _ => self.effective_permissions(guild_id, pubkey),
        }
    }

    /// A bot is a way to act, so the powers over other members reach one only
    /// from someone who holds them; otherwise Manage guild could launder them.
    pub(crate) fn may_grant_to_bot(
        &self,
        guild_id: Id,
        by_pubkey: &str,
        perms: &[Permission],
    ) -> Result<(), String> {
        let mine = self.effective_permissions(guild_id, by_pubkey);
        match perms.iter().find(|p| {
            matches!(p, Permission::ManageRoles | Permission::ManageXp) && !mine.contains(p)
        }) {
            Some(p) => Err(format!(
                "you can't grant {} to an app — you don't hold it yourself",
                serde_variant_name(*p)
            )),
            None => Ok(()),
        }
    }

    fn require_acting(&self, guild_id: Id, pubkey: &str, perm: Permission) -> Result<(), String> {
        if !self.guilds.contains_key(&guild_id) {
            return Err("unknown guild".into());
        }
        if self.acting_permissions(guild_id, pubkey).contains(&perm) {
            Ok(())
        } else {
            Err(format!(
                "you need the {} permission to do that here",
                serde_variant_name(perm)
            ))
        }
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
        let _write = self.durable_writes.lock().await;
        self.require_permission(guild_id, by_pubkey, Permission::ManageGuild)?;
        let updated = {
            let mut guild = self
                .guilds
                .get(&guild_id)
                .map(|v| v.clone())
                .ok_or_else(|| "unknown guild".to_string())?;
            guild.accent = accent;
            guild.clone()
        };
        durable(self.store.upsert_guild(&updated).await, "guild accent")?;
        self.guilds.insert(guild_id, updated.clone());
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

    pub fn update_screen_watching(&self, pubkey: &str, sharers: Vec<String>) -> Option<VoiceState> {
        let channel = self.voice_states.get(pubkey)?.channel_id?;
        let active = self.screen_sharers_in(channel);
        let mut sharers: Vec<_> = sharers
            .into_iter()
            .filter(|key| key != pubkey && active.contains(key))
            .collect();
        sharers.sort();
        sharers.dedup();
        let mut entry = self.voice_states.get_mut(pubkey)?;
        if entry.channel_id != Some(channel) || entry.screen_watching == sharers {
            return None;
        }
        entry.screen_watching = sharers;
        Some(entry.clone())
    }

    pub fn clear_screen_watchers(&self, sharer: &str) -> Vec<VoiceState> {
        let mut changed = Vec::new();
        for mut entry in self.voice_states.iter_mut() {
            let previous = entry.screen_watching.len();
            entry.screen_watching.retain(|key| key != sharer);
            if entry.screen_watching.len() != previous {
                changed.push(entry.clone());
            }
        }
        changed
    }

    pub fn voice_states_in(&self, guild_id: Id) -> Vec<VoiceState> {
        self.voice_states
            .iter()
            .filter(|v| v.guild_id == guild_id)
            .map(|v| v.value().clone())
            .collect()
    }

    pub fn profiles_snapshot(&self) -> Vec<Profile> {
        let mut out: Vec<Profile> = self
            .profiles
            .iter()
            .map(|p| self.shown_profile(p.value().clone()))
            .collect();
        // An operator who never edited a profile still wears the badge; one
        // who never connected is not named, so a configured key stays private.
        for pk in &self.operators {
            if !self.profiles.contains_key(pk) && self.users.contains_key(pk) {
                out.push(self.shown_profile(Profile {
                    pubkey: pk.clone(),
                    ..Default::default()
                }));
            }
        }
        out
    }

    /// Everyone who shares a guild with this key, themselves included. The
    /// audience for anything that is nobody else's business.
    pub fn peers_of(&self, pubkey: &str) -> Vec<String> {
        let mut peers: Vec<String> = self
            .members
            .iter()
            .filter(|e| e.value().contains_key(pubkey))
            .flat_map(|e| {
                e.value()
                    .iter()
                    .map(|m| m.key().clone())
                    .collect::<Vec<_>>()
            })
            .collect();
        peers.sort_unstable();
        peers.dedup();
        peers
    }

    pub fn set_activity(
        &self,
        pubkey: &str,
        activity: Option<crate::protocol::Activity>,
    ) -> crate::protocol::UserActivity {
        let activity = activity.and_then(crate::protocol::sanitize_activity);
        match &activity {
            Some(a) => {
                self.activities.insert(pubkey.to_string(), a.clone());
            }
            None => {
                self.activities.remove(pubkey);
            }
        }
        crate::protocol::UserActivity {
            pubkey: pubkey.to_string(),
            activity,
        }
    }

    /// `true` when there was one to clear, so a disconnect only tells the guild
    /// about a player who was actually playing.
    pub fn clear_activity(&self, pubkey: &str) -> bool {
        self.activities.remove(pubkey).is_some()
    }

    fn activities_of(&self, pubkeys: &[String]) -> Vec<crate::protocol::UserActivity> {
        pubkeys
            .iter()
            .filter_map(|pk| {
                self.activities
                    .get(pk)
                    .map(|a| crate::protocol::UserActivity {
                        pubkey: pk.clone(),
                        activity: Some(a.clone()),
                    })
            })
            .collect()
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
    ) -> Result<(Guild, Vec<Channel>, Member, Vec<Role>), String> {
        let _write = self.durable_writes.lock().await;
        let owned = self
            .guilds
            .iter()
            .filter(|g| g.owner_pubkey == creator.pubkey)
            .count();
        if owned >= MAX_GUILDS_PER_OWNER {
            return Err(format!(
                "you already own {MAX_GUILDS_PER_OWNER} guilds on this server"
            ));
        }
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
            leveling: Default::default(),
        };

        let mut channels = Vec::new();
        for (pos, (cname, kind, read_only)) in spec.channels.iter().enumerate() {
            channels.push(Channel {
                divider: Default::default(),
                id: Uuid::new_v4(),
                guild_id: gid,
                name: (*cname).into(),
                kind: *kind,
                topic: None,
                read_only: *read_only,
                slowmode_secs: 0,
                markdown: false,
                position: pos as u32,
                access: None,
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

        let member = Member {
            user: creator.clone(),
            guild_id: gid,
            online: true,
            bot: false,
            roles: Vec::new(),
            xp: 0,
            look: Default::default(),
        };
        durable(
            self.store
                .create_guild(&guild, &channels, &roles, Some(&member))
                .await,
            "guild create",
        )?;
        self.guilds.insert(gid, guild.clone());
        self.channels_by_guild
            .insert(gid, channels.iter().map(|c| c.id).collect());
        for ch in &channels {
            self.channels.insert(ch.id, ch.clone());
        }
        if !roles.is_empty() {
            self.roles.insert(gid, roles.clone());
        }

        self.members
            .entry(gid)
            .or_default()
            .insert(creator.pubkey.clone(), member.clone());

        Ok((guild, channels, member, roles))
    }

    fn default_guild_slot(&self) -> std::sync::MutexGuard<'_, Option<Id>> {
        self.default_guild
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn default_guild(&self) -> Option<Id> {
        *self.default_guild_slot()
    }

    pub async fn set_default_guild(
        &self,
        guild_id: Option<Id>,
        by_pubkey: &str,
    ) -> Result<(), String> {
        if !self.operators.contains(by_pubkey) {
            return Err("only a server operator can choose the default guild".into());
        }
        let _write = self.durable_writes.lock().await;
        if let Some(gid) = guild_id {
            let visibility = self.guilds.get(&gid).map(|g| g.visibility);
            match visibility {
                None => return Err("unknown guild".into()),
                Some(crate::protocol::GuildVisibility::Private) => {
                    return Err("only a public guild can be the default".into());
                }
                Some(crate::protocol::GuildVisibility::Public) => {}
            }
        } else if !self.guilds.iter().any(|g| g.owner_pubkey.is_empty()) {
            return Err(
                "no system guild is left for newcomers; choose another default instead of none"
                    .into(),
            );
        }
        durable(
            self.store.set_default_guild(guild_id).await,
            "default guild",
        )?;
        *self.default_guild_slot() = guild_id;
        Ok(())
    }

    /// Once per person per default guild, so leaving it or being kicked sticks.
    /// Returns a gated guild, whose challenge the connection must send itself.
    pub async fn offer_default_guild(&self, user: &User) -> Result<Option<Id>, String> {
        let Some(gid) = self.default_guild() else {
            return Ok(None);
        };
        let guild = self
            .guilds
            .get(&gid)
            .map(|g| (g.visibility, g.join_gate, g.panic_mode));
        // A lockdown defers the offer rather than spending it.
        let Some((crate::protocol::GuildVisibility::Public, gate, false)) = guild else {
            return Ok(None);
        };
        if self.is_banned(gid, &user.pubkey) {
            return Ok(None);
        }
        let _write = self.durable_writes.lock().await;
        if durable(
            self.store.default_guild_offered(gid, &user.pubkey).await,
            "default guild offer",
        )? {
            return Ok(None);
        }
        let member = self.is_guild_member(gid, &user.pubkey);
        if !member && gate == crate::protocol::JoinGate::Open {
            self.save_member(gid, user, None).await?;
        }
        durable(
            self.store
                .record_default_guild_offer(gid, &user.pubkey)
                .await,
            "default guild offer",
        )?;
        Ok((!member && gate != crate::protocol::JoinGate::Open).then_some(gid))
    }

    pub async fn snapshot_for(&self, user: &User) -> Result<ServerMessage, String> {
        if self.default_guild().is_none() {
            let system_guilds: Vec<Id> = self
                .guilds
                .iter()
                .filter(|g| g.owner_pubkey.is_empty())
                .map(|g| g.id)
                .collect();
            for gid in system_guilds {
                self.add_member(gid, user).await?;
            }
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
        let mut channels: Vec<Channel> = self
            .channels
            .iter()
            .filter(|c| my_guild_ids.contains(&c.guild_id))
            .map(|c| c.clone())
            .collect();
        channels.retain(|c| self.can_see_channel(&user.pubkey, c));
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
            .collect::<Vec<_>>()
            .into_iter()
            .map(|v| self.voice_state_seen_by(&user.pubkey, v))
            .collect();

        let (catalog, _) = self.guild_catalog_page(0, CATALOG_FIRST_PAGE);
        let profiles = self.profiles_snapshot();
        let roles = self.roles_for_guilds(&my_guild_ids);
        let emojis = self.emojis_for_guilds(&my_guild_ids);
        let sounds = self.sounds_for_guilds(&my_guild_ids);
        let seen: Vec<String> = members.iter().map(|m| m.user.pubkey.clone()).collect();
        let activities = self.activities_of(&seen);
        let bot_commands = self.commands_for_guilds(&my_guild_ids);

        Ok(ServerMessage::Ready {
            chat_tools: true,
            user: user.clone(),
            guilds,
            channels,
            members,
            voice_states,
            catalog,
            profiles,
            roles,
            emojis,
            sounds,
            activities,
            bot_commands,
            operator: self.operators.contains(&user.pubkey),
            default_guild: self.default_guild(),
        })
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
        self.require_acting(guild_id, by_pubkey, Permission::ManageRoles)?;
        if self.is_owner(guild_id, by_pubkey) {
            return Ok(());
        }
        let mine = self.acting_permissions(guild_id, by_pubkey);
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
        let _write = self.durable_writes.lock().await;
        let permissions = unique(permissions);
        self.authorize_role_touch(guild_id, by_pubkey, &[&permissions])?;
        let (name, color) = Self::sanitize_role(name, color)?;
        let role = {
            let roles = self.guild_roles(guild_id);
            if roles.len() >= Self::MAX_ROLES_PER_GUILD {
                return Err("role limit reached for this guild".into());
            }
            // Past the highest, not the count: after a deletion the count is
            // a position some role still holds.
            let position = roles.iter().map(|r| r.position + 1).max().unwrap_or(0);
            Role {
                id: Uuid::new_v4(),
                guild_id,
                name,
                color,
                permissions,
                position,
            }
        };
        durable(self.store.upsert_role(&role).await, "role create")?;
        self.roles.entry(guild_id).or_default().push(role.clone());
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
        let _write = self.durable_writes.lock().await;
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
            let roles = self.roles.get(&guild_id).ok_or("unknown role")?;
            let mut role = roles
                .iter()
                .find(|r| r.id == role_id)
                .cloned()
                .ok_or("unknown role")?;
            role.name = name;
            role.color = color;
            role.permissions = permissions;
            role.clone()
        };
        durable(self.store.upsert_role(&updated).await, "role update")?;
        if let Some(mut roles) = self.roles.get_mut(&guild_id)
            && let Some(role) = roles.iter_mut().find(|r| r.id == role_id)
        {
            *role = updated.clone();
        }
        Ok(updated)
    }

    /// A position only decides display order, so moving a role grants nothing
    /// and needs no more than Manage roles, even for an owner-only role.
    pub async fn reorder_roles(
        &self,
        guild_id: Id,
        order: &[Id],
        by_pubkey: &str,
    ) -> Result<Vec<Role>, String> {
        let _write = self.durable_writes.lock().await;
        self.require_permission(guild_id, by_pubkey, Permission::ManageRoles)?;
        let changed = {
            let mut roles = self
                .roles
                .get(&guild_id)
                .map(|r| r.clone())
                .ok_or("this guild has no roles")?;
            let mut wanted: Vec<Id> = order.to_vec();
            wanted.sort_unstable();
            wanted.dedup();
            let mut have: Vec<Id> = roles.iter().map(|r| r.id).collect();
            have.sort_unstable();
            if wanted != have || order.len() != have.len() {
                return Err("the role list changed — reopen it and try again".into());
            }
            let mut changed = Vec::new();
            for role in roles.iter_mut() {
                let position = order.iter().position(|id| *id == role.id).unwrap_or(0) as u32;
                if role.position != position {
                    role.position = position;
                    changed.push(role.clone());
                }
            }
            changed
        };
        durable(self.store.reorder_roles(&changed).await, "role position")?;
        if let Some(mut roles) = self.roles.get_mut(&guild_id) {
            for updated in changed {
                if let Some(role) = roles.iter_mut().find(|r| r.id == updated.id) {
                    role.position = updated.position;
                }
            }
            roles.sort_by_key(|r| r.position);
        }
        Ok(self.guild_roles(guild_id))
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
        let _write = self.durable_writes.lock().await;
        self.require_permission(guild_id, by_pubkey, Permission::ManageEmojis)?;
        let shortcode = shortcode.trim().trim_matches(':').to_ascii_lowercase();
        if !valid_shortcode(&shortcode) {
            return Err("shortcode must be 2-32 chars of a-z, 0-9 or _".into());
        }
        let emoji = {
            let list = self
                .emojis
                .get(&guild_id)
                .map(|l| l.clone())
                .unwrap_or_default();
            if list.len() >= MAX_EMOJIS_PER_GUILD {
                return Err(format!(
                    "emoji limit reached ({MAX_EMOJIS_PER_GUILD} per guild)"
                ));
            }
            if list.iter().any(|e| e.shortcode == shortcode) {
                return Err(format!(":{shortcode}: already exists in this guild"));
            }
            GuildEmoji {
                id: Uuid::new_v4(),
                guild_id,
                shortcode,
                image,
                added_by: by_pubkey.to_string(),
                created_ms: chrono::Utc::now().timestamp_millis(),
            }
        };
        durable(self.store.upsert_emoji(&emoji).await, "emoji create")?;
        self.emojis.entry(guild_id).or_default().push(emoji.clone());
        Ok(emoji)
    }

    pub async fn rename_emoji(
        &self,
        guild_id: Id,
        emoji_id: Id,
        shortcode: &str,
        by_pubkey: &str,
    ) -> Result<GuildEmoji, String> {
        let _write = self.durable_writes.lock().await;
        self.require_permission(guild_id, by_pubkey, Permission::ManageEmojis)?;
        let shortcode = shortcode.trim().trim_matches(':').to_ascii_lowercase();
        if !valid_shortcode(&shortcode) {
            return Err("shortcode must be 2-32 chars of a-z, 0-9 or _".into());
        }
        let updated = {
            let list = self
                .emojis
                .get(&guild_id)
                .map(|v| v.clone())
                .ok_or("unknown emoji")?;
            if list
                .iter()
                .any(|e| e.shortcode == shortcode && e.id != emoji_id)
            {
                return Err(format!(":{shortcode}: already exists in this guild"));
            }
            let mut e = list
                .iter()
                .find(|e| e.id == emoji_id)
                .cloned()
                .ok_or("unknown emoji")?;
            e.shortcode = shortcode;
            e.clone()
        };
        durable(self.store.upsert_emoji(&updated).await, "emoji rename")?;
        if let Some(mut list) = self.emojis.get_mut(&guild_id)
            && let Some(item) = list.iter_mut().find(|item| item.id == updated.id)
        {
            *item = updated.clone();
        }
        Ok(updated)
    }

    pub async fn delete_emoji(
        &self,
        guild_id: Id,
        emoji_id: Id,
        by_pubkey: &str,
    ) -> Result<(), String> {
        let _write = self.durable_writes.lock().await;
        self.require_permission(guild_id, by_pubkey, Permission::ManageEmojis)?;
        let existed = self
            .emojis
            .get(&guild_id)
            .is_some_and(|list| list.iter().any(|item| item.id == emoji_id));
        if !existed {
            return Err("unknown emoji".into());
        }
        durable(self.store.delete_emoji(emoji_id).await, "emoji delete")?;
        if let Some(mut list) = self.emojis.get_mut(&guild_id) {
            list.retain(|item| item.id != emoji_id);
        }
        Ok(())
    }

    pub fn sounds_of(&self, guild_id: Id) -> Vec<GuildSound> {
        self.sounds
            .get(&guild_id)
            .map(|s| s.clone())
            .unwrap_or_default()
    }

    pub fn sounds_for_guilds(&self, guild_ids: &[Id]) -> Vec<GuildSound> {
        guild_ids.iter().flat_map(|g| self.sounds_of(*g)).collect()
    }

    pub fn sound_in_guild(&self, guild_id: Id, sound_id: Id) -> bool {
        self.sounds
            .get(&guild_id)
            .is_some_and(|list| list.iter().any(|s| s.id == sound_id))
    }

    /// `audio` is already stored; the gateway checks the permission before
    /// storing it too, so a refused upload never reaches the disk.
    pub async fn create_sound(
        &self,
        guild_id: Id,
        name: &str,
        audio: String,
        by_pubkey: &str,
    ) -> Result<GuildSound, String> {
        let _write = self.durable_writes.lock().await;
        self.require_permission(guild_id, by_pubkey, Permission::ManageGuild)?;
        let name = crate::protocol::sound_name(name)?;
        let sound = {
            let list = self
                .sounds
                .get(&guild_id)
                .map(|l| l.clone())
                .unwrap_or_default();
            if list.len() >= MAX_SOUNDS_PER_GUILD {
                return Err(format!(
                    "sound limit reached ({MAX_SOUNDS_PER_GUILD} per guild)"
                ));
            }
            if list.iter().any(|s| s.name.eq_ignore_ascii_case(&name)) {
                return Err(format!("a sound called \"{name}\" already exists"));
            }
            GuildSound {
                id: Uuid::new_v4(),
                guild_id,
                name,
                audio,
                added_by: by_pubkey.to_string(),
                created_ms: chrono::Utc::now().timestamp_millis(),
            }
        };
        durable(self.store.upsert_sound(&sound).await, "sound create")?;
        self.sounds.entry(guild_id).or_default().push(sound.clone());
        Ok(sound)
    }

    pub async fn rename_sound(
        &self,
        guild_id: Id,
        sound_id: Id,
        name: &str,
        by_pubkey: &str,
    ) -> Result<GuildSound, String> {
        let _write = self.durable_writes.lock().await;
        self.require_permission(guild_id, by_pubkey, Permission::ManageGuild)?;
        let name = crate::protocol::sound_name(name)?;
        let updated = {
            let list = self
                .sounds
                .get(&guild_id)
                .map(|v| v.clone())
                .ok_or("unknown sound")?;
            if list
                .iter()
                .any(|s| s.name.eq_ignore_ascii_case(&name) && s.id != sound_id)
            {
                return Err(format!("a sound called \"{name}\" already exists"));
            }
            let mut s = list
                .iter()
                .find(|s| s.id == sound_id)
                .cloned()
                .ok_or("unknown sound")?;
            s.name = name;
            s.clone()
        };
        durable(self.store.upsert_sound(&updated).await, "sound rename")?;
        if let Some(mut list) = self.sounds.get_mut(&guild_id)
            && let Some(item) = list.iter_mut().find(|item| item.id == updated.id)
        {
            *item = updated.clone();
        }
        Ok(updated)
    }

    pub async fn delete_sound(
        &self,
        guild_id: Id,
        sound_id: Id,
        by_pubkey: &str,
    ) -> Result<(), String> {
        let _write = self.durable_writes.lock().await;
        self.require_permission(guild_id, by_pubkey, Permission::ManageGuild)?;
        let existed = self
            .sounds
            .get(&guild_id)
            .is_some_and(|list| list.iter().any(|item| item.id == sound_id));
        if !existed {
            return Err("unknown sound".into());
        }
        durable(self.store.delete_sound(sound_id).await, "sound delete")?;
        if let Some(mut list) = self.sounds.get_mut(&guild_id) {
            list.retain(|item| item.id != sound_id);
        }
        Ok(())
    }

    pub async fn delete_role(
        &self,
        guild_id: Id,
        role_id: Id,
        by_pubkey: &str,
    ) -> Result<Vec<Member>, String> {
        let _write = self.durable_writes.lock().await;
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
        let mut changed = Vec::new();
        if let Some(guild_members) = self.members.get(&guild_id) {
            for member in guild_members.iter() {
                let mut m = member.clone();
                if m.roles.contains(&role_id) {
                    m.roles.retain(|r| *r != role_id);
                    changed.push(self.stamp_xp(m.clone()));
                }
            }
        }
        durable(
            self.store
                .delete_role_and_assignments(role_id, &changed)
                .await,
            "role delete",
        )?;
        if let Some(mut roles) = self.roles.get_mut(&guild_id) {
            roles.retain(|r| r.id != role_id);
        }
        if let Some(members) = self.members.get(&guild_id) {
            for updated in &changed {
                if let Some(mut member) = members.get_mut(&updated.user.pubkey) {
                    member.roles = updated.roles.clone();
                }
            }
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
        let _write = self.durable_writes.lock().await;
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
                .get(target_pubkey)
                .map(|m| m.clone())
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
        durable(self.store.upsert_member(&updated).await, "member role")?;
        if let Some(members) = self.members.get(&guild_id)
            && let Some(mut member) = members.get_mut(target_pubkey)
        {
            member.roles = updated.roles.clone();
        }
        Ok(self.stamp_xp(updated))
    }

    pub async fn add_member(&self, guild_id: Id, user: &User) -> Result<Member, String> {
        let _write = self.durable_writes.lock().await;
        self.save_member(guild_id, user, None).await
    }

    async fn save_member(
        &self,
        guild_id: Id,
        user: &User,
        invite: Option<&str>,
    ) -> Result<Member, String> {
        let mut member = self
            .members
            .get(&guild_id)
            .and_then(|members| members.get(&user.pubkey).map(|m| m.clone()))
            .unwrap_or_else(|| Member {
                user: user.clone(),
                guild_id,
                online: true,
                bot: false,
                roles: Vec::new(),
                xp: 0,
                look: Default::default(),
            });
        member.online = true;
        durable(self.store.admit_member(&member, invite).await, "member add")?;
        self.members
            .entry(guild_id)
            .or_default()
            .insert(user.pubkey.clone(), member.clone());
        if let Some(code) = invite
            && let Some(mut entry) = self.invites.get_mut(code)
        {
            entry.uses += 1;
        }
        Ok(self.stamp_xp(member))
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
        let limit = if limit == 0 {
            CATALOG_FIRST_PAGE
        } else {
            limit.min(500)
        };
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
        let _write = self.durable_writes.lock().await;
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
        self.save_member(guild_id, user, None).await?;
        Ok(self.admit_member(guild, user).await)
    }

    pub async fn join_by_invite(
        &self,
        code: &str,
        user: &User,
    ) -> Result<(Guild, Vec<Channel>, Vec<Member>, Vec<Role>), String> {
        let _write = self.durable_writes.lock().await;
        let code = code.trim();
        let guild_id = {
            let entry = self
                .invites
                .get(code)
                .ok_or_else(|| "unknown or expired invite code".to_string())?;
            if !entry.is_live(now_ms()) {
                return Err("unknown or expired invite code".into());
            }
            entry.guild_id
        };
        let guild = self
            .guilds
            .get(&guild_id)
            .map(|g| g.clone())
            .ok_or_else(|| "unknown or expired invite code".to_string())?;
        if self.is_banned(guild_id, &user.pubkey) {
            return Err("you are banned from this guild".into());
        }
        self.save_member(guild_id, user, Some(code)).await?;
        Ok(self.admit_member(guild, user).await)
    }

    async fn admit_member(
        &self,
        guild: Guild,
        _user: &User,
    ) -> (Guild, Vec<Channel>, Vec<Member>, Vec<Role>) {
        let guild_id = guild.id;
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
        let _write = self.durable_writes.lock().await;
        self.require_permission(guild_id, by_pubkey, Permission::ManageGuild)?;
        let updated = {
            let mut guild = self
                .guilds
                .get(&guild_id)
                .map(|v| v.clone())
                .ok_or_else(|| "unknown guild".to_string())?;
            guild.visibility = visibility;
            guild.clone()
        };
        durable(self.store.upsert_guild(&updated).await, "guild visibility")?;
        self.guilds.insert(guild_id, updated.clone());
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
        let _write = self.durable_writes.lock().await;
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
        durable(
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
        )?;
        if let Some((_, old)) = self.invite_by_guild.remove(&guild_id) {
            self.invites.remove(&old);
        }
        self.invites.insert(code.clone(), invite.clone());
        self.invite_by_guild.insert(guild_id, code.clone());

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
            return Err("the owner is beyond a moderator's reach".into());
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
        let _write = self.durable_writes.lock().await;
        self.require_permission(guild_id, by_pubkey, Permission::KickMembers)?;
        self.validate_moderation_target(guild_id, target_pubkey, by_pubkey)?;
        if !self.is_guild_member(guild_id, target_pubkey) {
            return Err("that user isn't a member of this guild".into());
        }
        durable(
            self.store.delete_member(guild_id, target_pubkey).await,
            "member kick",
        )?;
        let cleared = self.remove_membership(guild_id, target_pubkey);
        Ok(cleared)
    }

    pub fn disconnect_from_voice(
        &self,
        guild_id: Id,
        target_pubkey: &str,
        by_pubkey: &str,
    ) -> Result<VoiceState, String> {
        self.require_permission(guild_id, by_pubkey, Permission::DisconnectMembers)?;
        if target_pubkey == by_pubkey {
            return Err("leave the call instead".into());
        }
        self.validate_moderation_target(guild_id, target_pubkey, by_pubkey)?;
        let in_a_call_here = self
            .voice_states
            .get(target_pubkey)
            .is_some_and(|v| v.guild_id == guild_id && v.channel_id.is_some());
        if !in_a_call_here {
            return Err("they aren't in a voice channel here".into());
        }
        self.clear_voice(target_pubkey)
            .ok_or_else(|| "they aren't in a voice channel here".into())
    }

    pub fn move_voice_member(
        &self,
        target_pubkey: &str,
        by_pubkey: &str,
        from_channel_id: Id,
        channel_id: Id,
    ) -> Result<(VoiceState, VoiceState, u64), String> {
        let destination = self.channel(channel_id).ok_or("not a voice channel")?;
        let guild_id = destination.guild_id;
        self.require_permission(guild_id, by_pubkey, Permission::MoveMembers)?;
        self.validate_moderation_target(guild_id, target_pubkey, by_pubkey)?;
        if destination.kind != crate::protocol::ChannelKind::Voice
            || from_channel_id == channel_id
            || !self.is_guild_member(guild_id, target_pubkey)
            || !self.can_see_channel(by_pubkey, &destination)
            || !self.can_see_channel(target_pubkey, &destination)
        {
            return Err("not an accessible voice destination".into());
        }
        let source = self
            .channel(from_channel_id)
            .ok_or("unknown source channel")?;
        if source.guild_id != guild_id || !self.can_see_channel(by_pubkey, &source) {
            return Err("voice moves must stay in the same guild".into());
        }
        let owners = self.voice_owners.lock().unwrap_or_else(|e| e.into_inner());
        let conn_id = *owners
            .get(target_pubkey)
            .ok_or("they aren't in a voice channel here")?;
        if !self.conns.contains_key(&conn_id) {
            return Err("their voice connection is unavailable".into());
        }
        let previous = self
            .voice_states
            .get(target_pubkey)
            .map(|v| v.clone())
            .ok_or("they aren't in a voice channel here")?;
        if previous.channel_id != Some(from_channel_id) || previous.guild_id != guild_id {
            return Err("their voice channel changed before the move".into());
        }
        let joined =
            self.set_voice_channel_unlocked(target_pubkey, guild_id, Some(channel_id), None);
        Ok((previous, joined, conn_id))
    }

    pub async fn ban_member(
        &self,
        guild_id: Id,
        target_pubkey: &str,
        by_pubkey: &str,
    ) -> Result<Option<VoiceState>, String> {
        let _write = self.durable_writes.lock().await;
        self.require_permission(guild_id, by_pubkey, Permission::BanMembers)?;
        self.validate_moderation_target(guild_id, target_pubkey, by_pubkey)?;
        durable(
            self.store.ban_member(guild_id, target_pubkey).await,
            "member ban",
        )?;
        self.bans
            .entry(guild_id)
            .or_default()
            .insert(target_pubkey.to_string());
        let cleared = self.remove_membership(guild_id, target_pubkey);
        Ok(cleared)
    }

    pub async fn unban_member(
        &self,
        guild_id: Id,
        target_pubkey: &str,
        by_pubkey: &str,
    ) -> Result<(), String> {
        let _write = self.durable_writes.lock().await;
        self.require_permission(guild_id, by_pubkey, Permission::BanMembers)?;
        let removed = self
            .bans
            .get(&guild_id)
            .map(|b| b.contains(target_pubkey))
            .unwrap_or(false);
        if removed {
            durable(
                self.store.delete_ban(guild_id, target_pubkey).await,
                "unban",
            )?;
            if let Some(mut bans) = self.bans.get_mut(&guild_id) {
                bans.remove(target_pubkey);
            }
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
        let _write = self.durable_writes.lock().await;
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
        durable(
            self.store.delete_member(guild_id, pubkey).await,
            "member leave",
        )?;
        let cleared = self.remove_membership(guild_id, pubkey);
        Ok(cleared)
    }

    pub async fn create_channel(
        &self,
        guild_id: Id,
        name: &str,
        kind: crate::protocol::ChannelKind,
        topic: Option<String>,
        divider: crate::protocol::DividerStyle,
        by_pubkey: &str,
    ) -> Result<Channel, String> {
        let _write = self.durable_writes.lock().await;
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
            divider: if kind.is_divider() {
                divider
            } else {
                Default::default()
            },
            id: Uuid::new_v4(),
            guild_id,
            name,
            kind,
            topic: topic
                .map(|t| crate::protocol::sanitize_line(&t, 120))
                .filter(|t| !t.is_empty() && !kind.is_divider()),
            read_only: false,
            slowmode_secs: 0,
            markdown: false,
            position: next_pos,
            access: None,
        };
        durable(self.store.upsert_channel(&channel).await, "channel create")?;
        self.channels.insert(channel.id, channel.clone());
        self.channels_by_guild
            .entry(guild_id)
            .or_default()
            .push(channel.id);
        Ok(channel)
    }

    pub async fn reorder_channels(
        &self,
        guild_id: Id,
        positions: &[(Id, u32)],
        by_pubkey: &str,
    ) -> Result<Vec<Channel>, String> {
        let _write = self.durable_writes.lock().await;
        self.require_permission(guild_id, by_pubkey, Permission::ManageChannels)?;
        for (id, _) in positions {
            if self.channel_guild(*id) != Some(guild_id) {
                return Err("that channel is not in this guild".into());
            }
        }
        let mut updated = Vec::with_capacity(positions.len());
        for (id, position) in positions {
            let Some(mut entry) = self.channels.get(id).map(|c| c.clone()) else {
                continue;
            };
            if entry.position == *position {
                continue;
            }
            entry.position = *position;
            updated.push(entry.clone());
        }
        durable(
            self.store.reorder_channels(&updated).await,
            "channel position",
        )?;
        for channel in &updated {
            self.channels.insert(channel.id, channel.clone());
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
        divider: Option<crate::protocol::DividerStyle>,
        markdown: Option<bool>,
        by_pubkey: &str,
    ) -> Result<Channel, String> {
        let _write = self.durable_writes.lock().await;
        let guild_id = self
            .channel_guild(channel_id)
            .ok_or_else(|| "unknown channel".to_string())?;
        self.require_permission(guild_id, by_pubkey, Permission::ManageChannels)?;
        let name = sanitize_channel_name(name)?;
        let updated = {
            let mut channel = self
                .channels
                .get(&channel_id)
                .map(|v| v.clone())
                .ok_or_else(|| "unknown channel".to_string())?;
            channel.name = name;
            channel.topic = topic
                .map(|t| crate::protocol::sanitize_line(&t, 120))
                .filter(|t| !t.is_empty());
            channel.read_only =
                read_only && matches!(channel.kind, crate::protocol::ChannelKind::Text);
            channel.slowmode_secs = slowmode_secs.min(21_600);
            channel.position = position;
            if let Some(on) = markdown {
                channel.markdown = on && channel.kind == crate::protocol::ChannelKind::Text;
            }
            if channel.kind.is_divider() {
                channel.topic = None;
                channel.divider = divider.unwrap_or(channel.divider);
            }
            channel.clone()
        };
        durable(self.store.upsert_channel(&updated).await, "channel update")?;
        self.channels.insert(channel_id, updated.clone());
        Ok(updated)
    }

    pub async fn delete_channel(
        &self,
        channel_id: Id,
        by_pubkey: &str,
    ) -> Result<(Id, Vec<VoiceState>), String> {
        let _write = self.durable_writes.lock().await;
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
        durable(
            self.store.delete_channel(channel_id).await,
            "channel delete",
        )?;
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
        Ok((guild_id, cleared))
    }

    pub async fn transfer_ownership(
        &self,
        guild_id: Id,
        new_owner_pubkey: &str,
        by_pubkey: &str,
    ) -> Result<Guild, String> {
        let _write = self.durable_writes.lock().await;
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
                .get(&guild_id)
                .map(|g| g.clone())
                .ok_or_else(|| "unknown guild".to_string())?;
            guild.owner_pubkey = new_owner_pubkey.to_string();
            guild.clone()
        };
        durable(
            self.store.upsert_guild(&updated).await,
            "ownership transfer",
        )?;
        self.guilds.insert(guild_id, updated.clone());
        Ok(updated)
    }

    pub async fn set_guild_profile(
        &self,
        guild_id: Id,
        name: Option<String>,
        description: Option<String>,
        icon_image: Option<String>,
        banner: Option<String>,
        by_pubkey: &str,
    ) -> Result<Guild, String> {
        let _write = self.durable_writes.lock().await;
        self.require_permission(guild_id, by_pubkey, Permission::ManageGuild)?;
        let icon_image = icon_image
            .map(|i| self.image_reference(by_pubkey, &i))
            .transpose()
            .map_err(|e| format!("guild icon: {e}"))?;
        let banner = banner
            .map(|b| self.image_reference(by_pubkey, &b))
            .transpose()
            .map_err(|e| format!("guild banner: {e}"))?;
        let description = description
            .map(|d| crate::protocol::sanitize_paragraph(&d, 280))
            .filter(|d| !d.is_empty());
        let updated = {
            let mut guild = self
                .guilds
                .get(&guild_id)
                .map(|v| v.clone())
                .ok_or_else(|| "unknown guild".to_string())?;
            if let Some(name) = name {
                guild.name = name;
            }
            guild.description = description;
            guild.icon_image = icon_image;
            guild.banner = banner;
            guild.clone()
        };
        durable(self.store.upsert_guild(&updated).await, "guild profile")?;
        self.guilds.insert(guild_id, updated.clone());
        Ok(updated)
    }

    /// Runs after the retention sweep, which is what *creates* unreferenced
    /// blobs. A failed query means we do not know what is referenced — so keep.
    pub async fn sweep_media(&self) -> crate::media::SweepReport {
        let _write = self.durable_writes.lock().await;
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
        let _write = self.durable_writes.lock().await;
        self.require_permission(guild_id, by_pubkey, Permission::ManageGuild)?;
        let days = days.map(|d| d.clamp(1, 3650));
        let updated = {
            let mut guild = self
                .guilds
                .get(&guild_id)
                .map(|v| v.clone())
                .ok_or_else(|| "unknown guild".to_string())?;
            guild.retention_days = days;
            guild.clone()
        };
        durable(self.store.upsert_guild(&updated).await, "guild retention")?;
        self.guilds.insert(guild_id, updated.clone());
        Ok(updated)
    }

    pub async fn set_join_gate(
        &self,
        guild_id: Id,
        gate: crate::protocol::JoinGate,
        rules: Option<String>,
        by_pubkey: &str,
    ) -> Result<Guild, String> {
        let _write = self.durable_writes.lock().await;
        self.require_permission(guild_id, by_pubkey, Permission::ManageGuild)?;
        let updated = {
            let mut guild = self
                .guilds
                .get(&guild_id)
                .map(|v| v.clone())
                .ok_or_else(|| "unknown guild".to_string())?;
            guild.join_gate = gate;
            guild.rules = rules.map(|r| crate::protocol::sanitize_paragraph(&r, 4000));
            guild.clone()
        };
        durable(self.store.upsert_guild(&updated).await, "join gate")?;
        self.guilds.insert(guild_id, updated.clone());
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
        let _write = self.durable_writes.lock().await;
        let updated = {
            let mut guild = self
                .guilds
                .get(&guild_id)
                .map(|v| v.clone())
                .ok_or_else(|| "unknown guild".to_string())?;
            guild.panic_mode = on;
            guild.clone()
        };
        durable(self.store.upsert_guild(&updated).await, "panic mode")?;
        self.guilds.insert(guild_id, updated.clone());
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
    ) -> Option<(String, crate::protocol::JoinGate, Option<String>, bool)> {
        self.guilds
            .get(&guild_id)
            .map(|g| (g.name.clone(), g.join_gate, g.rules.clone(), g.panic_mode))
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

    /// Who still writes in a read-only channel. A bot's roles never apply, so its install decides.
    pub fn writes_when_read_only(&self, guild_id: Id, pubkey: &str, is_bot: bool) -> bool {
        let may =
            |p: &Permission| *p == Permission::ManageMessages || *p == Permission::ManageChannels;
        if is_bot {
            self.bot_install(guild_id, pubkey).is_some_and(|i| {
                i.has_permission(Permission::ManageMessages)
                    || i.has_permission(Permission::ManageChannels)
            })
        } else {
            self.effective_permissions(guild_id, pubkey).iter().any(may)
        }
    }

    pub async fn edit_message(
        &self,
        channel_id: Id,
        message_id: Id,
        content: &str,
        by_pubkey: &str,
        is_bot: bool,
    ) -> Result<chrono::DateTime<chrono::Utc>, String> {
        let channel = self
            .channel(channel_id)
            .filter(|c| {
                self.is_guild_member(c.guild_id, by_pubkey) && self.can_see_channel(by_pubkey, c)
            })
            .ok_or("unknown channel")?;
        if channel.read_only && !self.writes_when_read_only(channel.guild_id, by_pubkey, is_bot) {
            return Err("this channel is read-only".into());
        }
        let content = content.trim();
        if content.len() > 2000 {
            return Err("message must be at most 2000 chars".into());
        }
        let author = self
            .store
            .message_author(channel_id, message_id)
            .await
            .map_err(|e| format!("store error: {e}"))?
            .ok_or("unknown message")?;
        if author != by_pubkey {
            return Err("only the author can edit a message".into());
        }
        let now = chrono::Utc::now();
        let edited = self
            .store
            .edit_message(
                channel_id,
                message_id,
                by_pubkey,
                content,
                now.timestamp_millis(),
            )
            .await
            .map_err(|e| format!("store error: {e}"))?;
        if !edited {
            return Err("a message needs text unless it has a picture or file".into());
        }
        Ok(now)
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
        let _write = self.durable_writes.lock().await;
        let owner = self
            .guilds
            .get(&guild_id)
            .map(|g| g.owner_pubkey.clone())
            .ok_or_else(|| "unknown guild".to_string())?;
        let allowed = if owner.is_empty() {
            self.operators.contains(by_pubkey)
        } else {
            owner == by_pubkey
        };
        if !allowed {
            return Err("only the owner can delete this guild".into());
        }
        let default = self.default_guild();
        if default == Some(guild_id) {
            return Err("choose another server default before deleting this guild".into());
        }
        // Without a default, newcomers land in the system guilds — removing one strands them.
        if owner.is_empty() && default.is_none() {
            return Err(
                "make another guild the server default before deleting a system guild".into(),
            );
        }
        durable(self.store.delete_guild(guild_id).await, "guild delete")?;
        let mut default = self.default_guild_slot();
        if *default == Some(guild_id) {
            *default = None;
        }
        drop(default);
        self.guilds.remove(&guild_id);
        self.members.remove(&guild_id);
        self.roles.remove(&guild_id);
        self.sounds.remove(&guild_id);
        self.bans.remove(&guild_id);
        if let Some((_, code)) = self.invite_by_guild.remove(&guild_id) {
            self.invites.remove(&code);
        }
        if let Some((_, channel_ids)) = self.channels_by_guild.remove(&guild_id) {
            for cid in channel_ids {
                self.channels.remove(&cid);
            }
        }
        Ok(())
    }

    /// Rows come back as stored: a `media:` address, never the bytes. A page of
    /// 200 inlined images was 600 MB of JSON for one cheap request.
    pub async fn history(
        &self,
        channel_id: Id,
        limit: u32,
        before_ms: Option<i64>,
    ) -> Vec<Message> {
        match self.store.history(channel_id, limit, before_ms).await {
            Ok(mut messages) => {
                self.mark_bot_authors(channel_id, &mut messages);
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
    ) -> Result<Message, String> {
        self.push_image_message(channel_id, author, content, image, None, reply_to)
            .await
    }

    pub async fn push_image_message(
        &self,
        channel_id: Id,
        author: User,
        content: String,
        image: Option<String>,
        image_name: Option<String>,
        reply_to: Option<Id>,
    ) -> Result<Message, String> {
        let author = self.attributed_author(channel_id, author);
        let guild_id = self
            .channels
            .get(&channel_id)
            .filter(|c| matches!(c.kind, crate::protocol::ChannelKind::Text))
            .map(|c| c.guild_id)
            .ok_or("can't post to this channel")?;
        let (image, image_metadata) = match image {
            Some(img) => {
                let policy = self
                    .store
                    .file_policy(guild_id)
                    .await
                    .map_err(|e| e.to_string())?;
                let limit =
                    (policy.max_bytes as usize).min(crate::protocol::MAX_GUILD_UPLOAD_BYTES);
                let error = || {
                    format!(
                        "image too large: this community's size limit is {} KB",
                        limit / 1000
                    )
                };
                if img.len() > limit.div_ceil(3) * 4 + 128 {
                    return Err(error());
                }
                self.charge_upload(&author.pubkey, img.len() as u64)?;
                let stored = self.media.store_chat_image(&img, limit).map_err(|e| {
                    if e == crate::media::StoreError::ImageTooLarge {
                        error()
                    } else {
                        e.to_string()
                    }
                })?;
                let metadata = crate::media::image_metadata(&img, image_name.as_deref());
                (Some(stored), Some(metadata))
            }
            None => (None, None),
        };
        let reply_ref = match reply_to {
            Some(id) => self.store.reply_ref(channel_id, id).await.unwrap_or(None),
            None => None,
        };
        self.append_message(
            channel_id,
            author,
            content,
            image,
            image_metadata,
            reply_ref,
        )
        .await
    }

    /// One rule for every picture a client may hand the server. A data URL is
    /// stored and replaced by its address; an address must already be on disk.
    /// A link is refused: every viewer's webview would fetch it, handing the
    /// viewer's IP to whoever set it.
    pub fn image_reference(&self, by_pubkey: &str, img: &str) -> Result<String, String> {
        if img.starts_with("media:") && !crate::media::is_image_address(img) {
            return Err("that attachment is not an image".into());
        }
        if let Some(address) = self.media.existing(img) {
            if self
                .media
                .stored_size(&address)
                .is_none_or(|size| size > discordia_protocol::MAX_IMAGE_BYTES as u64)
            {
                return Err("an image must be at most 2 MiB (2,097,152 bytes)".into());
            }
            return Ok(address);
        }
        if img.starts_with("data:image/") {
            if img.len() > MAX_IMAGE_LEN {
                return Err("an image must be at most 2 MiB (2,097,152 bytes)".into());
            }
            return self.store_upload(by_pubkey, img);
        }
        Err("must be an image file (PNG, JPEG, GIF, WebP or AVIF) up to 2 MiB, not a link".into())
    }

    pub fn store_upload(&self, by_pubkey: &str, data_url: &str) -> Result<String, String> {
        self.charge_upload(by_pubkey, data_url.len() as u64)?;
        self.media
            .store_data_url(data_url)
            .map_err(|e| e.to_string())
    }

    pub fn store_sound_upload(&self, by_pubkey: &str, data_url: &str) -> Result<String, String> {
        self.charge_upload(by_pubkey, data_url.len() as u64)?;
        self.media
            .store_sound_data_url(data_url)
            .map_err(|e| e.to_string())
    }

    fn charge_upload(&self, by_pubkey: &str, cost: u64) -> Result<(), String> {
        let now = std::time::Instant::now();
        {
            let mut slot = self
                .upload_budget
                .entry(by_pubkey.to_string())
                .or_insert((now, 0));
            if now.duration_since(slot.0) > UPLOAD_WINDOW {
                *slot = (now, 0);
            }
            if slot.1.saturating_add(cost) > UPLOAD_BUDGET_BYTES {
                return Err(format!(
                    "you have uploaded {} MB today, which is this server's daily limit",
                    UPLOAD_BUDGET_BYTES / (1024 * 1024)
                ));
            }
            slot.1 += cost;
        }
        Ok(())
    }

    /// Rows written before images had addresses hold the bytes themselves;
    /// moving them out at load is what makes every later snapshot small.
    async fn detach_inline_images(&self) {
        let detach = |img: Option<String>| {
            img.map(|i| match i.starts_with("data:") {
                true => self.media.store_data_url(&i).unwrap_or(i),
                false => i,
            })
        };
        let inline = |img: &Option<String>| img.as_deref().is_some_and(|i| i.starts_with("data:"));

        let profiles: Vec<Profile> = self
            .profiles
            .iter()
            .filter(|p| inline(&p.avatar) || inline(&p.banner))
            .map(|p| p.value().clone())
            .collect();
        for mut p in profiles {
            p.avatar = detach(p.avatar);
            p.banner = detach(p.banner);
            self.profiles.insert(p.pubkey.clone(), p.clone());
            persist(self.store.upsert_profile(&p).await, "profile image detach");
        }

        let guilds: Vec<Guild> = self
            .guilds
            .iter()
            .filter(|g| inline(&g.icon_image) || inline(&g.banner))
            .map(|g| g.value().clone())
            .collect();
        for mut g in guilds {
            g.icon_image = detach(g.icon_image);
            g.banner = detach(g.banner);
            self.guilds.insert(g.id, g.clone());
            persist(self.store.upsert_guild(&g).await, "guild image detach");
        }
    }

    /// Installs are the record, not the row: an uninstalled bot's old messages
    /// stop counting as a bot's, the same way its name stops being overridden.
    pub fn mark_bot_authors(&self, channel_id: Id, messages: &mut [Message]) {
        let Some(guild_id) = self.channel_guild(channel_id) else {
            return;
        };
        let bots: std::collections::HashSet<String> = self
            .guild_installs(guild_id)
            .into_iter()
            .map(|i| i.bot_pubkey)
            .collect();
        for m in messages {
            m.author_bot = bots.contains(&m.author.pubkey);
        }
    }

    fn is_installed_bot(&self, channel_id: Id, pubkey: &str) -> bool {
        self.channel_guild(channel_id)
            .is_some_and(|gid| self.bot_install(gid, pubkey).is_some())
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
        image_metadata: Option<crate::protocol::ImageMetadata>,
        reply_to: Option<ReplyRef>,
    ) -> Result<Message, String> {
        let message = Message {
            image_metadata,
            attachment: None,
            pinned: false,
            id: Uuid::new_v4(),
            channel_id,
            author_bot: self.is_installed_bot(channel_id, &author.pubkey),
            edited_at: None,
            author,
            content,
            image,
            reactions: Vec::new(),
            reply_to,
            created_at: chrono::Utc::now(),
        };
        durable(self.store.insert_message(&message).await, "message insert")?;
        Ok(message)
    }

    pub async fn rename_user(&self, pubkey: &str, username: &str) -> Result<Vec<Member>, String> {
        let _write = self.durable_writes.lock().await;
        let user = User {
            pubkey: pubkey.to_string(),
            username: username.to_string(),
        };
        durable(self.store.rename_user(&user).await, "user rename")?;
        self.users.insert(pubkey.to_owned(), user);

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
        Ok(changed)
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
        preferences: Option<crate::protocol::VoicePreferences>,
    ) -> VoiceState {
        let mut owners = self.voice_owners.lock().unwrap_or_else(|e| e.into_inner());
        owners.remove(user_pubkey);
        self.set_voice_channel_unlocked(user_pubkey, guild_id, channel_id, preferences)
    }

    pub fn set_voice_channel_owned(
        &self,
        conn_id: u64,
        user_pubkey: &str,
        guild_id: Id,
        channel_id: Id,
        preferences: Option<crate::protocol::VoicePreferences>,
    ) -> VoiceState {
        let mut owners = self.voice_owners.lock().unwrap_or_else(|e| e.into_inner());
        owners.insert(user_pubkey.to_string(), conn_id);
        self.set_voice_channel_unlocked(user_pubkey, guild_id, Some(channel_id), preferences)
    }

    pub fn resume_voice_session(&self, user_pubkey: &str, conn_id: u64) {
        let mut owners = self.voice_owners.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(owner) = owners.get_mut(user_pubkey)
            && !self.conns.contains_key(owner)
        {
            *owner = conn_id;
        }
    }

    pub fn voice_owned_by(&self, user_pubkey: &str, conn_id: u64) -> bool {
        self.voice_owners
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(user_pubkey)
            == Some(&conn_id)
    }

    pub fn update_owned_voice(
        &self,
        pubkey: &str,
        conn_id: u64,
        update: impl FnOnce(&Self) -> Option<VoiceState>,
    ) -> Option<VoiceState> {
        let owners = self.voice_owners.lock().unwrap_or_else(|e| e.into_inner());
        if owners.get(pubkey) != Some(&conn_id) {
            return None;
        }
        update(self)
    }

    fn set_voice_channel_unlocked(
        &self,
        user_pubkey: &str,
        guild_id: Id,
        channel_id: Option<Id>,
        preferences: Option<crate::protocol::VoicePreferences>,
    ) -> VoiceState {
        let prev = self.voice_states.get(user_pubkey).map(|v| v.clone());
        let deafened = preferences
            .map(|p| p.deafened)
            .unwrap_or_else(|| prev.as_ref().is_some_and(|p| p.deafened));
        let muted = deafened
            || preferences
                .map(|p| p.muted)
                .unwrap_or_else(|| prev.as_ref().is_some_and(|p| p.muted));
        let state = VoiceState {
            user_pubkey: user_pubkey.to_string(),
            guild_id,
            channel_id,
            muted,
            deafened,
            speaking: false,
            screen_sharing: false,
            screen_watching: Vec::new(),
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

    pub fn channel(&self, channel_id: Id) -> Option<Channel> {
        self.channels.get(&channel_id).map(|c| c.clone())
    }

    pub fn hideable_channels_of(&self, guild_id: Id) -> Vec<Channel> {
        self.channels
            .iter()
            .filter(|c| c.guild_id == guild_id && c.kind.can_be_hidden())
            .map(|c| c.clone())
            .collect()
    }

    /// Who is sent a channel and may join it (trap 30). Whoever manages
    /// channels always can: someone has to be able to open it back up.
    pub fn can_see_channel(&self, pubkey: &str, channel: &Channel) -> bool {
        let Some(access) = &channel.access else {
            return true;
        };
        if access.users.iter().any(|u| u == pubkey) {
            return true;
        }
        if self
            .effective_permissions(channel.guild_id, pubkey)
            .contains(&Permission::ManageChannels)
        {
            return true;
        }
        let held: Vec<Id> = self
            .members
            .get(&channel.guild_id)
            .and_then(|m| m.get(pubkey).map(|m| m.roles.clone()))
            .unwrap_or_default();
        access.roles.iter().any(|r| held.contains(r))
    }

    /// What one person is shown of someone's voice state: inside a channel
    /// they cannot see, they are not in voice at all.
    pub fn voice_state_seen_by(&self, viewer: &str, vs: VoiceState) -> VoiceState {
        let hidden = vs
            .channel_id
            .and_then(|c| self.channel(c))
            .is_some_and(|c| !self.can_see_channel(viewer, &c));
        if hidden { masked_voice_state(vs) } else { vs }
    }

    pub async fn set_channel_access(
        &self,
        channel_id: Id,
        access: Option<crate::protocol::ChannelAccess>,
        by_pubkey: &str,
    ) -> Result<Channel, String> {
        let _write = self.durable_writes.lock().await;
        let channel = self.channel(channel_id).ok_or("unknown channel")?;
        self.require_permission(channel.guild_id, by_pubkey, Permission::ManageChannels)?;
        if !channel.kind.can_be_hidden() {
            return Err("only voice channels and dividers can be hidden".into());
        }
        let access = access.map(crate::sanitize::access);
        let updated = {
            let mut entry = self
                .channels
                .get(&channel_id)
                .map(|c| c.clone())
                .ok_or("unknown channel")?;
            entry.access = access;
            entry.clone()
        };
        durable(self.store.upsert_channel(&updated).await, "channel access")?;
        self.channels.insert(channel_id, updated.clone());
        Ok(updated)
    }

    pub fn voice_members_in(&self, channel_id: Id) -> Vec<String> {
        self.voice_states
            .iter()
            .filter(|v| v.channel_id == Some(channel_id))
            .map(|v| v.user_pubkey.clone())
            .collect()
    }

    pub fn voice_channel_of(&self, user_pubkey: &str) -> Option<Id> {
        self.voice_states
            .get(user_pubkey)
            .and_then(|v| v.channel_id)
    }

    pub fn clear_voice(&self, user_pubkey: &str) -> Option<VoiceState> {
        let mut owners = self.voice_owners.lock().unwrap_or_else(|e| e.into_inner());
        owners.remove(user_pubkey);
        self.clear_voice_unlocked(user_pubkey)
    }

    pub fn clear_voice_owned(
        &self,
        user_pubkey: &str,
        conn_id: u64,
    ) -> Option<(VoiceState, VoiceState)> {
        let mut owners = self.voice_owners.lock().unwrap_or_else(|e| e.into_inner());
        if owners.get(user_pubkey) != Some(&conn_id) {
            return None;
        }
        owners.remove(user_pubkey);
        let previous = self.voice_states.get(user_pubkey)?.clone();
        let cleared = self.clear_voice_unlocked(user_pubkey)?;
        Some((previous, cleared))
    }

    fn clear_voice_unlocked(&self, user_pubkey: &str) -> Option<VoiceState> {
        let prev = self.voice_states.remove(user_pubkey)?.1;
        Some(VoiceState {
            channel_id: None,
            speaking: false,
            camera_on: false,
            screen_sharing: false,
            screen_watching: Vec::new(),
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

    #[allow(clippy::too_many_arguments)]
    pub async fn install_bot(
        &self,
        guild_id: Id,
        bot_pubkey: &str,
        name: &str,
        permissions: Vec<Permission>,
        intents: Vec<Intent>,
        manifest: Option<String>,
        by_pubkey: &str,
    ) -> Result<(BotInstall, crate::store::Seat), String> {
        let app = match manifest {
            Some(event) => Some(self.verified_manifest_for(bot_pubkey, &event)?),
            None => None,
        };
        if let Some(app) = &app {
            if let Some(p) = permissions
                .iter()
                .find(|p| !app.manifest.permissions.contains(p))
            {
                return Err(format!("the app's manifest does not ask for {p:?}"));
            }
            if let Some(i) = intents.iter().find(|i| !app.manifest.intents.contains(i)) {
                return Err(format!("the app's manifest does not ask for {i:?}"));
            }
        }
        let _write = self.durable_writes.lock().await;
        self.require_permission(guild_id, by_pubkey, Permission::ManageGuild)?;
        if let Some(curator) = self.blocked_by(guild_id, bot_pubkey) {
            return Err(format!(
                "that app is on a blocklist this guild follows (curated by {})",
                &curator[..12]
            ));
        }
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
        self.may_grant_to_bot(guild_id, by_pubkey, &permissions)?;
        let name = {
            let n = crate::protocol::sanitize_line(name, 32);
            match (&app, n.is_empty()) {
                (_, false) => n,
                (Some(app), true) => app.manifest.name.clone(),
                (None, true) => "Bot".to_string(),
            }
        };
        let install = BotInstall {
            guild_id,
            bot_pubkey: bot_pubkey.to_string(),
            name: name.clone(),
            permissions: unique(permissions),
            intents: unique(intents),
            may_call: self
                .bot_install(guild_id, bot_pubkey)
                .map(|i| i.may_call)
                .unwrap_or_default(),
            app: app.as_ref().map(|a| a.manifest.clone()),
            pending: None,
            look: self
                .bot_install(guild_id, bot_pubkey)
                .map(|i| i.look)
                .unwrap_or_default(),
        };
        let seat = self.seat_for(&install);
        durable(self.store.install_bot(&install, &seat).await, "bot install")?;
        self.bot_installs
            .entry(bot_pubkey.to_string())
            .or_default()
            .insert(guild_id, install.clone());
        self.apply_seat(&install, &seat);
        Ok((install, seat))
    }

    pub async fn uninstall_bot(
        &self,
        guild_id: Id,
        bot_pubkey: &str,
        by_pubkey: &str,
    ) -> Result<(), String> {
        let _write = self.durable_writes.lock().await;
        self.require_permission(guild_id, by_pubkey, Permission::ManageGuild)?;
        durable(
            self.store.uninstall_bot(guild_id, bot_pubkey).await,
            "bot uninstall",
        )?;
        // An app allowed to call this one must not regain that if it returns.
        for mut other in self.guild_installs(guild_id) {
            if other.may_call.iter().any(|k| k == bot_pubkey) {
                other.may_call.retain(|k| k != bot_pubkey);
                if self.store.upsert_bot_install(&other).await.is_ok() {
                    self.bot_installs
                        .entry(other.bot_pubkey.clone())
                        .or_default()
                        .insert(guild_id, other);
                }
            }
        }
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
        let mut channels: Vec<Channel> = self
            .channels
            .iter()
            .filter(|c| my_guild_ids.contains(&c.guild_id))
            .map(|c| c.clone())
            .collect();
        channels.retain(|c| self.can_see_channel(&bot.pubkey, c));
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
            chat_tools: false,
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
            sounds: Vec::new(),
            activities: Vec::new(),
            bot_commands: Vec::new(),
            operator: false,
            default_guild: None,
        }
    }
}

pub fn masked_voice_state(vs: VoiceState) -> VoiceState {
    VoiceState {
        channel_id: None,
        speaking: false,
        camera_on: false,
        screen_sharing: false,
        screen_watching: Vec::new(),
        ..vs
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

pub use crate::protocol::is_hex_color;

/// The read guard ends before the write. `match map.get(k)` keeps its guard
/// alive through every arm, and an insert there on the same key hangs the thread.
fn mark_if_off_cooldown(
    last_by_key: &DashMap<String, std::time::Instant>,
    key: &str,
    window: std::time::Duration,
) -> bool {
    let now = std::time::Instant::now();
    let still_cooling = last_by_key
        .get(key)
        .is_some_and(|last| now.duration_since(*last) < window);
    if still_cooling {
        return false;
    }
    last_by_key.insert(key.to_string(), now);
    true
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

#[cfg(test)]
mod cooldown_tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn hidden_voice_state_does_not_leak_watched_streams() {
        let state = VoiceState {
            user_pubkey: "viewer".into(),
            guild_id: Id::new_v4(),
            channel_id: Some(Id::new_v4()),
            muted: false,
            deafened: false,
            speaking: true,
            camera_on: false,
            screen_sharing: false,
            screen_watching: vec!["private-sharer".into()],
        };
        let masked = masked_voice_state(state);
        assert!(masked.channel_id.is_none());
        assert!(masked.screen_watching.is_empty());
    }

    /// In a thread with a deadline: the regression this guards against is a
    /// hang, and a hung test is a red build with no name on it.
    #[test]
    fn the_first_award_after_the_cooldown_does_not_deadlock() {
        let (done, finished) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let map: DashMap<String, std::time::Instant> = DashMap::new();
            let window = Duration::from_millis(50);
            assert!(mark_if_off_cooldown(&map, "k", window), "first award");
            assert!(
                !mark_if_off_cooldown(&map, "k", window),
                "inside the window"
            );
            std::thread::sleep(Duration::from_millis(80));
            assert!(mark_if_off_cooldown(&map, "k", window), "window passed");
            assert!(!mark_if_off_cooldown(&map, "k", window), "inside again");
            done.send(()).unwrap();
        });
        finished
            .recv_timeout(Duration::from_secs(5))
            .expect("cooldown bookkeeping deadlocked");
    }
}
