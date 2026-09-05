use std::path::Path;

use chrono::{DateTime, Utc};
use std::collections::HashSet;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::protocol::{
    BotInstall, Channel, ChannelKind, Guild, GuildEmoji, GuildVisibility, Id, Member, Message,
    Profile, Reaction, ReplyRef, Role, User,
};

#[derive(Debug, Clone)]
pub struct InviteRow {
    pub code: String,
    pub guild_id: Id,
    pub expires_at_ms: Option<i64>,
    pub max_uses: Option<u32>,
    pub uses: u32,
    pub created_by: String,
}

#[derive(Default)]
pub struct LoadedState {
    pub users: Vec<User>,
    pub profiles: Vec<Profile>,
    pub guild_xp: Vec<(Id, String, u64)>,
    pub guilds: Vec<Guild>,
    pub channels: Vec<Channel>,
    pub members: Vec<(Id, String, String, bool, Vec<Id>)>,
    pub roles: Vec<Role>,
    pub emojis: Vec<GuildEmoji>,
    pub bans: Vec<(Id, String)>,
    pub invites: Vec<InviteRow>,
    pub bot_installs: Vec<BotInstall>,
}

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
}

type Result<T> = std::result::Result<T, sqlx::Error>;

pub const DB_FILE: &str = "discordia.db";

impl Store {
    /// The only way to reach a data directory's database. `export`/`import`
    /// spent a release opening `db.sqlite` beside the server's `discordia.db`.
    pub async fn open_in(data_dir: &Path) -> Result<Store> {
        Store::open(&data_dir.join(DB_FILE)).await
    }

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
        let ddl = [
            "CREATE TABLE IF NOT EXISTS users (
                pubkey TEXT PRIMARY KEY, username TEXT NOT NULL)",
            "CREATE TABLE IF NOT EXISTS profiles (
                pubkey TEXT PRIMARY KEY, avatar TEXT, banner TEXT, bio TEXT,
                status TEXT, custom_status TEXT)",
            "CREATE TABLE IF NOT EXISTS guild_xp (
                guild_id TEXT NOT NULL, pubkey TEXT NOT NULL,
                xp INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (guild_id, pubkey))",
            "CREATE TABLE IF NOT EXISTS guilds (
                id TEXT PRIMARY KEY, name TEXT NOT NULL, icon TEXT,
                owner_pubkey TEXT NOT NULL DEFAULT '', accent TEXT,
                visibility TEXT NOT NULL DEFAULT 'public',
                description TEXT, icon_image TEXT, banner TEXT,
                retention_days INTEGER,
                join_gate TEXT NOT NULL DEFAULT 'open', rules TEXT,
                panic_mode INTEGER NOT NULL DEFAULT 0,
                leveling TEXT)",
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
            "CREATE TABLE IF NOT EXISTS guild_emojis (
                id TEXT PRIMARY KEY, guild_id TEXT NOT NULL, shortcode TEXT NOT NULL,
                image TEXT NOT NULL, added_by TEXT NOT NULL DEFAULT '',
                created_ms INTEGER NOT NULL DEFAULT 0,
                UNIQUE (guild_id, shortcode))",
            "CREATE INDEX IF NOT EXISTS idx_emojis_guild ON guild_emojis(guild_id)",
            "CREATE TABLE IF NOT EXISTS messages (
                id TEXT PRIMARY KEY, channel_id TEXT NOT NULL,
                author_pubkey TEXT NOT NULL, author_username TEXT NOT NULL,
                content TEXT NOT NULL, image TEXT,
                reactions TEXT NOT NULL DEFAULT '[]',
                reply_id TEXT, reply_author_pubkey TEXT,
                reply_author_username TEXT, reply_excerpt TEXT,
                created_at INTEGER NOT NULL)",
            "CREATE INDEX IF NOT EXISTS idx_messages_channel_time
                ON messages(channel_id, created_at)",
            "CREATE TABLE IF NOT EXISTS bans (
                guild_id TEXT NOT NULL, pubkey TEXT NOT NULL,
                PRIMARY KEY (guild_id, pubkey))",
            "CREATE TABLE IF NOT EXISTS invites (
                code TEXT PRIMARY KEY, guild_id TEXT NOT NULL UNIQUE,
                expires_at_ms INTEGER, max_uses INTEGER,
                uses INTEGER NOT NULL DEFAULT 0,
                created_by TEXT NOT NULL DEFAULT '')",
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

        for stmt in [
            "ALTER TABLE messages ADD COLUMN reply_id TEXT",
            "ALTER TABLE messages ADD COLUMN reply_author_pubkey TEXT",
            "ALTER TABLE messages ADD COLUMN reply_author_username TEXT",
            "ALTER TABLE messages ADD COLUMN reply_excerpt TEXT",
            "ALTER TABLE invites ADD COLUMN expires_at_ms INTEGER",
            "ALTER TABLE invites ADD COLUMN max_uses INTEGER",
            "ALTER TABLE invites ADD COLUMN uses INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE invites ADD COLUMN created_by TEXT NOT NULL DEFAULT ''",
            "ALTER TABLE guilds ADD COLUMN leveling TEXT",
        ] {
            if let Err(e) = sqlx::query(stmt).execute(&self.pool).await
                && !e.to_string().contains("duplicate column name")
            {
                return Err(e);
            }
        }
        Ok(())
    }

    pub async fn load_all(&self) -> Result<LoadedState> {
        let mut out = LoadedState::default();

        for r in sqlx::query("SELECT pubkey, username FROM users")
            .fetch_all(&self.pool)
            .await?
        {
            out.users.push(User {
                pubkey: r.get(0),
                username: r.get(1),
            });
        }
        for r in
            sqlx::query("SELECT pubkey, avatar, banner, bio, status, custom_status FROM profiles")
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
            });
        }
        for r in sqlx::query("SELECT guild_id, pubkey, xp FROM guild_xp")
            .fetch_all(&self.pool)
            .await?
        {
            let gid: String = r.get(0);
            let Ok(gid) = gid.parse::<Id>() else { continue };
            let xp: i64 = r.get(2);
            out.guild_xp.push((gid, r.get(1), xp.max(0) as u64));
        }
        for r in sqlx::query(
            "SELECT id, name, icon, owner_pubkey, accent, visibility, description,
                    icon_image, banner, retention_days, join_gate, rules, panic_mode,
                    leveling
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
                // A row written before the column existed reads as the
                // behaviour it had: a point a message, once a minute.
                leveling: r
                    .get::<Option<String>, _>(13)
                    .and_then(|j| serde_json::from_str(&j).ok())
                    .unwrap_or_default(),
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
        for r in sqlx::query(
            "SELECT id, guild_id, shortcode, image, added_by, created_ms FROM guild_emojis",
        )
        .fetch_all(&self.pool)
        .await?
        {
            out.emojis.push(GuildEmoji {
                id: parse_id(&r.get::<String, _>(0)),
                guild_id: parse_id(&r.get::<String, _>(1)),
                shortcode: r.get(2),
                image: r.get(3),
                added_by: r.get(4),
                created_ms: r.get(5),
            });
        }
        for r in sqlx::query("SELECT guild_id, pubkey FROM bans")
            .fetch_all(&self.pool)
            .await?
        {
            out.bans.push((parse_id(&r.get::<String, _>(0)), r.get(1)));
        }
        for r in sqlx::query(
            "SELECT code, guild_id, expires_at_ms, max_uses, uses, created_by FROM invites",
        )
        .fetch_all(&self.pool)
        .await?
        {
            out.invites.push(InviteRow {
                code: r.get(0),
                guild_id: parse_id(&r.get::<String, _>(1)),
                expires_at_ms: r.get(2),
                max_uses: r.get::<Option<i64>, _>(3).map(|v| v as u32),
                uses: r.get::<i64, _>(4) as u32,
                created_by: r.get(5),
            });
        }
        for r in
            sqlx::query("SELECT guild_id, bot_pubkey, name, permissions, intents FROM bot_installs")
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

    pub async fn upsert_guild_xp(&self, guild_id: Id, pubkey: &str, xp: u64) -> Result<()> {
        sqlx::query(
            "INSERT INTO guild_xp (guild_id, pubkey, xp) VALUES (?, ?, ?)
             ON CONFLICT(guild_id, pubkey) DO UPDATE SET xp = excluded.xp",
        )
        .bind(guild_id.to_string())
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
                                 join_gate, rules, panic_mode, leveling)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET name=excluded.name, icon=excluded.icon,
               owner_pubkey=excluded.owner_pubkey, accent=excluded.accent,
               visibility=excluded.visibility, description=excluded.description,
               icon_image=excluded.icon_image, banner=excluded.banner,
               retention_days=excluded.retention_days, join_gate=excluded.join_gate,
               rules=excluded.rules, panic_mode=excluded.panic_mode,
               leveling=excluded.leveling",
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
        .bind(serde_json::to_string(&g.leveling).ok())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

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
        for table in [
            "channels",
            "members",
            "roles",
            "guild_emojis",
            "bans",
            "invites",
            "bot_installs",
            "guild_xp",
        ] {
            sqlx::query(&format!("DELETE FROM {table} WHERE guild_id = ?"))
                .bind(&gid)
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query("DELETE FROM guilds WHERE id = ?")
            .bind(&gid)
            .execute(&mut *tx)
            .await?;
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

    pub async fn audit_log(
        &self,
        guild_id: Id,
        limit: u32,
    ) -> Result<Vec<crate::protocol::AuditEntry>> {
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
        sqlx::query("DELETE FROM channels WHERE id = ?")
            .bind(&cid)
            .execute(&mut *tx)
            .await?;
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

    pub async fn upsert_emoji(&self, e: &GuildEmoji) -> Result<()> {
        sqlx::query(
            "INSERT INTO guild_emojis (id, guild_id, shortcode, image, added_by, created_ms)
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET shortcode=excluded.shortcode,
               image=excluded.image",
        )
        .bind(e.id.to_string())
        .bind(e.guild_id.to_string())
        .bind(&e.shortcode)
        .bind(&e.image)
        .bind(&e.added_by)
        .bind(e.created_ms)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn delete_emoji(&self, emoji_id: Id) -> Result<()> {
        sqlx::query("DELETE FROM guild_emojis WHERE id = ?")
            .bind(emoji_id.to_string())
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

    pub async fn set_invite(
        &self,
        guild_id: Id,
        code: &str,
        expires_at_ms: Option<i64>,
        max_uses: Option<u32>,
        created_by: &str,
    ) -> Result<()> {
        let gid = guild_id.to_string();
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM invites WHERE guild_id = ?")
            .bind(&gid)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO invites (code, guild_id, expires_at_ms, max_uses, uses, created_by)
             VALUES (?, ?, ?, ?, 0, ?)",
        )
        .bind(code)
        .bind(&gid)
        .bind(expires_at_ms)
        .bind(max_uses.map(i64::from))
        .bind(created_by)
        .execute(&mut *tx)
        .await?;
        tx.commit().await
    }

    pub async fn referenced_media(&self) -> Result<HashSet<String>> {
        let mut out = HashSet::new();
        for r in sqlx::query("SELECT image FROM messages WHERE image IS NOT NULL")
            .fetch_all(&self.pool)
            .await?
        {
            let v: String = r.get(0);
            if let Some(name) = v.strip_prefix("media:") {
                out.insert(name.to_string());
            }
        }
        for r in sqlx::query("SELECT image FROM guild_emojis")
            .fetch_all(&self.pool)
            .await?
        {
            let v: String = r.get(0);
            out.insert(v.strip_prefix("media:").unwrap_or(&v).to_string());
        }
        // Profiles and guilds point at blobs too; a sweep that forgot them
        // deleted every avatar older than a day.
        for sql in [
            "SELECT avatar, banner FROM profiles",
            "SELECT icon_image, banner FROM guilds",
        ] {
            for r in sqlx::query(sql).fetch_all(&self.pool).await? {
                for col in 0..2 {
                    let v: Option<String> = r.get(col);
                    if let Some(name) = v.as_deref().and_then(|v| v.strip_prefix("media:")) {
                        out.insert(name.to_string());
                    }
                }
            }
        }
        Ok(out)
    }

    pub async fn bump_invite_uses(&self, code: &str) -> Result<()> {
        sqlx::query("UPDATE invites SET uses = uses + 1 WHERE code = ?")
            .bind(code)
            .execute(&self.pool)
            .await
            .map(|_| ())
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

    pub async fn insert_message(&self, m: &Message) -> Result<()> {
        sqlx::query(
            "INSERT INTO messages (id, channel_id, author_pubkey, author_username,
                                   content, image, reactions, reply_id,
                                   reply_author_pubkey, reply_author_username,
                                   reply_excerpt, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(m.id.to_string())
        .bind(m.channel_id.to_string())
        .bind(&m.author.pubkey)
        .bind(&m.author.username)
        .bind(&m.content)
        .bind(&m.image)
        .bind(serde_json::to_string(&m.reactions).unwrap_or_else(|_| "[]".into()))
        .bind(m.reply_to.as_ref().map(|r| r.message_id.to_string()))
        .bind(m.reply_to.as_ref().map(|r| r.author_pubkey.clone()))
        .bind(m.reply_to.as_ref().map(|r| r.author_username.clone()))
        .bind(m.reply_to.as_ref().map(|r| r.excerpt.clone()))
        .bind(m.created_at.timestamp_millis())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn reply_ref(&self, channel_id: Id, message_id: Id) -> Result<Option<ReplyRef>> {
        let row = sqlx::query(
            "SELECT author_pubkey, author_username, content, image
             FROM messages WHERE id = ? AND channel_id = ?",
        )
        .bind(message_id.to_string())
        .bind(channel_id.to_string())
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| {
            let content: String = r.get(2);
            let image: Option<String> = r.get(3);
            ReplyRef {
                message_id,
                author_pubkey: r.get(0),
                author_username: r.get(1),
                excerpt: excerpt_for_reply(&content, image.is_some()),
            }
        }))
    }

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
                            image, reactions, reply_id, reply_author_pubkey,
                            reply_author_username, reply_excerpt, created_at
                     FROM messages WHERE channel_id = ? AND created_at < ?
                     ORDER BY created_at DESC, rowid DESC LIMIT ?",
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
                            image, reactions, reply_id, reply_author_pubkey,
                            reply_author_username, reply_excerpt, created_at
                     FROM messages WHERE channel_id = ?
                     ORDER BY created_at DESC, rowid DESC LIMIT ?",
                )
                .bind(channel_id.to_string())
                .bind(limit as i64)
                .fetch_all(&self.pool)
                .await?
            }
        };
        let mut out: Vec<Message> = rows.into_iter().map(row_to_message).collect();
        out.reverse();
        Ok(out)
    }

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
            reactions.push(Reaction {
                emoji: emoji.to_string(),
                users: vec![pubkey.to_string()],
            });
        }
        reactions.retain(|r| !r.users.is_empty());

        sqlx::query("UPDATE messages SET reactions = ? WHERE id = ?")
            .bind(serde_json::to_string(&reactions).unwrap_or_else(|_| "[]".into()))
            .bind(message_id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(Some(reactions))
    }

    pub async fn message_author(&self, channel_id: Id, message_id: Id) -> Result<Option<String>> {
        let row = sqlx::query("SELECT author_pubkey FROM messages WHERE id = ? AND channel_id = ?")
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
            let _ = sqlx::query("PRAGMA incremental_vacuum")
                .execute(&self.pool)
                .await;
        }
        Ok(res.rows_affected())
    }
}

fn row_to_message(r: sqlx::sqlite::SqliteRow) -> Message {
    let ms: i64 = r.get(11);
    let reply_id: Option<String> = r.get(7);
    let reply_to = reply_id.and_then(|id| {
        Some(ReplyRef {
            message_id: parse_id(&id),
            author_pubkey: r.get::<Option<String>, _>(8)?,
            author_username: r.get::<Option<String>, _>(9).unwrap_or_default(),
            excerpt: r.get::<Option<String>, _>(10).unwrap_or_default(),
        })
    });
    Message {
        id: parse_id(&r.get::<String, _>(0)),
        channel_id: parse_id(&r.get::<String, _>(1)),
        author: User {
            pubkey: r.get(2),
            username: r.get(3),
        },
        content: r.get(4),
        image: r.get(5),
        reactions: serde_json::from_str(&r.get::<String, _>(6)).unwrap_or_default(),
        reply_to,
        created_at: DateTime::<Utc>::from_timestamp_millis(ms).unwrap_or_else(Utc::now),
    }
}

fn excerpt_for_reply(content: &str, has_image: bool) -> String {
    let flat = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return if has_image {
            "[image]".into()
        } else {
            String::new()
        };
    }
    if flat.chars().count() <= crate::protocol::REPLY_EXCERPT_CHARS {
        return flat;
    }
    let cut: String = flat
        .chars()
        .take(crate::protocol::REPLY_EXCERPT_CHARS)
        .collect();
    format!("{cut}…")
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
    if s == "private" {
        GuildVisibility::Private
    } else {
        GuildVisibility::Public
    }
}

fn kind_str(k: ChannelKind) -> &'static str {
    match k {
        ChannelKind::Text => "text",
        ChannelKind::Voice => "voice",
    }
}

fn parse_kind(s: &str) -> ChannelKind {
    if s == "voice" {
        ChannelKind::Voice
    } else {
        ChannelKind::Text
    }
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
