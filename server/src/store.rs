//! Phase-1 persistence: a SQLite-backed store (sqlx).
//!
//! Design (see docs/ROADMAP.md):
//! - **Messages live only here** — they're the unbounded dataset, queried and
//!   paginated from the DB, never held in RAM.
//! - **Metadata (guilds/channels/roles/members/bans/invites/profiles/…)** is
//!   authoritative in `AppState`'s in-memory maps (that's what every tested
//!   security invariant runs against) and **written through** here on every
//!   mutation, then **rehydrated** at boot. Single-node, single-writer: no
//!   cache-invalidation windows, no TOCTOU reopened.
//! - Encodings are deliberately portable (TEXT ids, INTEGER unix-ms times,
//!   JSON TEXT for small vecs) so a Postgres backend can slot in behind the
//!   same API later without a schema redesign.

use std::path::Path;

use chrono::{DateTime, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::protocol::{
    BotInstall, Channel, ChannelKind, Guild, GuildVisibility, Id, Member, Message,
    Profile, Reaction, Role, User,
};

/// Everything rehydrated into `AppState` at boot.
#[derive(Default)]
pub struct LoadedState {
    pub users: Vec<User>,
    pub profiles: Vec<Profile>,
    pub xp: Vec<(String, u64)>,
    pub guilds: Vec<Guild>,
    pub channels: Vec<Channel>,
    /// (guild_id, pubkey, username, bot, role ids)
    pub members: Vec<(Id, String, String, bool, Vec<Id>)>,
    pub roles: Vec<Role>,
    /// (dm channel id, participant_a, participant_b) — participants sorted.
    pub dms: Vec<(Id, String, String)>,
    pub bans: Vec<(Id, String)>,
    pub invites: Vec<(String, Id)>,
    pub bot_installs: Vec<BotInstall>,
}

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
}

type Result<T> = std::result::Result<T, sqlx::Error>;

impl Store {
    /// Open (creating if needed) the SQLite database at `path` and ensure the
    /// schema exists. WAL journaling + incremental autovacuum so the retention
    /// sweep actually returns disk space.
    pub async fn open(path: &Path) -> Result<Store> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .pragma("auto_vacuum", "INCREMENTAL");
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(opts)
            .await?;
        let store = Store { pool };
        store.init_schema().await?;
        Ok(store)
    }

    async fn init_schema(&self) -> Result<()> {
        // IF NOT EXISTS everywhere: idempotent boot, no migration framework
        // needed until the schema actually changes shape (roadmap Phase 2+).
        let ddl = [
            "CREATE TABLE IF NOT EXISTS users (
                pubkey TEXT PRIMARY KEY, username TEXT NOT NULL)",
            "CREATE TABLE IF NOT EXISTS profiles (
                pubkey TEXT PRIMARY KEY, avatar TEXT, banner TEXT, bio TEXT,
                status TEXT, custom_status TEXT)",
            "CREATE TABLE IF NOT EXISTS xp (
                pubkey TEXT PRIMARY KEY, xp INTEGER NOT NULL DEFAULT 0)",
            "CREATE TABLE IF NOT EXISTS guilds (
                id TEXT PRIMARY KEY, name TEXT NOT NULL, icon TEXT,
                owner_pubkey TEXT NOT NULL DEFAULT '', accent TEXT,
                visibility TEXT NOT NULL DEFAULT 'public',
                description TEXT, icon_image TEXT, banner TEXT,
                retention_days INTEGER,
                join_gate TEXT NOT NULL DEFAULT 'open', rules TEXT,
                panic_mode INTEGER NOT NULL DEFAULT 0)",
            "CREATE TABLE IF NOT EXISTS channels (
                id TEXT PRIMARY KEY, guild_id TEXT NOT NULL, name TEXT NOT NULL,
                kind TEXT NOT NULL, topic TEXT,
                read_only INTEGER NOT NULL DEFAULT 0,
                slowmode_secs INTEGER NOT NULL DEFAULT 0,
                position INTEGER NOT NULL DEFAULT 0)",
            "CREATE TABLE IF NOT EXISTS audit_log (
                guild_id TEXT NOT NULL, at_ms INTEGER NOT NULL,
                actor_pubkey TEXT NOT NULL, action TEXT NOT NULL,
                target TEXT NOT NULL DEFAULT '', detail TEXT NOT NULL DEFAULT '')",
            "CREATE INDEX IF NOT EXISTS idx_audit_guild_time
                ON audit_log(guild_id, at_ms)",
            "CREATE INDEX IF NOT EXISTS idx_channels_guild ON channels(guild_id)",
            "CREATE TABLE IF NOT EXISTS members (
                guild_id TEXT NOT NULL, pubkey TEXT NOT NULL,
                username TEXT NOT NULL, bot INTEGER NOT NULL DEFAULT 0,
                roles TEXT NOT NULL DEFAULT '[]',
                PRIMARY KEY (guild_id, pubkey))",
            "CREATE TABLE IF NOT EXISTS roles (
                id TEXT PRIMARY KEY, guild_id TEXT NOT NULL, name TEXT NOT NULL,
                color TEXT, permissions TEXT NOT NULL DEFAULT '[]',
                position INTEGER NOT NULL DEFAULT 0)",
            "CREATE INDEX IF NOT EXISTS idx_roles_guild ON roles(guild_id)",
            "CREATE TABLE IF NOT EXISTS messages (
                id TEXT PRIMARY KEY, channel_id TEXT NOT NULL,
                author_pubkey TEXT NOT NULL, author_username TEXT NOT NULL,
                content TEXT NOT NULL, image TEXT,
                reactions TEXT NOT NULL DEFAULT '[]',
                created_at INTEGER NOT NULL)",
            "CREATE INDEX IF NOT EXISTS idx_messages_channel_time
                ON messages(channel_id, created_at)",
            "CREATE TABLE IF NOT EXISTS dms (
                id TEXT PRIMARY KEY, participant_a TEXT NOT NULL,
                participant_b TEXT NOT NULL)",
            "CREATE TABLE IF NOT EXISTS bans (
                guild_id TEXT NOT NULL, pubkey TEXT NOT NULL,
                PRIMARY KEY (guild_id, pubkey))",
            "CREATE TABLE IF NOT EXISTS invites (
                code TEXT PRIMARY KEY, guild_id TEXT NOT NULL UNIQUE)",
            "CREATE TABLE IF NOT EXISTS bot_installs (
                guild_id TEXT NOT NULL, bot_pubkey TEXT NOT NULL,
                name TEXT NOT NULL,
                permissions TEXT NOT NULL DEFAULT '[]',
                intents TEXT NOT NULL DEFAULT '[]',
                PRIMARY KEY (guild_id, bot_pubkey))",
        ];
        for stmt in ddl {
            sqlx::query(stmt).execute(&self.pool).await?;
        }
        Ok(())
    }

    // ----- boot -------------------------------------------------------------

    pub async fn load_all(&self) -> Result<LoadedState> {
        let mut out = LoadedState::default();

        for r in sqlx::query("SELECT pubkey, username FROM users")
            .fetch_all(&self.pool)
            .await?
        {
            out.users.push(User { pubkey: r.get(0), username: r.get(1) });
        }
        for r in sqlx::query(
            "SELECT pubkey, avatar, banner, bio, status, custom_status FROM profiles",
        )
        .fetch_all(&self.pool)
        .await?
        {
            out.profiles.push(Profile {
                pubkey: r.get(0),
                avatar: r.get(1),
                banner: r.get(2),
                bio: r.get(3),
                status: r.get(4),
                custom_status: r.get(5),
                xp: 0,
            });
        }
        for r in sqlx::query("SELECT pubkey, xp FROM xp").fetch_all(&self.pool).await? {
            let xp: i64 = r.get(1);
            out.xp.push((r.get(0), xp.max(0) as u64));
        }
        for r in sqlx::query(
            "SELECT id, name, icon, owner_pubkey, accent, visibility, description,
                    icon_image, banner, retention_days, join_gate, rules, panic_mode
             FROM guilds",
        )
        .fetch_all(&self.pool)
        .await?
        {
            let retention: Option<i64> = r.get(9);
            out.guilds.push(Guild {
                id: parse_id(&r.get::<String, _>(0)),
                name: r.get(1),
                icon: r.get(2),
                owner_pubkey: r.get(3),
                accent: r.get(4),
                visibility: parse_visibility(&r.get::<String, _>(5)),
                description: r.get(6),
                icon_image: r.get(7),
                banner: r.get(8),
                retention_days: retention.map(|d| d.max(0) as u32),
                join_gate: parse_gate(&r.get::<String, _>(10)),
                rules: r.get(11),
                panic_mode: r.get::<i64, _>(12) != 0,
            });
        }
        for r in sqlx::query(
            "SELECT id, guild_id, name, kind, topic, read_only, slowmode_secs, position
             FROM channels",
        )
        .fetch_all(&self.pool)
        .await?
        {
            out.channels.push(Channel {
                id: parse_id(&r.get::<String, _>(0)),
                guild_id: parse_id(&r.get::<String, _>(1)),
                name: r.get(2),
                kind: parse_kind(&r.get::<String, _>(3)),
                topic: r.get(4),
                read_only: r.get::<i64, _>(5) != 0,
                slowmode_secs: r.get::<i64, _>(6).max(0) as u32,
                position: r.get::<i64, _>(7).max(0) as u32,
            });
        }
        for r in sqlx::query("SELECT guild_id, pubkey, username, bot, roles FROM members")
            .fetch_all(&self.pool)
            .await?
        {
            out.members.push((
                parse_id(&r.get::<String, _>(0)),
                r.get(1),
                r.get(2),
                r.get::<i64, _>(3) != 0,
                parse_json_ids(&r.get::<String, _>(4)),
            ));
        }
        for r in sqlx::query("SELECT id, guild_id, name, color, permissions, position FROM roles")
            .fetch_all(&self.pool)
            .await?
        {
            out.roles.push(Role {
                id: parse_id(&r.get::<String, _>(0)),
                guild_id: parse_id(&r.get::<String, _>(1)),
                name: r.get(2),
                color: r.get(3),
                permissions: serde_json::from_str(&r.get::<String, _>(4)).unwrap_or_default(),
                position: r.get::<i64, _>(5).max(0) as u32,
            });
        }
        for r in sqlx::query("SELECT id, participant_a, participant_b FROM dms")
            .fetch_all(&self.pool)
            .await?
        {
            out.dms.push((parse_id(&r.get::<String, _>(0)), r.get(1), r.get(2)));
        }
        for r in sqlx::query("SELECT guild_id, pubkey FROM bans").fetch_all(&self.pool).await? {
            out.bans.push((parse_id(&r.get::<String, _>(0)), r.get(1)));
        }
        for r in sqlx::query("SELECT code, guild_id FROM invites").fetch_all(&self.pool).await? {
            out.invites.push((r.get(0), parse_id(&r.get::<String, _>(1))));
        }
        for r in sqlx::query(
            "SELECT guild_id, bot_pubkey, name, permissions, intents FROM bot_installs",
        )
        .fetch_all(&self.pool)
        .await?
        {
            out.bot_installs.push(BotInstall {
                guild_id: parse_id(&r.get::<String, _>(0)),
                bot_pubkey: r.get(1),
                name: r.get(2),
                permissions: serde_json::from_str(&r.get::<String, _>(3)).unwrap_or_default(),
                intents: serde_json::from_str(&r.get::<String, _>(4)).unwrap_or_default(),
            });
        }
        Ok(out)
    }

    // ----- metadata write-through -------------------------------------------

    pub async fn upsert_user(&self, user: &User) -> Result<()> {
        sqlx::query(
            "INSERT INTO users (pubkey, username) VALUES (?, ?)
             ON CONFLICT(pubkey) DO UPDATE SET username = excluded.username",
        )
        .bind(&user.pubkey)
        .bind(&user.username)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn upsert_profile(&self, p: &Profile) -> Result<()> {
        sqlx::query(
            "INSERT INTO profiles (pubkey, avatar, banner, bio, status, custom_status)
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT(pubkey) DO UPDATE SET avatar=excluded.avatar,
               banner=excluded.banner, bio=excluded.bio, status=excluded.status,
               custom_status=excluded.custom_status",
        )
        .bind(&p.pubkey)
        .bind(&p.avatar)
        .bind(&p.banner)
        .bind(&p.bio)
        .bind(&p.status)
        .bind(&p.custom_status)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn upsert_xp(&self, pubkey: &str, xp: u64) -> Result<()> {
        sqlx::query(
            "INSERT INTO xp (pubkey, xp) VALUES (?, ?)
             ON CONFLICT(pubkey) DO UPDATE SET xp = excluded.xp",
        )
        .bind(pubkey)
        .bind(xp as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn upsert_guild(&self, g: &Guild) -> Result<()> {
        sqlx::query(
            "INSERT INTO guilds (id, name, icon, owner_pubkey, accent, visibility,
                                 description, icon_image, banner, retention_days,
                                 join_gate, rules, panic_mode)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET name=excluded.name, icon=excluded.icon,
               owner_pubkey=excluded.owner_pubkey, accent=excluded.accent,
               visibility=excluded.visibility, description=excluded.description,
               icon_image=excluded.icon_image, banner=excluded.banner,
               retention_days=excluded.retention_days, join_gate=excluded.join_gate,
               rules=excluded.rules, panic_mode=excluded.panic_mode",
        )
        .bind(g.id.to_string())
        .bind(&g.name)
        .bind(&g.icon)
        .bind(&g.owner_pubkey)
        .bind(&g.accent)
        .bind(visibility_str(g.visibility))
        .bind(&g.description)
        .bind(&g.icon_image)
        .bind(&g.banner)
        .bind(g.retention_days.map(|d| d as i64))
        .bind(gate_str(g.join_gate))
        .bind(&g.rules)
        .bind(g.panic_mode as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Delete a guild and everything under it (channels, messages, members,
    /// roles, bans, invites, installs) in one transaction.
    pub async fn delete_guild(&self, guild_id: Id) -> Result<()> {
        let gid = guild_id.to_string();
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "DELETE FROM messages WHERE channel_id IN
               (SELECT id FROM channels WHERE guild_id = ?)",
        )
        .bind(&gid)
        .execute(&mut *tx)
        .await?;
        for table in ["channels", "members", "roles", "bans", "invites", "bot_installs"] {
            sqlx::query(&format!("DELETE FROM {table} WHERE guild_id = ?"))
                .bind(&gid)
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query("DELETE FROM guilds WHERE id = ?").bind(&gid).execute(&mut *tx).await?;
        tx.commit().await
    }

    pub async fn upsert_channel(&self, c: &Channel) -> Result<()> {
        sqlx::query(
            "INSERT INTO channels (id, guild_id, name, kind, topic, read_only,
                                   slowmode_secs, position)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET name=excluded.name, topic=excluded.topic,
               read_only=excluded.read_only, slowmode_secs=excluded.slowmode_secs,
               position=excluded.position",
        )
        .bind(c.id.to_string())
        .bind(c.guild_id.to_string())
        .bind(&c.name)
        .bind(kind_str(c.kind))
        .bind(&c.topic)
        .bind(c.read_only as i64)
        .bind(c.slowmode_secs as i64)
        .bind(c.position as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Append an audit-log row and return the guild's recent entries (newest
    /// first, capped) — trims older rows opportunistically.
    pub async fn append_audit(&self, guild_id: Id, e: &crate::protocol::AuditEntry) -> Result<()> {
        sqlx::query(
            "INSERT INTO audit_log (guild_id, at_ms, actor_pubkey, action, target, detail)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(guild_id.to_string())
        .bind(e.at_ms)
        .bind(&e.actor_pubkey)
        .bind(&e.action)
        .bind(&e.target)
        .bind(&e.detail)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn audit_log(&self, guild_id: Id, limit: u32) -> Result<Vec<crate::protocol::AuditEntry>> {
        let rows = sqlx::query(
            "SELECT at_ms, actor_pubkey, action, target, detail FROM audit_log
             WHERE guild_id = ? ORDER BY at_ms DESC LIMIT ?",
        )
        .bind(guild_id.to_string())
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| crate::protocol::AuditEntry {
                at_ms: r.get(0),
                actor_pubkey: r.get(1),
                action: r.get(2),
                target: r.get(3),
                detail: r.get(4),
            })
            .collect())
    }

    pub async fn delete_channel(&self, channel_id: Id) -> Result<()> {
        let cid = channel_id.to_string();
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM messages WHERE channel_id = ?")
            .bind(&cid)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM channels WHERE id = ?").bind(&cid).execute(&mut *tx).await?;
        tx.commit().await
    }

    pub async fn upsert_member(&self, m: &Member) -> Result<()> {
        sqlx::query(
            "INSERT INTO members (guild_id, pubkey, username, bot, roles)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT(guild_id, pubkey) DO UPDATE SET username=excluded.username,
               bot=excluded.bot, roles=excluded.roles",
        )
        .bind(m.guild_id.to_string())
        .bind(&m.user.pubkey)
        .bind(&m.user.username)
        .bind(m.bot as i64)
        .bind(serde_json::to_string(&m.roles).unwrap_or_else(|_| "[]".into()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn delete_member(&self, guild_id: Id, pubkey: &str) -> Result<()> {
        sqlx::query("DELETE FROM members WHERE guild_id = ? AND pubkey = ?")
            .bind(guild_id.to_string())
            .bind(pubkey)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn upsert_role(&self, role: &Role) -> Result<()> {
        sqlx::query(
            "INSERT INTO roles (id, guild_id, name, color, permissions, position)
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET name=excluded.name, color=excluded.color,
               permissions=excluded.permissions, position=excluded.position",
        )
        .bind(role.id.to_string())
        .bind(role.guild_id.to_string())
        .bind(&role.name)
        .bind(&role.color)
        .bind(serde_json::to_string(&role.permissions).unwrap_or_else(|_| "[]".into()))
        .bind(role.position as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn delete_role(&self, role_id: Id) -> Result<()> {
        sqlx::query("DELETE FROM roles WHERE id = ?")
            .bind(role_id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn upsert_dm(&self, id: Id, a: &str, b: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO dms (id, participant_a, participant_b) VALUES (?, ?, ?)
             ON CONFLICT(id) DO NOTHING",
        )
        .bind(id.to_string())
        .bind(a)
        .bind(b)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Ban rows are written BEFORE the membership row is removed by the caller
    /// — the DB can never say "not banned but also not a member" mid-ban.
    pub async fn insert_ban(&self, guild_id: Id, pubkey: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO bans (guild_id, pubkey) VALUES (?, ?)
             ON CONFLICT(guild_id, pubkey) DO NOTHING",
        )
        .bind(guild_id.to_string())
        .bind(pubkey)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn delete_ban(&self, guild_id: Id, pubkey: &str) -> Result<()> {
        sqlx::query("DELETE FROM bans WHERE guild_id = ? AND pubkey = ?")
            .bind(guild_id.to_string())
            .bind(pubkey)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn set_invite(&self, guild_id: Id, code: &str) -> Result<()> {
        let gid = guild_id.to_string();
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM invites WHERE guild_id = ?")
            .bind(&gid)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO invites (code, guild_id) VALUES (?, ?)")
            .bind(code)
            .bind(&gid)
            .execute(&mut *tx)
            .await?;
        tx.commit().await
    }

    pub async fn upsert_bot_install(&self, i: &BotInstall) -> Result<()> {
        sqlx::query(
            "INSERT INTO bot_installs (guild_id, bot_pubkey, name, permissions, intents)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT(guild_id, bot_pubkey) DO UPDATE SET name=excluded.name,
               permissions=excluded.permissions, intents=excluded.intents",
        )
        .bind(i.guild_id.to_string())
        .bind(&i.bot_pubkey)
        .bind(&i.name)
        .bind(serde_json::to_string(&i.permissions).unwrap_or_else(|_| "[]".into()))
        .bind(serde_json::to_string(&i.intents).unwrap_or_else(|_| "[]".into()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn delete_bot_install(&self, guild_id: Id, bot_pubkey: &str) -> Result<()> {
        sqlx::query("DELETE FROM bot_installs WHERE guild_id = ? AND bot_pubkey = ?")
            .bind(guild_id.to_string())
            .bind(bot_pubkey)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ----- messages (DB is the only home) ------------------------------------

    pub async fn insert_message(&self, m: &Message) -> Result<()> {
        sqlx::query(
            "INSERT INTO messages (id, channel_id, author_pubkey, author_username,
                                   content, image, reactions, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(m.id.to_string())
        .bind(m.channel_id.to_string())
        .bind(&m.author.pubkey)
        .bind(&m.author.username)
        .bind(&m.content)
        .bind(&m.image)
        .bind(serde_json::to_string(&m.reactions).unwrap_or_else(|_| "[]".into()))
        .bind(m.created_at.timestamp_millis())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The most recent `limit` messages of a channel, oldest-first (the shape
    /// clients render). `before` (unix ms) paginates further back.
    pub async fn history(
        &self,
        channel_id: Id,
        limit: u32,
        before: Option<i64>,
    ) -> Result<Vec<Message>> {
        let rows = match before {
            Some(cutoff) => {
                sqlx::query(
                    "SELECT id, channel_id, author_pubkey, author_username, content,
                            image, reactions, created_at
                     FROM messages WHERE channel_id = ? AND created_at < ?
                     ORDER BY created_at DESC, id DESC LIMIT ?",
                )
                .bind(channel_id.to_string())
                .bind(cutoff)
                .bind(limit as i64)
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query(
                    "SELECT id, channel_id, author_pubkey, author_username, content,
                            image, reactions, created_at
                     FROM messages WHERE channel_id = ?
                     ORDER BY created_at DESC, id DESC LIMIT ?",
                )
                .bind(channel_id.to_string())
                .bind(limit as i64)
                .fetch_all(&self.pool)
                .await?
            }
        };
        let mut out: Vec<Message> = rows.into_iter().map(row_to_message).collect();
        out.reverse(); // delivered oldest-first
        Ok(out)
    }

    /// Toggle `pubkey`'s reaction with `emoji` on a message (read-modify-write;
    /// safe under this process's single-writer design). Returns the updated
    /// reaction set, or None if the message doesn't exist.
    pub async fn toggle_reaction(
        &self,
        channel_id: Id,
        message_id: Id,
        emoji: &str,
        pubkey: &str,
    ) -> Result<Option<Vec<Reaction>>> {
        let row = sqlx::query("SELECT reactions FROM messages WHERE id = ? AND channel_id = ?")
            .bind(message_id.to_string())
            .bind(channel_id.to_string())
            .fetch_optional(&self.pool)
            .await?;
        let Some(row) = row else { return Ok(None) };
        let mut reactions: Vec<Reaction> =
            serde_json::from_str(&row.get::<String, _>(0)).unwrap_or_default();

        if let Some(r) = reactions.iter_mut().find(|r| r.emoji == emoji) {
            if let Some(pos) = r.users.iter().position(|u| u == pubkey) {
                r.users.remove(pos);
            } else {
                r.users.push(pubkey.to_string());
            }
        } else {
            reactions.push(Reaction { emoji: emoji.to_string(), users: vec![pubkey.to_string()] });
        }
        reactions.retain(|r| !r.users.is_empty());

        sqlx::query("UPDATE messages SET reactions = ? WHERE id = ?")
            .bind(serde_json::to_string(&reactions).unwrap_or_else(|_| "[]".into()))
            .bind(message_id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(Some(reactions))
    }

    /// The author pubkey of a message, if it exists (for delete permission).
    pub async fn message_author(&self, channel_id: Id, message_id: Id) -> Result<Option<String>> {
        let row = sqlx::query(
            "SELECT author_pubkey FROM messages WHERE id = ? AND channel_id = ?",
        )
        .bind(message_id.to_string())
        .bind(channel_id.to_string())
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.get(0)))
    }

    pub async fn delete_message(&self, channel_id: Id, message_id: Id) -> Result<()> {
        sqlx::query("DELETE FROM messages WHERE id = ? AND channel_id = ?")
            .bind(message_id.to_string())
            .bind(channel_id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Retention sweep: delete messages in `guild_id`'s channels older than
    /// `cutoff_ms`, then reclaim freed pages. Returns rows deleted.
    pub async fn sweep_guild_messages(&self, guild_id: Id, cutoff_ms: i64) -> Result<u64> {
        let res = sqlx::query(
            "DELETE FROM messages WHERE created_at < ? AND channel_id IN
               (SELECT id FROM channels WHERE guild_id = ?)",
        )
        .bind(cutoff_ms)
        .bind(guild_id.to_string())
        .execute(&self.pool)
        .await?;
        if res.rows_affected() > 0 {
            // Deleting rows alone never shrinks a SQLite file.
            let _ = sqlx::query("PRAGMA incremental_vacuum").execute(&self.pool).await;
        }
        Ok(res.rows_affected())
    }
}

// ----- row/enum helpers ------------------------------------------------------

fn row_to_message(r: sqlx::sqlite::SqliteRow) -> Message {
    let ms: i64 = r.get(7);
    Message {
        id: parse_id(&r.get::<String, _>(0)),
        channel_id: parse_id(&r.get::<String, _>(1)),
        author: User { pubkey: r.get(2), username: r.get(3) },
        content: r.get(4),
        image: r.get(5),
        reactions: serde_json::from_str(&r.get::<String, _>(6)).unwrap_or_default(),
        created_at: DateTime::<Utc>::from_timestamp_millis(ms).unwrap_or_else(Utc::now),
    }
}

fn parse_id(s: &str) -> Id {
    Uuid::parse_str(s).unwrap_or_else(|_| Uuid::nil())
}

fn parse_json_ids(s: &str) -> Vec<Id> {
    serde_json::from_str(s).unwrap_or_default()
}

fn visibility_str(v: GuildVisibility) -> &'static str {
    match v {
        GuildVisibility::Public => "public",
        GuildVisibility::Private => "private",
    }
}

fn parse_visibility(s: &str) -> GuildVisibility {
    if s == "private" { GuildVisibility::Private } else { GuildVisibility::Public }
}

fn kind_str(k: ChannelKind) -> &'static str {
    match k {
        ChannelKind::Text => "text",
        ChannelKind::Voice => "voice",
    }
}

fn parse_kind(s: &str) -> ChannelKind {
    if s == "voice" { ChannelKind::Voice } else { ChannelKind::Text }
}

fn gate_str(g: crate::protocol::JoinGate) -> &'static str {
    use crate::protocol::JoinGate::*;
    match g {
        Open => "open",
        Rules => "rules",
        Pow => "pow",
    }
}

fn parse_gate(s: &str) -> crate::protocol::JoinGate {
    use crate::protocol::JoinGate::*;
    match s {
        "rules" => Rules,
        "pow" => Pow,
        _ => Open,
    }
}
