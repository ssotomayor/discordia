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

/// Log-and-continue for write-through persistence. Memory is authoritative on
/// this (single-writer) node; a failed DB write means the mutation survives
/// the session but not a restart — loud in the logs, never a user-facing error.
fn persist(res: Result<(), sqlx::Error>, what: &str) {
    if let Err(e) = res {
        tracing::error!(error = %e, what, "write-through persist FAILED — state will regress on restart");
    }
}

/// Per-connection outbound queue depth. A connection whose queue fills (a
/// consumer too slow to keep up) is dropped; the client reconnects and gets a
/// fresh snapshot — the bounded-and-drop successor to the old lag→resync.
const CONN_QUEUE_CAP: usize = 256;

/// Max length of an inline image data URL (~2.2 MB of raw bytes once base64
/// is accounted for). Keeps a single broadcast frame from getting absurd.
pub const MAX_IMAGE_LEN: usize = 3_000_000;

/// A registered connection's outbound handle. The connection's own task drains
/// the matching receiver and writes to its websocket (applying bot-intent
/// filtering on the way out). Routing to this handle is what makes `deliver`
/// cost O(recipients) instead of O(all connections).
struct Conn {
    tx: mpsc::Sender<ServerMessage>,
}

/// One invite code, with whatever limits it was minted under.
///
/// **Both limits are `Option`, and `None` means unlimited** — which is what
/// every code minted before this existed already was, so an old code keeps
/// working rather than expiring the moment the server restarts.
///
/// Validation belongs here rather than at the call site because there are two
/// callers with different jobs: `invite_guild` resolves a code *before* the
/// join gate, so an expired code is refused instead of being handed a
/// proof-of-work challenge it can never spend, and `join_by_invite` resolves
/// it again and consumes a use. Both have to agree about what "still valid"
/// means.
#[derive(Debug, Clone)]
pub struct Invite {
    /// The code itself. Duplicated from the map key so an `Invite` handed to a
    /// caller is complete on its own.
    pub code: String,
    pub guild_id: Id,
    /// Unix ms after which the code stops working. `None` = never expires.
    pub expires_at_ms: Option<i64>,
    /// Successful joins the code allows. `None` = unlimited.
    pub max_uses: Option<u32>,
    /// Joins already spent. Only a join that actually happened counts — a
    /// challenge issued and never answered must not burn one.
    pub uses: u32,
    /// Who minted it. The "per-code attribution" half of the entry: an owner
    /// looking at a leaked code can now tell which moderator created it.
    pub created_by: String,
}

/// How long an unreferenced blob is left alone before the sweep may take it.
///
/// Generous on purpose: the cost of waiting is a day of disk, and the cost of
/// being wrong is a picture disappearing from a message somebody just sent.
const MEDIA_GRACE: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

/// Wall clock in Unix milliseconds — the unit every timestamp on the wire uses.
fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

impl Invite {
    /// Whether the code can still admit somebody, at `now_ms`.
    fn is_live(&self, now_ms: i64) -> bool {
        let unexpired = self.expires_at_ms.is_none_or(|at| now_ms < at);
        let unspent = self.max_uses.is_none_or(|max| self.uses < max);
        unexpired && unspent
    }
}

pub struct AppState {
    /// Durable persistence (SQLite). Messages live ONLY there; the metadata
    /// maps below are the in-memory authority, written through on mutation and
    /// rehydrated at boot.
    pub store: Store,
    /// Content-addressed blob storage for message images.
    pub media: MediaStore,
    pub guilds: DashMap<Id, Guild>,
    pub channels: DashMap<Id, Channel>,
    pub channels_by_guild: DashMap<Id, Vec<Id>>,
    /// Members per guild, by user pubkey.
    pub members: DashMap<Id, DashMap<String, Member>>,
    /// Every user we've ever seen identify, by pubkey. Lets us resolve a
    /// display name for a DM partner who may not share a guild with us.
    pub users: DashMap<String, User>,
    /// Public profiles (avatar/bio) by pubkey. Owned by the client, uploaded
    /// on connect; cached here so we can hand them to everyone.
    pub profiles: DashMap<String, Profile>,
    /// Voice state per user pubkey (global; a user can only be in one voice
    /// channel at a time across all guilds, same as Discord).
    pub voice_states: DashMap<String, VoiceState>,
    /// Installed bots, indexed by bot pubkey then guild. Grants apply to
    /// connections that self-declare as bots (`Identify { bot: true }`); an
    /// install alone never restricts a human connection. Managed via
    /// `ManageGuild`.
    pub bot_installs: DashMap<String, DashMap<Id, BotInstall>>,
    /// Guild roles, by guild. A member's effective permissions are the union
    /// over their assigned roles (see `effective_permissions`).
    pub roles: DashMap<Id, Vec<Role>>,
    /// Custom emoji, by guild. The catalog only — the images live in the
    /// content-addressed media store and are served on demand (`FetchEmoji`).
    pub emojis: DashMap<Id, Vec<GuildEmoji>>,
    /// Invite codes: code -> the invite. High-entropy random strings — invites are
    /// a ban-evasion surface, so guessability matters.
    pub invites: DashMap<String, Invite>,
    /// The (single, rotating) invite code per guild, reverse index.
    pub invite_by_guild: DashMap<Id, String>,
    /// Banned pubkeys per guild. Checked FIRST on every join path.
    pub bans: DashMap<Id, std::collections::HashSet<String>>,
    /// Last-post instant per (channel, pubkey), for slowmode. Ephemeral.
    pub last_post: DashMap<(Id, String), std::time::Instant>,
    /// Recent join instants per guild, for mass-join (raid) detection.
    /// Ephemeral; trimmed on each join.
    pub recent_joins: DashMap<Id, Vec<std::time::Instant>>,
    /// Server-authoritative message-XP per pubkey (drives the level display).
    /// Kept separate from `profiles` so the client-owned profile data stays
    /// clean; XP is injected onto members on emit (`stamp_xp`). Keyed
    /// guild → pubkey: levels are standing in ONE community, not a cross-guild
    /// reputation, and they survive leave/kick/rejoin.
    pub xp: DashMap<Id, DashMap<String, u64>>,
    /// Pubkeys treated as owners of SYSTEM guilds (empty `owner_pubkey`, e.g.
    /// the seeded Lobby) — the escape hatch that makes the shared landing
    /// space moderatable. Self-host sets this to the host's own pubkey; a
    /// central deployment reads it from `DIOXUSFUN_OPERATORS`. Operators get
    /// full permissions in system guilds but those guilds stay undeletable and
    /// non-transferable (see `delete_guild` / `transfer_ownership`).
    pub operators: std::collections::HashSet<String>,
    /// Registered connections by id → outbound handle. The routing table that
    /// replaced the single broadcast-everything hub.
    conns: DashMap<u64, Conn>,
    /// Identified connections indexed by user pubkey (a user may hold several —
    /// multiple devices/tabs). Lets `deliver` find exactly the target sockets.
    conn_ids_by_pubkey: DashMap<String, std::collections::HashSet<u64>>,
    /// Monotonic source of connection ids.
    next_conn_id: AtomicU64,
}

impl AppState {
    /// Rehydrate from the store, or seed a fresh instance (the system Lobby)
    /// if the database is empty. `operators` are owners of system guilds.
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
                    // Presence is ephemeral: everyone starts offline.
                    online: false,
                    bot,
                    roles: role_ids,
                    // Stored rows keep xp at 0 — the xp map is the truth,
                    // stamped at emit (`stamp_xp`).
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

    /// Seed the default system guild (empty owner: undeletable, auto-joined,
    /// manageable only by operators) with a text + voice channel.
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

    /// Register a fresh connection. Returns its id and the receiver its task
    /// drains. The connection joins `conns` immediately (so `broadcast` reaches
    /// it) but is not yet resolvable by pubkey until `identify_conn`.
    pub fn register_conn(&self) -> (u64, mpsc::Receiver<ServerMessage>) {
        let id = self.next_conn_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel(CONN_QUEUE_CAP);
        self.conns.insert(id, Conn { tx });
        (id, rx)
    }

    /// Associate a registered connection with its identified user so targeted
    /// delivery (`deliver`) can find it. Call this BEFORE sending the identify
    /// snapshot so no concurrently-delivered frame is lost in the gap.
    pub fn identify_conn(&self, conn_id: u64, pubkey: &str) {
        self.conn_ids_by_pubkey
            .entry(pubkey.to_string())
            .or_default()
            .insert(conn_id);
    }

    /// Tear a connection down on disconnect (or when it's dropped for being too
    /// slow). Idempotent.
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
                // Only remove if still empty (a racing reconnect may have
                // re-added the pubkey under a new conn id).
                self.conn_ids_by_pubkey
                    .remove_if(pk, |_, set| set.is_empty());
            }
        }
    }

    /// Enqueue `msg` for one connection. A full queue means the consumer can't
    /// keep up: drop the connection (its task ends, the client reconnects and
    /// resnapshots) rather than block every other recipient.
    fn route(&self, conn_id: u64, msg: &ServerMessage) {
        // Clone the sender out so the DashMap shard guard is released before we
        // ever call `remove` (removing while holding a ref to the same shard
        // would deadlock).
        let tx = match self.conns.get(&conn_id) {
            Some(c) => c.tx.clone(),
            None => return,
        };
        if tx.try_send(msg.clone()).is_err() {
            self.conns.remove(&conn_id);
        }
    }

    /// Send a message to every connected client. Now rare — only truly global
    /// frames (a profile/level update that could show anywhere) use this; guild
    /// and DM traffic goes through `deliver`.
    pub fn broadcast(&self, msg: ServerMessage) {
        let ids: Vec<u64> = self.conns.iter().map(|e| *e.key()).collect();
        for id in ids {
            self.route(id, &msg);
        }
    }

    /// Deliver a message only to the connections whose user is in `to`
    /// (deduplicated across a user's multiple devices). O(recipient
    /// connections), not O(all connections).
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

    /// Record (or refresh) a user in the global directory.
    pub async fn remember_user(&self, user: &User) {
        self.users.insert(user.pubkey.clone(), user.clone());
        persist(self.store.upsert_user(user).await, "user");
    }

    /// Store/overwrite a user's profile and return it.
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

    /// A member's message-XP in a guild (0 if they've never posted there).
    pub fn xp_of(&self, guild_id: Id, pubkey: &str) -> u64 {
        self.xp
            .get(&guild_id)
            .and_then(|g| g.get(pubkey).map(|v| *v))
            .unwrap_or(0)
    }

    /// Stamp a member with their authoritative per-guild XP. Called at every
    /// member emit point (stored rows keep xp at 0 — the map is the truth).
    pub fn stamp_xp(&self, mut member: Member) -> Member {
        member.xp = self.xp_of(member.guild_id, &member.user.pubkey);
        member
    }

    /// Increment a member's message-XP in a guild by one. Returns
    /// `Some(member)` — stamped with the new XP — only when the derived level
    /// changed, so callers deliver a `MemberUpdate` to that guild on level-up
    /// (rare) rather than per message.
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
        // Level changed — surface the member row so the new level renders in
        // that guild's rosters.
        let member = self
            .members
            .get(&guild_id)
            .and_then(|gm| gm.get(pubkey).map(|m| m.clone()))?;
        Some(self.stamp_xp(member))
    }

    /// Toggle a user's emoji reaction on a message. Returns the message's full
    /// reaction set afterwards, or None if the message doesn't exist.
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

    // ----- Permission engine -----------------------------------------------

    /// True if `pubkey` is the effective owner of `guild_id` for PERMISSION
    /// purposes. A normal guild: the literal `owner_pubkey`. A system guild
    /// (empty owner, e.g. the Lobby): a configured operator. Note this is not
    /// consulted by `delete_guild` / `transfer_ownership`, which guard on the
    /// literal empty owner so system guilds stay undeletable + non-transferable.
    pub fn is_owner(&self, guild_id: Id, pubkey: &str) -> bool {
        match self.guilds.get(&guild_id) {
            Some(g) if !g.owner_pubkey.is_empty() => g.owner_pubkey == pubkey,
            Some(_) => self.operators.contains(pubkey),
            None => false,
        }
    }

    /// A member's effective permission set: owner ⇒ everything; otherwise the
    /// union over their assigned roles (dangling role ids are ignored). This
    /// is the HUMAN grant path — bot connections derive per-guild powers from
    /// `bot_install` only, never from roles. System guilds have no owner and
    /// can never mint roles, so this returns empty there by construction.
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

    /// Whether `pubkey` holds `perm` in `guild_id` (owner ⇒ always).
    pub fn has_permission(&self, guild_id: Id, pubkey: &str, perm: Permission) -> bool {
        self.effective_permissions(guild_id, pubkey).contains(&perm)
    }

    /// Gate helper: `Err` with a uniform message when `perm` is missing.
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

    /// Requires `ManageGuild`: set/clear a guild's accent. Returns the updated
    /// guild on success.
    pub async fn set_guild_accent(
        &self,
        guild_id: Id,
        accent: Option<String>,
        by_pubkey: &str,
    ) -> Result<Guild, String> {
        // Check BEFORE taking the entry guard — require_permission reads the
        // same map.
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

    /// Flip a user's screen-share flag; `None` when it was already there.
    ///
    /// Same shape as `update_camera`, and it replaced a channel-keyed
    /// `DashMap<Id, HashSet<String>>`. Three things came off with that map: the
    /// unchanged-means-`None` short-circuit here bounds the fan-out a spammed
    /// toggle can cause, where the old setter re-broadcast its whole sorted list
    /// on every redundant call; the map leaked an empty `HashSet` per channel
    /// through `entry().or_default()` even when *stopping*; and teardown no
    /// longer needs call sites of its own, because leaving, disconnecting, being
    /// kicked and channel deletion all already clear the whole `VoiceState`.
    pub fn update_screen_share(&self, user_pubkey: &str, sharing: bool) -> Option<VoiceState> {
        let mut entry = self.voice_states.get_mut(user_pubkey)?;
        if entry.screen_sharing == sharing {
            return None;
        }
        entry.screen_sharing = sharing;
        Some(entry.clone())
    }

    /// The sorted pubkeys sharing a screen in a channel, derived from the voice
    /// states rather than stored.
    ///
    /// Only `ServerMessage::ScreenShareState` needs this. That message is kept
    /// because dropping it would silently take the LIVE badge away from any
    /// client older than `screen_sharing`, and nothing on this wire carries a
    /// version to detect that with. Deriving it means the two can never disagree.
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

    /// Live voice presence inside one guild.
    ///
    /// The single-guild counterpart of the filter `snapshot_for` runs across
    /// every guild you are in — `GuildJoined` needs exactly one, because that
    /// is the only guild the joiner has just gained the right to see voice
    /// presence for.
    pub fn voice_states_in(&self, guild_id: Id) -> Vec<VoiceState> {
        self.voice_states
            .iter()
            .filter(|v| v.guild_id == guild_id)
            .map(|v| v.value().clone())
            .collect()
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

    /// Create a new guild from a community `template` and make `creator` its
    /// first (online) member. Returns the guild, its channels, and the
    /// creator's membership so the caller can broadcast them. The returned
    /// roles are broadcast separately by the caller.
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

        // Channels from the template.
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
        // Roles from the template.
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
            self.add_member(gid, user).await;
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
        // Voice state is only visible within a guild you're in.
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

    /// Roles of the given guilds, flattened (for `Ready`).
    pub fn roles_for_guilds(&self, guild_ids: &[Id]) -> Vec<crate::protocol::Role> {
        guild_ids
            .iter()
            .flat_map(|gid| self.guild_roles(*gid))
            .collect()
    }

    /// A guild's roles (empty if none defined).
    pub fn guild_roles(&self, guild_id: Id) -> Vec<crate::protocol::Role> {
        self.roles
            .get(&guild_id)
            .map(|r| r.clone())
            .unwrap_or_default()
    }

    // ----- Role CRUD (grant-subset rule; see protocol::Role docs) ----------

    const MAX_ROLES_PER_GUILD: usize = 50;

    /// The anti-escalation gate every role mutation runs: the actor must hold
    /// `ManageRoles`, may only touch roles whose permissions they hold
    /// themselves (the subset rule), and roles carrying `ManageRoles` /
    /// `ManageGuild` are owner-touch-only (kills demotion wars between equal
    /// moderators). `role_perms` is every permission set involved — a role's
    /// current set, its updated set, or both.
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

    /// Validate + normalize a role name/color. Shared by create/update.
    fn sanitize_role(
        name: &str,
        color: Option<String>,
    ) -> Result<(String, Option<String>), String> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 32 {
            return Err("role name must be 1..=32 chars".into());
        }
        let color = color.filter(|c| is_hex_color(c));
        Ok((name.to_string(), color))
    }

    /// Create a role. Returns it on success.
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

    /// Full-replace a role's name/color/permissions.
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
        // Subset rule applies to what the role IS and what it WOULD BECOME —
        // otherwise a moderator could hollow out (or repurpose) a senior role.
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

    // -- Custom emoji ----------------------------------------------------
    //
    // Authority is `ManageEmojis` (the owner implicitly holds it, like every
    // other permission). The client's `can()` only hides buttons — every one of
    // these re-checks, because that is the only check that counts.

    /// The guild's emoji catalog (empty slice if it has none).
    pub fn emojis_of(&self, guild_id: Id) -> Vec<GuildEmoji> {
        self.emojis
            .get(&guild_id)
            .map(|e| e.clone())
            .unwrap_or_default()
    }

    /// Catalogs for every guild in `guild_ids` — used to build the Ready
    /// snapshot without leaking the emoji of guilds you aren't in.
    pub fn emojis_for_guilds(&self, guild_ids: &[Id]) -> Vec<GuildEmoji> {
        guild_ids.iter().flat_map(|g| self.emojis_of(*g)).collect()
    }

    /// Add a custom emoji. `image` is the media-store sentinel the caller has
    /// already stored (the gateway owns blob decoding, as it does for message
    /// attachments).
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

    /// Rename an emoji, leaving its image alone — clients that already hold the
    /// bytes keep them, since the content address hasn't moved.
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

    /// Remove an emoji from the catalog. The blob stays: it is content-
    /// addressed and may be shared with a message attachment, so dropping it
    /// needs refcounting (the blob-GC item in docs/AUDIT-2026-08-17.md).
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

    /// Delete a role, stripping it from every member. Returns the members
    /// whose role set changed (for `MemberUpdate` broadcasts).
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

    /// Grant (or revoke, for `assign = false`) a role on a member. Returns the
    /// updated member. Roles never apply to bots — assignment is rejected.
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

    /// Insert `user` as an online member of `guild_id` if not already present;
    /// otherwise just flip them online. Returns the resulting membership.
    /// (Presence isn't persisted — only NEW memberships hit the store.)
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
        // Rejoining resumes any level earned in this guild before.
        self.stamp_xp(member)
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

    /// Snapshot of PUBLIC guilds for the browse directory (private guilds are
    /// invite-only and never listed).
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

    /// A page of the public directory: `limit` summaries starting at `offset`
    /// (sorted by member count desc, then name, for a stable order), plus the
    /// total public-guild count. Backs the on-demand `FetchCatalog`.
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

    /// True if `pubkey` is banned from `guild_id`.
    pub fn is_banned(&self, guild_id: Id, pubkey: &str) -> bool {
        self.bans
            .get(&guild_id)
            .map(|b| b.contains(pubkey))
            .unwrap_or(false)
    }

    /// Add `user` to an existing PUBLIC guild from the directory. Returns the
    /// guild, its channels, members, and roles so the joiner's client can
    /// render it. Bans are checked before anything else.
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

    /// Redeem an invite code. Everything is validated at redemption time (the
    /// server is the sole oracle): the code must exist, map to a live guild,
    /// and the redeemer must not be banned. Works for private guilds — that's
    /// the point of invites.
    pub async fn join_by_invite(
        &self,
        code: &str,
        user: &User,
    ) -> Result<(Guild, Vec<Channel>, Vec<Member>, Vec<Role>), String> {
        // Checked and spent under the same entry lock, so two joins racing the
        // last use of a capped code cannot both win it.
        let code = code.trim();
        let (guild_id, spent) = {
            let mut entry = self
                .invites
                .get_mut(code)
                .ok_or_else(|| "unknown or expired invite code".to_string())?;
            if !entry.is_live(now_ms()) {
                return Err("unknown or expired invite code".into());
            }
            entry.uses += 1;
            (entry.guild_id, entry.uses)
        };
        let guild = self
            .guilds
            .get(&guild_id)
            .map(|g| g.clone())
            .ok_or_else(|| "unknown or expired invite code".to_string())?;
        if self.is_banned(guild_id, &user.pubkey) {
            // Refunded: a ban is not a redemption, and letting one burn a use
            // would let a banned key exhaust somebody else's capped code.
            if let Some(mut entry) = self.invites.get_mut(code) {
                entry.uses = entry.uses.saturating_sub(1);
            }
            return Err("you are banned from this guild".into());
        }
        persist(self.store.set_invite_uses(code, spent).await, "invite uses");
        Ok(self.admit_member(guild, user).await)
    }

    /// Shared tail of both join paths: add the member and snapshot the guild
    /// bundle for `GuildJoined`.
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

    // ----- Membership control (invites, kick/ban, leave) --------------------

    /// Requires `ManageGuild`: flip a guild between public and private.
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

    /// Requires `CreateInvite` or `ManageGuild`: return the guild's invite
    /// code, minting one if absent. `rotate` replaces (and invalidates) the
    /// current code.
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
        // An existing code is only worth handing back if it still works.
        // Otherwise "get" would return a dead code and the caller would have no
        // way to tell, which is the failure this entry is about.
        if !rotate
            && let Some(existing) = self.invite_by_guild.get(&guild_id)
            && let Some(invite) = self.invites.get(existing.value())
            && invite.is_live(now_ms())
        {
            return Ok(invite.clone());
        }
        // Rotation invalidates the old code.
        if let Some((_, old)) = self.invite_by_guild.remove(&guild_id) {
            self.invites.remove(&old);
        }
        // 12 random alphanumerics (~62 bits) — statistically unguessable, and
        // redemption is rate-limited on top.
        let code = loop {
            let candidate = random_invite_code();
            if !self.invites.contains_key(&candidate) {
                break candidate;
            }
        };
        let invite = Invite {
            code: code.clone(),
            guild_id,
            // Seconds in, absolute milliseconds out: a relative TTL is what a
            // caller can express, and an absolute instant is the only thing
            // that survives a restart without silently extending itself.
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

    /// Common target validation for kick/ban ("moderator immunity"): never the
    /// owner, never yourself, never a bot (uninstall instead), and moderators
    /// can't moderate moderators — only the owner can.
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

    /// Remove `target` from the guild (membership row + any voice state there).
    /// Shared tail of kick, ban, and leave. Returns the target's cleared voice
    /// state (if they were in voice in THIS guild) for broadcasting.
    fn remove_membership(&self, guild_id: Id, target_pubkey: &str) -> Option<VoiceState> {
        if let Some(gm) = self.members.get(&guild_id) {
            gm.remove(target_pubkey);
        }
        // Clear voice only if it points into this guild.
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

    /// Requires `KickMembers`: remove a member (they may rejoin later).
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

    /// Requires `BanMembers`: atomically remove membership AND record the ban
    /// (no join-between-kick-and-ban window — the ban set is written first,
    /// in the DB as well as in memory).
    pub async fn ban_member(
        &self,
        guild_id: Id,
        target_pubkey: &str,
        by_pubkey: &str,
    ) -> Result<Option<VoiceState>, String> {
        self.require_permission(guild_id, by_pubkey, Permission::BanMembers)?;
        self.validate_moderation_target(guild_id, target_pubkey, by_pubkey)?;
        // DB ban row FIRST: a crash mid-ban must never restart into
        // "membership removed but not banned" — the safe failure mode is
        // "banned and still listed as member", which the join gates ignore.
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

    /// Requires `BanMembers`: lift a ban.
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

    /// Requires `BanMembers`: the guild's ban list with display names resolved.
    pub fn ban_list(&self, guild_id: Id, by_pubkey: &str) -> Result<Vec<User>, String> {
        self.require_permission(guild_id, by_pubkey, Permission::BanMembers)?;
        Ok(self
            .bans
            .get(&guild_id)
            .map(|b| b.iter().map(|pk| self.resolve_user(pk)).collect())
            .unwrap_or_default())
    }

    /// Voluntarily leave a guild. Rejected for system guilds (you'd be
    /// auto-rejoined on reconnect) and for the owner (transfer or delete).
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

    // ----- Channel management + moderation (minimal) -------------------------

    /// Requires `ManageChannels`: add a channel. Position appends at the end.
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

    /// Requires `ManageChannels`: full-replace a channel's
    /// name/topic/read_only/position.
    /// Move channels within one guild, touching **only** their positions.
    ///
    /// The permission is checked once for the guild rather than once per row,
    /// and every id is required to belong to it — a caller must not be able to
    /// renumber a channel in a guild they can manage *into* one they cannot, or
    /// use a guild they own to move somebody else's rows.
    ///
    /// Returns the updated channels so the caller can broadcast them; an empty
    /// list is a no-op rather than an error, because a drag that ends where it
    /// started is not a failure.
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

    // Full replace of every editable field, so the argument count tracks the
    // channel's shape rather than a design worth splitting up.
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
            // Read-only only means something for text channels.
            channel.read_only =
                read_only && matches!(channel.kind, crate::protocol::ChannelKind::Text);
            channel.slowmode_secs = slowmode_secs.min(21_600); // cap at 6h
            channel.position = position;
            channel.clone()
        };
        persist(self.store.upsert_channel(&updated).await, "channel update");
        Ok(updated)
    }

    /// Requires `ManageChannels`: delete a channel. A guild's last text
    /// channel is protected (clients need somewhere to land). Returns the
    /// guild id and the voice states evicted from a deleted voice channel.
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
        // Evict anyone in the (voice) channel; collect the cleared states so
        // the caller can broadcast them.
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
        // No screen-share map to prune: the flag rides `VoiceState`, and the
        // `clear_voice` sweep above already took it with the occupants.
        if let Some(mut ids) = self.channels_by_guild.get_mut(&guild_id) {
            ids.retain(|c| *c != channel_id);
        }
        persist(
            self.store.delete_channel(channel_id).await,
            "channel delete",
        );
        Ok((guild_id, cleared))
    }

    // ----- Delegation + branding ---------------------------------------------

    /// Owner-only: hand the guild to another member. The target must be an
    /// existing human member (a bot can't own a guild). The old owner stays a
    /// member but instantly loses the implicit permissions.
    pub async fn transfer_ownership(
        &self,
        guild_id: Id,
        new_owner_pubkey: &str,
        by_pubkey: &str,
    ) -> Result<Guild, String> {
        // Guard on the LITERAL owner, not is_owner: a system guild (empty
        // owner) has no transferable ownership, and an operator isn't the
        // literal owner, so this correctly rejects both.
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

    /// Requires `ManageGuild`: full-replace the guild's description and
    /// icon/banner images (None clears). Validation mirrors `SetProfile`:
    /// http(s) URLs (≤2048) or data:image URLs under the size cap.
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
            // Reached when a client sends something odd (an old build, a bot,
            // a non-image mime slipping through) — say what would work rather
            // than restating the rule in wire terms.
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

    /// Reclaim blob files no row points at any more.
    ///
    /// Runs after the retention sweep on purpose: retention is what *creates*
    /// unreferenced blobs, by deleting the messages that named them. Doing it
    /// the other way round would leave every freshly-orphaned picture on disk
    /// for another hour.
    pub async fn sweep_media(&self) -> crate::media::SweepReport {
        match self.store.referenced_media().await {
            Ok(referenced) => self.media.sweep(&referenced, MEDIA_GRACE),
            // A failed query means we do not know what is referenced, and the
            // only safe answer to that is to delete nothing.
            Err(e) => {
                tracing::error!(error = %e, "media sweep skipped: could not read references");
                crate::media::SweepReport::default()
            }
        }
    }

    /// Requires `ManageGuild`: set/clear the guild's message retention (days).
    /// Clamped to 1..=3650; the hourly sweep enforces it.
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

    // ----- Community safety (gates, panic, slowmode, audit) -----------------

    /// Requires `ManageGuild`: configure the join gate + rules text.
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

    /// Requires `ManageGuild`: toggle anti-raid lockdown.
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

    /// Flip the panic flag without a permission check (used by auto-detection).
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

    /// The guild an invite code maps to, if any (for pre-join gate checks).
    pub fn invite_guild(&self, code: &str) -> Option<Id> {
        let now = now_ms();
        self.invites
            .get(code.trim())
            .filter(|i| i.is_live(now))
            .map(|i| i.guild_id)
    }

    /// The guild's current gate config (gate, rules, panic) for the join flow.
    pub fn join_requirements(
        &self,
        guild_id: Id,
    ) -> Option<(crate::protocol::JoinGate, Option<String>, bool)> {
        self.guilds
            .get(&guild_id)
            .map(|g| (g.join_gate, g.rules.clone(), g.panic_mode))
    }

    /// Record a join for raid detection; if joins-per-minute crosses the
    /// threshold, auto-enable panic mode and return the updated guild so the
    /// caller can broadcast it.
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

    /// Slowmode check + record. Moderators are exempt (checked by caller).
    /// Returns Err(remaining_secs) if the user must still wait.
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

    /// Append a moderation action to the guild's audit log (fire-and-forget).
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

    /// Requires `ManageGuild`: the guild's recent audit entries (newest first).
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

    /// Hourly sweep: enforce per-guild message retention. Returns total rows
    /// deleted (for logs).
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

    /// True if a channel is flagged read-only.
    pub fn channel_read_only(&self, channel_id: Id) -> bool {
        self.channels
            .get(&channel_id)
            .map(|c| c.read_only)
            .unwrap_or(false)
    }

    /// Delete a message. Authors always may; in guild channels `ManageMessages`
    /// holders may delete anyone's; DM messages are author-only (your DMs are
    /// nobody's moderation surface).
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

    /// Delete a guild and everything under it. Returns `Err` with a reason if
    /// the guild is unknown or `by_pubkey` is not its owner.
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

    /// The most recent `limit` messages of a channel (oldest-first), with any
    /// blob-stored images inlined back into data URLs for the wire.
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

    /// The DM conversations `pubkey` participates in, each described from their
    /// point of view (so `other` is the partner).
    pub async fn push_message(
        &self,
        channel_id: Id,
        author: User,
        content: String,
        image: Option<String>,
        reply_to: Option<Id>,
    ) -> Option<Message> {
        let kind_ok = self
            .channels
            .get(&channel_id)
            .map(|c| matches!(c.kind, crate::protocol::ChannelKind::Text))
            .unwrap_or(false);
        if !kind_ok {
            return None;
        }
        // Resolve the quote from our own row, scoped to this channel. An id
        // that doesn't resolve there is dropped rather than rejected: the reply
        // still sends, just without a quote.
        let reply_ref = match reply_to {
            Some(id) => self.store.reply_ref(channel_id, id).await.unwrap_or(None),
            None => None,
        };
        Some(
            self.append_message(channel_id, author, content, image, reply_ref)
                .await,
        )
    }

    /// Persist a message. Inbound data-URL images are offloaded to the blob
    /// store (the DB row keeps a tiny `media:` sentinel); the returned message
    /// carries the ORIGINAL image so the broadcast needs no re-read.
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
                // Unknown mime or decode failure → fall back to storing the
                // data URL itself (still capped by MAX_IMAGE_LEN upstream).
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

    /// Mark a user offline in every guild they're a member of. Returns the
    /// guild_id + pubkey pairs that were actually flipped (so callers know
    /// which broadcasts to send).
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
            // Same reset, same reason as the camera below: a share publishes into
            // the channel's own `screen-…` room, so switching channels leaves it.
            screen_sharing: false,
            // Reset, not carried over from `prev` like mute/deafen: the camera
            // publishes into the *channel's* `screen-…` room, so a channel
            // switch leaves the room it was published to. The webview has to
            // republish into the new one, and until it does, claiming the
            // camera is on would put a tile in front of everyone that never
            // fills in.
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
        // Deafening implies muting — you can't talk to people you can't hear —
        // but not the reverse: muting says nothing about what you're listening
        // to. Coercing it here rather than trusting the client is what keeps
        // the pair consistent for every watcher.
        //
        // It does not rescue a client older than the deafen button, which sends
        // `deafened: muted` and so now reads as deafened whenever it mutes. The
        // flag was equally wrong before this change; the difference is that
        // nothing rendered it. Coercing the other way would only trade a
        // mislabelled mute for a deafen that cannot be expressed at all, and
        // the mixed-version window is one pre-release wide.
        entry.muted = muted || deafened;
        entry.deafened = deafened;
        Some(entry.clone())
    }

    /// Flip a user's camera flag; `None` when it was already there.
    ///
    /// Modelled on `update_speaking` rather than `update_voice_flags`, and the
    /// unchanged-means-`None` short-circuit is the reason why. A camera button
    /// can be clicked as fast as a mouse allows, and every accepted call fans
    /// out to *every member of the guild* — so collapsing no-ops here is what
    /// keeps a spammed toggle from becoming a broadcast storm, without a rate
    /// limiter. `set_screen_share` does re-broadcast its whole sorted list on a
    /// redundant call; that is a wart, not a precedent.
    ///
    /// Returns `None` for a user with no voice state at all, which is what
    /// gates this on membership: only `JoinVoice` creates one, and it checks.
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

    /// Tombstone voice state for a disconnecting user; returns the cleared state if any.
    pub fn clear_voice(&self, user_pubkey: &str) -> Option<VoiceState> {
        let prev = self.voice_states.remove(user_pubkey)?.1;
        Some(VoiceState {
            channel_id: None,
            speaking: false,
            // Explicit, and it must stay explicit: `..prev` would carry a live
            // `true` into the tombstone delivered to every guild member. Today
            // the client drops the row once `channel_id` is None so nothing
            // renders it, which is exactly what would make a stale camera flag
            // here cost a debugging session rather than announce itself.
            camera_on: false,
            // Explicit for the same reason, and it matters more here: this flag
            // is older and is rendered in two places, so a stale `true` would be
            // visible rather than merely wrong.
            screen_sharing: false,
            ..prev
        })
    }

    // ----- Bot platform (Tier 1) -------------------------------------------

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

    /// Requires `ManageGuild`: install (or update the grants of) a bot in a
    /// guild. Adds the bot as a `bot: true` guild member so it shows in the
    /// roster. Returns the stored install and the bot's membership for
    /// broadcasting. Only `Permission::BOT_INSTALLABLE` grants are accepted.
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

        // Surface the bot in the roster. Keep an already-connected bot's online
        // flag; a fresh install starts offline until the bot process connects.
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

    /// Requires `ManageGuild`: remove a bot from a guild (drops its grants and
    /// roster row).
    pub async fn uninstall_bot(
        &self,
        guild_id: Id,
        bot_pubkey: &str,
        by_pubkey: &str,
    ) -> Result<(), String> {
        self.require_permission(guild_id, by_pubkey, Permission::ManageGuild)?;
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

    /// Scoped `Ready` for a bot connection: only the guilds it's installed in,
    /// their channels, and — gated behind the `Members` intent — their rosters.
    /// No DMs, no profiles, no public catalog (a bot doesn't browse).
    pub fn snapshot_for_bot(&self, bot: &User) -> ServerMessage {
        let installs = self.bot_guilds(&bot.pubkey);
        let my_guild_ids: Vec<Id> = installs.iter().map(|i| i.guild_id).collect();

        // Flip the bot online in each installed guild.
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
        // Roster is sensitive: only hand it over for guilds the bot was granted
        // the Members intent in.
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
            // Roles never apply to bot connections — don't leak them.
            roles: Vec::new(),
            // Nor do custom emoji: a bot posts text, and the catalog is guild
            // configuration it has no business enumerating.
            emojis: Vec::new(),
            // Bots are never operators.
            operator: false,
        }
    }
}

/// A community preset applied at guild creation: channel layout, role presets,
/// default visibility + join gate. Two choices at creation, not a hundred
/// toggles (roadmap Phase 4).
struct GuildTemplate {
    channels: Vec<(&'static str, crate::protocol::ChannelKind, bool)>, // (name, kind, read_only)
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
                // Public communities start private + rules-gated so the owner
                // can set up before opening the doors.
                visibility: Private,
                join_gate: JoinGate::Rules,
            },
            // "friend" and default: minimal + open.
            _ => GuildTemplate {
                channels: vec![("general", Text, false), ("General Voice", Voice, false)],
                roles: vec![],
                visibility: Public,
                join_gate: JoinGate::Open,
            },
        }
    }
}

/// Channel names mirror guild-name rules: trimmed, 1..=64 chars.
fn sanitize_channel_name(raw: &str) -> Result<String, String> {
    let name = raw.trim();
    if name.is_empty() || name.chars().count() > 64 {
        return Err("channel name must be 1..=64 chars".into());
    }
    Ok(name.to_string())
}

/// 12 random alphanumerics from the OS RNG (~62 bits of entropy). Invite codes
/// gate private guilds and ban evasion, so they must be unguessable — the cute
/// `adj-animal-NN` rendezvous format (~17 bits) is not enough here.
pub(crate) fn random_invite_code() -> String {
    use rand::Rng;
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut rng = rand::rngs::OsRng;
    (0..12)
        .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
        .collect()
}

/// Accept only short `#rrggbb`/`#rgb` hex colors (guild accents, role colors).
pub fn is_hex_color(s: &str) -> bool {
    let s = s.trim();
    let Some(hex) = s.strip_prefix('#') else {
        return false;
    };
    (hex.len() == 3 || hex.len() == 6) && hex.chars().all(|c| c.is_ascii_hexdigit())
}

/// The wire (snake_case) name of a unit enum variant, for error messages that
/// match what API users see in JSON (e.g. `manage_guild`).
fn serde_variant_name<T: serde::Serialize>(v: T) -> String {
    serde_json::to_string(&v)
        .unwrap_or_default()
        .trim_matches('"')
        .to_string()
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
