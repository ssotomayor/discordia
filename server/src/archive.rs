//! Guild export / import (Phase 6 — graduation).
//!
//! An archive is a self-contained, versioned snapshot of one guild: its
//! metadata, channels, roles, members, bans, invite, installed bots, message
//! history, and audit log. Import writes it into a (possibly different) store
//! under **fresh** guild/channel/role IDs, while leaving every **pubkey
//! unchanged** — nobody re-registers, and a member who follows the moved guild
//! keeps their identity. This is the mechanism a community uses to graduate
//! from one instance (or DB backend) to another.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::protocol::{
    AuditEntry, BotInstall, Channel, Guild, GuildEmoji, Id, Member, Message, Role,
};
use crate::store::Store;

/// Current archive schema version. Bump on a breaking layout change; `import`
/// refuses versions it doesn't understand.
pub const ARCHIVE_VERSION: u32 = 1;

/// A portable snapshot of a single guild. `messages` is keyed by the archive's
/// (old) channel id; import remaps those to the freshly-minted channel ids.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuildArchive {
    pub version: u32,
    pub guild: Guild,
    pub channels: Vec<Channel>,
    pub roles: Vec<Role>,
    /// The guild's custom emoji. Only the catalog travels — `image` is a
    /// content address, so the destination must already hold (or later
    /// re-receive) the blob. Cross-instance blob copy is the open P6-tail item
    /// in docs/AUDIT-2026-08-17.md; until then an imported guild keeps its shortcodes and
    /// re-uploads are cheap because identical bytes dedupe to the same address.
    #[serde(default)]
    pub emojis: Vec<GuildEmoji>,
    pub members: Vec<Member>,
    /// Banned pubkeys.
    pub bans: Vec<String>,
    /// The current invite code, if any (import mints a fresh one to avoid
    /// colliding with codes already live on the destination).
    pub invite: Option<String>,
    pub bot_installs: Vec<BotInstall>,
    /// (old channel id, that channel's messages in chronological order).
    pub messages: Vec<(Id, Vec<Message>)>,
    pub audit: Vec<AuditEntry>,
}

impl Store {
    /// Build a portable archive of `guild_id`, or `None` if no such guild.
    pub async fn export_guild(&self, guild_id: Id) -> anyhow::Result<Option<GuildArchive>> {
        let loaded = self.load_all().await?;
        let Some(guild) = loaded.guilds.into_iter().find(|g| g.id == guild_id) else {
            return Ok(None);
        };

        let channels: Vec<Channel> = loaded
            .channels
            .into_iter()
            .filter(|c| c.guild_id == guild_id)
            .collect();
        let roles: Vec<Role> = loaded
            .roles
            .into_iter()
            .filter(|r| r.guild_id == guild_id)
            .collect();
        let emojis: Vec<GuildEmoji> = loaded
            .emojis
            .into_iter()
            .filter(|e| e.guild_id == guild_id)
            .collect();

        // Rebuild members from the loaded tuples: (guild, pubkey, username, bot, roles).
        let members: Vec<Member> = loaded
            .members
            .into_iter()
            .filter(|(gid, ..)| *gid == guild_id)
            .map(|(gid, pubkey, username, bot, roles)| {
                let xp = loaded
                    .guild_xp
                    .iter()
                    .find(|(g, pk, _)| *g == gid && *pk == pubkey)
                    .map(|(_, _, xp)| *xp)
                    .unwrap_or(0);
                Member {
                    user: loaded
                        .users
                        .iter()
                        .find(|u| u.pubkey == pubkey)
                        .cloned()
                        .unwrap_or(crate::protocol::User { pubkey, username }),
                    guild_id: gid,
                    online: false,
                    bot,
                    roles,
                    xp,
                }
            })
            .collect();

        let bans: Vec<String> = loaded
            .bans
            .into_iter()
            .filter(|(gid, _)| *gid == guild_id)
            .map(|(_, pk)| pk)
            .collect();
        let invite = loaded
            .invites
            .into_iter()
            .find(|(_, gid)| *gid == guild_id)
            .map(|(code, _)| code);
        let bot_installs: Vec<BotInstall> = loaded
            .bot_installs
            .into_iter()
            .filter(|b| b.guild_id == guild_id)
            .collect();

        // Full history per channel, oldest-first (history() returns newest-first).
        let mut messages = Vec::new();
        for ch in &channels {
            let mut msgs = self.history(ch.id, u32::MAX, None).await?;
            msgs.reverse();
            messages.push((ch.id, msgs));
        }

        let audit = self.audit_log(guild_id, u32::MAX).await?;

        Ok(Some(GuildArchive {
            version: ARCHIVE_VERSION,
            guild,
            channels,
            roles,
            emojis,
            members,
            bans,
            invite,
            bot_installs,
            messages,
            audit,
        }))
    }

    /// Import an archive into this store under fresh guild/channel/role ids,
    /// preserving every pubkey. Returns the new guild id. Refuses an archive
    /// whose version this build doesn't understand.
    pub async fn import_guild(&self, archive: &GuildArchive) -> anyhow::Result<Id> {
        if archive.version != ARCHIVE_VERSION {
            anyhow::bail!(
                "unsupported archive version {} (this build understands {})",
                archive.version,
                ARCHIVE_VERSION
            );
        }

        let new_guild_id = Uuid::new_v4();
        // old id -> new id, for channels and roles (the only cross-referenced ids).
        let mut channel_map: HashMap<Id, Id> = HashMap::new();
        let mut role_map: HashMap<Id, Id> = HashMap::new();
        for c in &archive.channels {
            channel_map.insert(c.id, Uuid::new_v4());
        }
        for r in &archive.roles {
            role_map.insert(r.id, Uuid::new_v4());
        }

        // Guild — new id, everything else (owner pubkey, gates, branding) kept.
        let mut guild = archive.guild.clone();
        guild.id = new_guild_id;
        self.upsert_guild(&guild).await?;

        // Roles.
        for r in &archive.roles {
            let mut role = r.clone();
            role.id = role_map[&r.id];
            role.guild_id = new_guild_id;
            self.upsert_role(&role).await?;
        }

        // Emoji — fresh ids, shortcodes and content addresses preserved.
        for e in &archive.emojis {
            let mut emoji = e.clone();
            emoji.id = Uuid::new_v4();
            emoji.guild_id = new_guild_id;
            self.upsert_emoji(&emoji).await?;
        }

        // Channels.
        for c in &archive.channels {
            let mut ch = c.clone();
            ch.id = channel_map[&c.id];
            ch.guild_id = new_guild_id;
            self.upsert_channel(&ch).await?;
        }

        // Members — pubkeys unchanged; role ids remapped (unknown ids dropped).
        // Per-guild XP rides inside Member.xp and moves with the guild.
        for m in &archive.members {
            let mut member = m.clone();
            member.guild_id = new_guild_id;
            member.roles = m
                .roles
                .iter()
                .filter_map(|rid| role_map.get(rid).copied())
                .collect();
            self.upsert_member(&member).await?;
            if m.xp > 0 {
                self.upsert_guild_xp(new_guild_id, &m.user.pubkey, m.xp)
                    .await?;
            }
        }

        // Bans.
        for pk in &archive.bans {
            self.insert_ban(new_guild_id, pk).await?;
        }

        // Invite — mint a fresh code rather than reuse (avoids collisions on the
        // destination). Only if the source had one.
        if archive.invite.is_some() {
            let code = crate::state::random_invite_code();
            self.set_invite(new_guild_id, &code).await?;
        }

        // Installed bots.
        for b in &archive.bot_installs {
            let mut install = b.clone();
            install.guild_id = new_guild_id;
            self.upsert_bot_install(&install).await?;
        }

        // Messages — fresh message ids, remapped channel ids, pubkeys/content/
        // timestamps preserved.
        for (old_channel, msgs) in &archive.messages {
            let Some(new_channel) = channel_map.get(old_channel).copied() else {
                continue;
            };
            for m in msgs {
                let mut msg = m.clone();
                msg.id = Uuid::new_v4();
                msg.channel_id = new_channel;
                self.insert_message(&msg).await?;
            }
        }

        // Audit trail (historical; re-appended under the new guild id).
        for e in &archive.audit {
            self.append_audit(new_guild_id, e).await?;
        }

        Ok(new_guild_id)
    }
}
