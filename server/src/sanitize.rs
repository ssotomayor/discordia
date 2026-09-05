//! The gateway filters what it is sent; these are the same filters for rows
//! that never passed through it — an archive being imported, and rows written
//! before a filter existed. Caps mirror the gateway arms they stand in for.

use crate::archive::GuildArchive;
use crate::protocol::{
    AuditEntry, BotInstall, Channel, Guild, GuildEmoji, Message, PRESENCES, Permission, Profile,
    REPLY_EXCERPT_CHARS, Role, User, canonical_username, sanitize_line, sanitize_name,
    sanitize_paragraph, valid_shortcode,
};
use crate::store::LoadedState;

const GUILD_NAME: usize = 64;
const CHANNEL_NAME: usize = 64;
const ROLE_NAME: usize = 32;
const TOPIC: usize = 120;
const DESCRIPTION: usize = 280;
const RULES: usize = 4000;
const BIO: usize = 280;
const CUSTOM_STATUS: usize = 80;
const BOT_NAME: usize = 32;
const PUBKEY: usize = 64;
const AUDIT_ACTION: usize = 64;
const AUDIT_TARGET: usize = 128;
const AUDIT_DETAIL: usize = 280;
pub const MESSAGE_BYTES: usize = 2000;

/// A name the gateway would have refused is cut down rather than dropped: the
/// row already exists and something has to be shown for it.
fn name_or(kind: &str, raw: &str, max: usize, fallback: &str) -> String {
    sanitize_name(kind, raw, max).unwrap_or_else(|_| {
        let cut = sanitize_line(raw, max);
        if cut.is_empty() {
            fallback.to_string()
        } else {
            cut
        }
    })
}

fn line(v: Option<String>, max: usize) -> Option<String> {
    v.map(|t| sanitize_line(&t, max)).filter(|t| !t.is_empty())
}

fn paragraph(v: Option<String>, max: usize) -> Option<String> {
    v.map(|t| sanitize_paragraph(&t, max))
        .filter(|t| !t.is_empty())
}

/// Anything else is a link, and a link is a fetch by every viewer.
fn picture(v: Option<String>) -> Option<String> {
    v.filter(|p| p.starts_with("media:") || p.starts_with("data:image/"))
}

fn pubkey(raw: &str) -> String {
    sanitize_line(raw, PUBKEY)
}

/// Bytes, like the gateway's own check, cut back to a character boundary.
pub fn message_content(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.len() <= MESSAGE_BYTES {
        return trimmed.to_string();
    }
    let mut end = MESSAGE_BYTES;
    while !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    trimmed[..end].trim_end().to_string()
}

pub fn guild(g: &mut Guild) {
    g.name = name_or("guild", &g.name, GUILD_NAME, "guild");
    g.icon = line(g.icon.take(), 4);
    g.owner_pubkey = pubkey(&g.owner_pubkey);
    g.accent = g.accent.take().filter(|a| crate::state::is_hex_color(a));
    g.description = paragraph(g.description.take(), DESCRIPTION);
    g.rules = paragraph(g.rules.take(), RULES);
    g.icon_image = picture(g.icon_image.take());
    g.banner = picture(g.banner.take());
}

pub fn channel(c: &mut Channel) {
    c.name = name_or("channel", &c.name, CHANNEL_NAME, "channel");
    c.topic = line(c.topic.take(), TOPIC);
}

pub fn role(r: &mut Role) {
    r.name = name_or("role", &r.name, ROLE_NAME, "role");
    r.color = r.color.take().filter(|c| crate::state::is_hex_color(c));
}

pub fn emoji_is_sound(e: &GuildEmoji) -> bool {
    valid_shortcode(&e.shortcode) && crate::media::is_address(&e.image)
}

pub fn user(u: &mut User) -> bool {
    u.pubkey = pubkey(&u.pubkey);
    u.username = canonical_username(&u.username);
    !u.pubkey.is_empty()
}

pub fn profile(p: &mut Profile) -> bool {
    p.pubkey = pubkey(&p.pubkey);
    p.avatar = picture(p.avatar.take());
    p.banner = picture(p.banner.take());
    p.bio = paragraph(p.bio.take(), BIO);
    p.status = p.status.take().filter(|s| PRESENCES.contains(&s.as_str()));
    p.custom_status = line(p.custom_status.take(), CUSTOM_STATUS);
    !p.pubkey.is_empty()
}

pub fn bot_install(b: &mut BotInstall) -> bool {
    b.bot_pubkey = pubkey(&b.bot_pubkey);
    b.name = name_or("bot", &b.name, BOT_NAME, "Bot");
    b.permissions
        .retain(|p| Permission::BOT_INSTALLABLE.contains(p));
    !b.bot_pubkey.is_empty()
}

pub fn message(m: &mut Message) -> bool {
    if !user(&mut m.author) {
        return false;
    }
    m.content = message_content(&m.content);
    m.image = picture(m.image.take());
    if m.content.is_empty() && m.image.is_none() {
        return false;
    }
    m.reactions.retain_mut(|r| {
        r.emoji = sanitize_line(&r.emoji, 8);
        r.users = r
            .users
            .iter()
            .map(|u| pubkey(u))
            .filter(|u| !u.is_empty())
            .collect();
        !r.emoji.is_empty() && !r.users.is_empty()
    });
    if let Some(reply) = m.reply_to.as_mut() {
        reply.author_pubkey = pubkey(&reply.author_pubkey);
        reply.author_username = canonical_username(&reply.author_username);
        reply.excerpt = sanitize_line(&reply.excerpt, REPLY_EXCERPT_CHARS + 1);
    }
    true
}

pub fn audit(a: &mut AuditEntry) -> bool {
    a.actor_pubkey = pubkey(&a.actor_pubkey);
    a.action = sanitize_line(&a.action, AUDIT_ACTION);
    a.target = sanitize_line(&a.target, AUDIT_TARGET);
    a.detail = sanitize_line(&a.detail, AUDIT_DETAIL);
    !a.actor_pubkey.is_empty() && !a.action.is_empty()
}

pub fn archive(a: &mut GuildArchive) {
    guild(&mut a.guild);
    a.channels.iter_mut().for_each(channel);
    a.roles.iter_mut().for_each(role);
    a.emojis.retain(emoji_is_sound);
    a.members.retain_mut(|m| user(&mut m.user));
    a.bans = a
        .bans
        .iter()
        .map(|b| pubkey(b))
        .filter(|b| !b.is_empty())
        .collect();
    a.bot_installs.retain_mut(bot_install);
    for (_, messages) in a.messages.iter_mut() {
        messages.retain_mut(message);
    }
    a.audit.retain_mut(audit);
}

pub fn loaded(l: &mut LoadedState) {
    l.guilds.iter_mut().for_each(guild);
    l.channels.iter_mut().for_each(channel);
    l.roles.iter_mut().for_each(role);
    l.emojis.retain(emoji_is_sound);
    l.users.retain_mut(user);
    l.profiles.retain_mut(profile);
    l.bot_installs.retain_mut(bot_install);
    l.members.retain_mut(|(_, pk, username, _, _)| {
        *pk = pubkey(pk);
        *username = canonical_username(username);
        !pk.is_empty()
    });
    l.bans.retain_mut(|(_, pk)| {
        *pk = pubkey(pk);
        !pk.is_empty()
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_is_cut_at_a_character_boundary() {
        let wide = "🙂".repeat(1000);
        let cut = message_content(&wide);
        assert!(cut.len() <= MESSAGE_BYTES);
        assert_eq!(cut.len() % 4, 0, "no half emoji at the end");
        assert_eq!(message_content("  short  "), "short");
    }

    #[test]
    fn a_name_the_gateway_would_refuse_is_cut_not_dropped() {
        let long = "a".repeat(200);
        assert_eq!(
            name_or("guild", &long, GUILD_NAME, "guild").len(),
            GUILD_NAME
        );
        assert_eq!(
            name_or("guild", "\u{202E}\u{0}", GUILD_NAME, "guild"),
            "guild"
        );
        assert_eq!(name_or("guild", " Fine ", GUILD_NAME, "guild"), "Fine");
    }

    #[test]
    fn a_picture_is_an_address_or_bytes_never_a_link() {
        assert!(picture(Some("https://example.invalid/a.png".into())).is_none());
        assert!(picture(Some("media:abc.png".into())).is_some());
        assert!(picture(Some("data:image/png;base64,AA==".into())).is_some());
    }
}
