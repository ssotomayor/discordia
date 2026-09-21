//! Import a Discord server's shape — name, pictures, channels, roles, emojis —
//! by replaying the messages the UI already sends, so the server re-checks
//! every permission and every picture takes the upload path. Members are not
//! a thing that moves; they are invited.

use std::collections::HashSet;
use std::time::Duration;

use dioxus::prelude::*;
use serde::Deserialize;

use crate::protocol::{ChannelKind, ClientMessage, Id, Permission};
use crate::state::{GatewayTx, use_app_state, use_gateway};

const API: &str = "https://discord.com/api/v10";
const CDN: &str = "https://cdn.discordapp.com";
const USER_AGENT: &str = "DiscordBot (https://github.com/ssotomayor/discordia, 0.1.0)";

/// Under the gateway's write bar of 30 per 10 s, with room for the rest of
/// the UI to keep talking.
const WRITE_GAP: Duration = Duration::from_millis(450);
const WAIT_FOR_SERVER: Duration = Duration::from_secs(20);

/// Both pictures ride one `SetGuildProfile` frame, because a second frame
/// with `None` would clear the first; together they must fit under 4 MiB.
const ICON_MAX: usize = 1_000_000;
const BANNER_MAX: usize = 2_400_000;
const EMOJI_MAX: usize = 340_000;

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct DcServer {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct DcGuild {
    name: String,
    icon: Option<String>,
    banner: Option<String>,
    description: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct DcChannel {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: u8,
    pub name: Option<String>,
    pub position: Option<i64>,
    pub parent_id: Option<String>,
    pub topic: Option<String>,
    pub rate_limit_per_user: Option<u32>,
    #[serde(default)]
    pub permission_overwrites: Vec<DcOverwrite>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct DcOverwrite {
    pub id: String,
    #[serde(default)]
    pub deny: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct DcRole {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub color: u32,
    #[serde(default)]
    pub position: i64,
    #[serde(default)]
    pub permissions: String,
    #[serde(default)]
    pub managed: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct DcEmoji {
    id: Option<String>,
    name: Option<String>,
    #[serde(default)]
    animated: bool,
}

const TEXT: u8 = 0;
const VOICE: u8 = 2;
const CATEGORY: u8 = 4;
const ANNOUNCEMENT: u8 = 5;
const STAGE: u8 = 13;
const FORUM: u8 = 15;
const MEDIA: u8 = 16;

const DC_SEND_MESSAGES: u64 = 1 << 11;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedChannel {
    pub name: String,
    pub kind: ChannelKind,
    pub topic: Option<String>,
    pub slowmode_secs: u32,
    pub read_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedRole {
    pub name: String,
    pub color: Option<String>,
    pub permissions: Vec<Permission>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedEmoji {
    pub shortcode: String,
    pub image: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportPlan {
    pub name: String,
    pub description: Option<String>,
    pub icon: Option<String>,
    pub banner: Option<String>,
    pub channels: Vec<PlannedChannel>,
    pub roles: Vec<PlannedRole>,
    pub emojis: Vec<PlannedEmoji>,
    pub skipped: Vec<String>,
}

impl ImportPlan {
    fn writes(&self) -> usize {
        let profile =
            usize::from(self.description.is_some() || self.icon.is_some() || self.banner.is_some());
        let tweaks = self
            .channels
            .iter()
            .filter(|c| c.read_only || c.slowmode_secs > 0)
            .count();
        1 + profile + self.channels.len() + tweaks + 2 + self.roles.len() + self.emojis.len()
    }
}

/// Discord's order, flattened: uncategorised first, then each category by
/// position, and inside one the text channels before the voice ones.
pub fn flatten_channels(chans: &[DcChannel]) -> (Vec<PlannedChannel>, Vec<String>) {
    fn key(c: &DcChannel) -> (i64, u64) {
        (c.position.unwrap_or(0), c.id.parse().unwrap_or(0))
    }
    let known: HashSet<&str> = chans
        .iter()
        .filter(|c| c.kind == CATEGORY)
        .map(|c| c.id.as_str())
        .collect();
    let mut categories: Vec<&DcChannel> = chans.iter().filter(|c| c.kind == CATEGORY).collect();
    categories.sort_by_key(|c| key(c));

    let mut groups: Vec<Option<&str>> = vec![None];
    groups.extend(categories.iter().map(|c| Some(c.id.as_str())));

    let mut out = Vec::new();
    let mut forums = 0usize;
    let mut unknown = 0usize;
    for parent in groups {
        let in_group = chans.iter().filter(|c| {
            c.kind != CATEGORY && c.parent_id.as_deref().filter(|p| known.contains(p)) == parent
        });
        let mut text: Vec<&DcChannel> = Vec::new();
        let mut voice: Vec<&DcChannel> = Vec::new();
        for c in in_group {
            match c.kind {
                TEXT | ANNOUNCEMENT => text.push(c),
                VOICE | STAGE => voice.push(c),
                FORUM | MEDIA => forums += 1,
                _ => unknown += 1,
            }
        }
        text.sort_by_key(|c| key(c));
        voice.sort_by_key(|c| key(c));
        for c in text {
            out.push(PlannedChannel {
                name: c.name.clone().unwrap_or_else(|| "channel".into()),
                kind: ChannelKind::Text,
                topic: c.topic.clone().filter(|t| !t.trim().is_empty()),
                slowmode_secs: c.rate_limit_per_user.unwrap_or(0),
                read_only: c.kind == ANNOUNCEMENT || everyone_cannot_send(c),
            });
        }
        for c in voice {
            out.push(PlannedChannel {
                name: c.name.clone().unwrap_or_else(|| "voice".into()),
                kind: ChannelKind::Voice,
                topic: None,
                slowmode_secs: 0,
                read_only: false,
            });
        }
    }

    let mut skipped = Vec::new();
    if !categories.is_empty() {
        skipped.push(format!(
            "{} categories — the channel list here is flat, their order is kept",
            categories.len()
        ));
    }
    if forums > 0 {
        skipped.push(format!("{forums} forum channels"));
    }
    if unknown > 0 {
        skipped.push(format!(
            "{unknown} channels of a kind that has no equivalent"
        ));
    }
    if chans.iter().any(|c| !c.permission_overwrites.is_empty()) {
        skipped.push("per-channel permission overrides".into());
    }
    (out, skipped)
}

/// A channel that denies `@everyone` sending is read-only in the only sense
/// this side has. The fetcher tags that overwrite, since a channel object
/// does not carry the guild id the role shares.
fn everyone_cannot_send(c: &DcChannel) -> bool {
    c.permission_overwrites.iter().any(|o| {
        o.id.starts_with("everyone:") && o.deny.parse::<u64>().unwrap_or(0) & DC_SEND_MESSAGES != 0
    })
}

pub fn map_permissions(bits: &str) -> Vec<Permission> {
    let bits: u64 = bits.parse().unwrap_or(0);
    if bits & (1 << 3) != 0 {
        return vec![
            Permission::SendMessages,
            Permission::ReadMessageHistory,
            Permission::AddReactions,
            Permission::ManageChannels,
            Permission::ManageMessages,
            Permission::KickMembers,
            Permission::BanMembers,
            Permission::ManageRoles,
            Permission::ManageGuild,
            Permission::CreateInvite,
            Permission::ManageEmojis,
        ];
    }
    const TABLE: &[(u64, Permission)] = &[
        (1 << 0, Permission::CreateInvite),
        (1 << 1, Permission::KickMembers),
        (1 << 2, Permission::BanMembers),
        (1 << 4, Permission::ManageChannels),
        (1 << 5, Permission::ManageGuild),
        (1 << 6, Permission::AddReactions),
        (1 << 11, Permission::SendMessages),
        (1 << 13, Permission::ManageMessages),
        (1 << 16, Permission::ReadMessageHistory),
        (1 << 28, Permission::ManageRoles),
        (1 << 30, Permission::ManageEmojis),
    ];
    TABLE
        .iter()
        .filter(|(bit, _)| bits & bit != 0)
        .map(|(_, p)| *p)
        .collect()
}

pub fn color_hex(color: u32) -> Option<String> {
    (color != 0).then(|| format!("#{:06x}", color & 0xff_ff_ff))
}

/// Discord's `@everyone` and bot-managed roles are not roles anyone assigns.
/// Lowest position first, so the hierarchy comes out in creation order.
pub fn plan_roles(guild_id: &str, roles: &[DcRole]) -> (Vec<PlannedRole>, usize) {
    let mut kept: Vec<&DcRole> = roles
        .iter()
        .filter(|r| r.id != guild_id && !r.managed)
        .collect();
    kept.sort_by_key(|r| (r.position, r.id.parse::<u64>().unwrap_or(0)));
    let managed = roles.iter().filter(|r| r.managed).count();
    (
        kept.into_iter()
            .map(|r| PlannedRole {
                name: r.name.clone(),
                color: color_hex(r.color),
                permissions: map_permissions(&r.permissions),
            })
            .collect(),
        managed,
    )
}

pub fn shortcode_of(name: &str) -> Option<String> {
    let code: String = name
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(32)
        .collect();
    (code.len() >= 2).then_some(code)
}

async fn discord_get<T: serde::de::DeserializeOwned>(
    http: &reqwest::Client,
    token: &str,
    path: &str,
) -> Result<T, String> {
    let url = format!("{API}{path}");
    for attempt in 0..2 {
        let res = http
            .get(&url)
            .header("Authorization", format!("Bot {token}"))
            .header("User-Agent", USER_AGENT)
            .send()
            .await
            .map_err(|e| format!("Discord unreachable: {e}"))?;
        let status = res.status();
        if status.as_u16() == 429 && attempt == 0 {
            let wait = res
                .headers()
                .get("Retry-After")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<f64>().ok())
                .unwrap_or(1.0);
            tokio::time::sleep(Duration::from_secs_f64(wait.min(10.0))).await;
            continue;
        }
        return match status.as_u16() {
            200 => res
                .json::<T>()
                .await
                .map_err(|e| format!("Discord answered something unexpected: {e}")),
            401 => Err("Discord rejected that token".into()),
            403 => Err("the bot is not in that server, or cannot see it".into()),
            404 => Err("Discord has no such server".into()),
            _ => Err(format!("Discord answered {status}")),
        };
    }
    Err("Discord is rate limiting; try again in a moment".into())
}

/// A CDN picture as a data URL under `max_len`, trying smaller renditions
/// before giving up on it.
async fn fetch_picture(
    http: &reqwest::Client,
    path: &str,
    sizes: &[u32],
    max_len: usize,
) -> Option<String> {
    for size in sizes {
        let url = format!("{CDN}{path}?size={size}");
        let Ok(res) = http.get(&url).header("User-Agent", USER_AGENT).send().await else {
            return None;
        };
        if !res.status().is_success() {
            return None;
        }
        let mime = res
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.split(';').next().unwrap_or(s).trim().to_string())
            .filter(|m| m.starts_with("image/"))
            .unwrap_or_else(|| {
                if path.ends_with(".gif") {
                    "image/gif".into()
                } else {
                    "image/png".into()
                }
            });
        let Ok(bytes) = res.bytes().await else {
            return None;
        };
        use base64::Engine;
        let data_url = format!(
            "data:{mime};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&bytes)
        );
        if data_url.len() <= max_len {
            return Some(data_url);
        }
    }
    None
}

fn hash_ext(hash: &str) -> &'static str {
    if hash.starts_with("a_") {
        ".gif"
    } else {
        ".png"
    }
}

pub async fn list_servers(token: &str) -> Result<Vec<DcServer>, String> {
    let http = reqwest::Client::new();
    let mut servers: Vec<DcServer> = discord_get(&http, token, "/users/@me/guilds").await?;
    servers.sort_by_key(|s| s.name.to_lowercase());
    Ok(servers)
}

pub async fn read_plan(token: &str, server_id: &str) -> Result<ImportPlan, String> {
    let http = reqwest::Client::new();
    let guild: DcGuild = discord_get(&http, token, &format!("/guilds/{server_id}")).await?;
    let mut channels: Vec<DcChannel> =
        discord_get(&http, token, &format!("/guilds/{server_id}/channels")).await?;
    let roles: Vec<DcRole> =
        discord_get(&http, token, &format!("/guilds/{server_id}/roles")).await?;
    let emojis: Vec<DcEmoji> =
        discord_get(&http, token, &format!("/guilds/{server_id}/emojis")).await?;

    // The @everyone overwrite is the one keyed by the guild id; tag it so
    // the flattener, which never sees the guild, can tell it apart.
    for c in &mut channels {
        for o in &mut c.permission_overwrites {
            if o.id == server_id {
                o.id = format!("everyone:{server_id}");
            }
        }
    }

    let (planned_channels, mut skipped) = flatten_channels(&channels);
    let (planned_roles, managed) = plan_roles(server_id, &roles);
    if managed > 0 {
        skipped.push(format!("{managed} bot-managed roles"));
    }

    let icon = match &guild.icon {
        Some(h) => {
            fetch_picture(
                &http,
                &format!("/icons/{server_id}/{h}{}", hash_ext(h)),
                &[512, 256, 128],
                ICON_MAX,
            )
            .await
        }
        None => None,
    };
    let banner = match &guild.banner {
        Some(h) => {
            fetch_picture(
                &http,
                &format!("/banners/{server_id}/{h}{}", hash_ext(h)),
                &[1024, 512, 256],
                BANNER_MAX,
            )
            .await
        }
        None => None,
    };

    let mut planned_emojis = Vec::new();
    let mut seen = HashSet::new();
    let mut lost_emojis = 0usize;
    for e in &emojis {
        let (Some(id), Some(name)) = (&e.id, &e.name) else {
            continue;
        };
        let Some(code) = shortcode_of(name) else {
            lost_emojis += 1;
            continue;
        };
        if !seen.insert(code.clone()) {
            lost_emojis += 1;
            continue;
        }
        let ext = if e.animated { ".gif" } else { ".png" };
        match fetch_picture(&http, &format!("/emojis/{id}{ext}"), &[128, 64], EMOJI_MAX).await {
            Some(image) => planned_emojis.push(PlannedEmoji {
                shortcode: code,
                image,
            }),
            None => lost_emojis += 1,
        }
    }
    if lost_emojis > 0 {
        skipped.push(format!(
            "{lost_emojis} emojis that would not fetch or had no usable name"
        ));
    }
    skipped.push("stickers, threads, webhooks, members".into());

    Ok(ImportPlan {
        name: guild.name,
        description: guild.description.filter(|d| !d.trim().is_empty()),
        icon,
        banner,
        channels: planned_channels,
        roles: planned_roles,
        emojis: planned_emojis,
        skipped,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    Idle,
    Working {
        stage: String,
        done: usize,
        total: usize,
    },
    Done {
        guild_id: Id,
        summary: String,
    },
    Failed(String),
}

async fn pause() {
    tokio::time::sleep(WRITE_GAP).await;
}

/// Waits for the server's answer to show up in state, or gives up.
async fn wait_for<T>(
    state: Signal<crate::state::AppState>,
    mut probe: impl FnMut(&crate::state::AppState) -> Option<T>,
) -> Option<T> {
    let deadline = tokio::time::Instant::now() + WAIT_FOR_SERVER;
    loop {
        if let Some(found) = probe(&state.read()) {
            return Some(found);
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

pub async fn run_import(
    plan: ImportPlan,
    gateway: GatewayTx,
    state: Signal<crate::state::AppState>,
    mut progress: Signal<Progress>,
) {
    let total = plan.writes();
    let mut done = 0usize;
    let mut step = |stage: &str, done: usize| {
        progress.set(Progress::Working {
            stage: stage.to_string(),
            done,
            total,
        });
    };

    let Some(me) = state.read().identity.as_ref().map(|i| i.pubkey.clone()) else {
        progress.set(Progress::Failed("not signed in".into()));
        return;
    };
    let before: HashSet<Id> = state.read().guilds.iter().map(|g| g.id).collect();

    step("Creating the guild", done);
    gateway.send(ClientMessage::CreateGuild {
        name: plan.name.clone(),
        template: None,
    });
    done += 1;
    let Some(gid) = wait_for(state, |s| {
        s.guilds
            .iter()
            .find(|g| g.owner_pubkey == me && !before.contains(&g.id))
            .map(|g| g.id)
    })
    .await
    else {
        progress.set(Progress::Failed(
            "the server did not create the guild; it may have refused (guild limit?) — see the toast"
                .into(),
        ));
        return;
    };
    let template_channels: Vec<Id> = state
        .read()
        .channels
        .iter()
        .filter(|c| c.guild_id == gid)
        .map(|c| c.id)
        .collect();
    pause().await;

    if plan.description.is_some() || plan.icon.is_some() || plan.banner.is_some() {
        step("Setting name, picture and banner", done);
        gateway.send(ClientMessage::SetGuildProfile {
            guild_id: gid,
            name: None,
            description: plan.description.clone(),
            icon_image: plan.icon.clone(),
            banner: plan.banner.clone(),
        });
        done += 1;
        pause().await;
    }

    for (i, c) in plan.channels.iter().enumerate() {
        step(
            &format!("Creating channels ({}/{})", i + 1, plan.channels.len()),
            done,
        );
        gateway.send(ClientMessage::CreateChannel {
            guild_id: gid,
            name: c.name.clone(),
            kind: c.kind,
            topic: c.topic.clone(),
        });
        done += 1;
        pause().await;
    }

    // Created channels take positions after the template's, in send order,
    // so the n-th new one is the n-th planned one.
    let expected = template_channels.len() + plan.channels.len();
    let created = wait_for(state, |s| {
        let mut mine: Vec<_> = s
            .channels
            .iter()
            .filter(|c| c.guild_id == gid && !template_channels.contains(&c.id))
            .cloned()
            .collect();
        let all = s.channels.iter().filter(|c| c.guild_id == gid).count();
        (all >= expected).then(|| {
            mine.sort_by_key(|c| (c.position, c.id));
            mine
        })
    })
    .await
    .unwrap_or_default();

    let tweaks: Vec<_> = plan
        .channels
        .iter()
        .zip(created.iter())
        .filter(|(p, _)| p.read_only || p.slowmode_secs > 0)
        .collect();
    for (i, (p, c)) in tweaks.iter().enumerate() {
        step(
            &format!("Slowmode and read-only ({}/{})", i + 1, tweaks.len()),
            done,
        );
        gateway.send(ClientMessage::UpdateChannel {
            channel_id: c.id,
            name: c.name.clone(),
            topic: c.topic.clone(),
            read_only: p.read_only,
            position: c.position,
            slowmode_secs: p.slowmode_secs,
        });
        done += 1;
        pause().await;
    }

    let has_text = plan.channels.iter().any(|c| c.kind == ChannelKind::Text);
    let has_text_created = created.iter().any(|c| c.kind == ChannelKind::Text);
    step("Removing the placeholder channels", done);
    for id in &template_channels {
        let is_text = state
            .read()
            .channels
            .iter()
            .any(|c| c.id == *id && c.kind == ChannelKind::Text);
        if is_text && !(has_text && has_text_created) {
            continue;
        }
        gateway.send(ClientMessage::DeleteChannel { channel_id: *id });
        pause().await;
    }
    done += 2;

    for (i, r) in plan.roles.iter().enumerate() {
        step(
            &format!("Creating roles ({}/{})", i + 1, plan.roles.len()),
            done,
        );
        gateway.send(ClientMessage::CreateRole {
            guild_id: gid,
            name: r.name.clone(),
            color: r.color.clone(),
            permissions: r.permissions.clone(),
        });
        done += 1;
        pause().await;
    }

    for (i, e) in plan.emojis.iter().enumerate() {
        step(
            &format!("Uploading emojis ({}/{})", i + 1, plan.emojis.len()),
            done,
        );
        gateway.send(ClientMessage::CreateGuildEmoji {
            guild_id: gid,
            shortcode: e.shortcode.clone(),
            image: e.image.clone(),
        });
        done += 1;
        pause().await;
    }

    // Let the last answers land before counting what actually exists.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let (channels, roles, emojis) = {
        let s = state.read();
        (
            s.channels.iter().filter(|c| c.guild_id == gid).count(),
            s.roles_of(gid).len(),
            s.emojis_of(gid).len(),
        )
    };
    let summary = format!(
        "{channels} of {} channels, {roles} of {} roles, {emojis} of {} emojis",
        plan.channels.len(),
        plan.roles.len(),
        plan.emojis.len()
    );
    progress.set(Progress::Done {
        guild_id: gid,
        summary,
    });
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    Token,
    Pick(Vec<DcServer>),
    Reading(String),
    Preview(ImportPlan),
    Running,
}

const INPUT: &str = "w-full bg-transparent border border-[var(--border)] focus:border-[var(--accent)] rounded px-2 py-1.5 text-xs font-mono text-[var(--text)] outline-none transition-colors";
const BUTTON: &str = "px-3 py-1.5 rounded text-[10px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors disabled:opacity-30 disabled:cursor-not-allowed";
const QUIET: &str = "px-3 py-1.5 rounded text-[10px] uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--text-muted)] transition-colors";
const LABEL: &str = "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)]";

#[component]
pub fn DiscordImportDialog(on_close: EventHandler<()>) -> Element {
    let mut state = use_app_state();
    let gateway = use_gateway();
    let mut step = use_signal(|| Step::Token);
    let mut token = use_signal(String::new);
    let mut error = use_signal(|| None::<String>);
    let mut busy = use_signal(|| false);
    let progress = use_signal(|| Progress::Idle);

    let running = matches!(step(), Step::Running)
        && matches!(progress(), Progress::Working { .. } | Progress::Idle);
    let close = move |_| {
        if !running {
            on_close.call(());
        }
    };

    let mut find_servers = move || {
        let t = token().trim().to_string();
        if t.is_empty() {
            return;
        }
        busy.set(true);
        error.set(None);
        spawn(async move {
            match list_servers(&t).await {
                Ok(list) if list.is_empty() => error.set(Some(
                    "that bot is not in any server yet — invite it first".into(),
                )),
                Ok(list) => step.set(Step::Pick(list)),
                Err(e) => error.set(Some(e)),
            }
            busy.set(false);
        });
    };

    let mut pick = move |server: DcServer| {
        let t = token().trim().to_string();
        step.set(Step::Reading(server.name.clone()));
        error.set(None);
        spawn(async move {
            match read_plan(&t, &server.id).await {
                Ok(plan) => step.set(Step::Preview(plan)),
                Err(e) => {
                    error.set(Some(e));
                    step.set(Step::Token);
                }
            }
        });
    };

    let start = {
        let gateway = gateway.clone();
        move |plan: ImportPlan| {
            step.set(Step::Running);
            let gateway = gateway.clone();
            spawn(async move {
                run_import(plan, gateway, state, progress).await;
            });
        }
    };

    rsx! {
        div {
            class: "dxf-backdrop-in fixed inset-0 z-50 flex items-center justify-center bg-black/50",
            onclick: close,
            div {
                class: "dxf-modal-in w-[30rem] max-h-[85vh] flex flex-col bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg shadow-xl overflow-hidden",
                onclick: move |e| e.stop_propagation(),
                div { class: "px-4 py-3 border-b border-[var(--border)] flex items-center",
                    h3 { class: "text-sm font-medium text-[var(--accent)] flex-1", "Import a Discord server" }
                    if !running {
                        button {
                            class: "text-[var(--text-dim)] hover:text-[var(--text)] text-lg leading-none",
                            onclick: move |_| on_close.call(()),
                            "✕"
                        }
                    }
                }
                div { class: "flex-1 overflow-y-auto p-4 space-y-4",
                    if let Some(e) = error() {
                        div { class: "text-xs text-[var(--danger)] border border-[var(--danger)]/40 rounded px-3 py-2", "{e}" }
                    }
                    match step() {
                        Step::Token => rsx! {
                            div { class: "space-y-2 text-xs text-[var(--text-muted)] leading-relaxed",
                                p { "The shape of the server comes over: name, picture, banner, channels in order, roles, emojis. Members are invited, not moved; messages stay where they are." }
                                ol { class: "list-decimal pl-4 space-y-1",
                                    li { "Create a bot at discord.com/developers → New Application → Bot, and copy its token." }
                                    li { "Invite it to the server: OAuth2 → URL Generator, scope \"bot\", no permissions needed." }
                                    li { "Paste the token below. It stays on this computer and is forgotten when this closes." }
                                }
                            }
                            form {
                                class: "space-y-2",
                                onsubmit: move |_| find_servers(),
                                label { class: LABEL, "Bot token" }
                                input {
                                    class: INPUT,
                                    r#type: "password",
                                    autocomplete: "off",
                                    spellcheck: "false",
                                    value: "{token}",
                                    oninput: move |e| token.set(e.value()),
                                }
                                div { class: "flex justify-end gap-2",
                                    button { r#type: "button", class: QUIET, onclick: move |_| on_close.call(()), "Cancel" }
                                    button {
                                        r#type: "submit",
                                        class: BUTTON,
                                        disabled: busy() || token().trim().is_empty(),
                                        if busy() { "Asking Discord…" } else { "Find servers" }
                                    }
                                }
                            }
                        },
                        Step::Pick(servers) => rsx! {
                            div { class: LABEL, "Servers the bot is in" }
                            div { class: "space-y-1.5",
                                for s in servers.iter().cloned() {
                                    button {
                                        key: "{s.id}",
                                        class: "w-full text-left px-3 py-2 rounded border border-[var(--border)] hover:border-[var(--accent)] text-sm text-[var(--text)] transition-colors",
                                        onclick: move |_| pick(s.clone()),
                                        "{s.name}"
                                    }
                                }
                            }
                            div { class: "flex justify-start",
                                button { class: QUIET, onclick: move |_| step.set(Step::Token), "← Back" }
                            }
                        },
                        Step::Reading(name) => rsx! {
                            div { class: "text-xs text-[var(--text-muted)] py-6 text-center",
                                "Reading “{name}” and fetching its pictures…"
                            }
                        },
                        Step::Preview(plan) => {
                            let plan_for_start = plan.clone();
                            let mut start = start.clone();
                            rsx! {
                                div { class: "flex items-start gap-3",
                                    if let Some(icon) = plan.icon.clone() {
                                        img { class: "w-12 h-12 rounded-xl object-cover border border-[var(--border)]", src: "{icon}", alt: "" }
                                    }
                                    div { class: "min-w-0 flex-1",
                                        div { class: "text-sm font-medium text-[var(--text)] truncate", "{plan.name}" }
                                        if let Some(d) = plan.description.clone() {
                                            div { class: "text-xs text-[var(--text-muted)] line-clamp-2", "{d}" }
                                        }
                                    }
                                }
                                if let Some(banner) = plan.banner.clone() {
                                    img { class: "w-full h-20 rounded-lg object-cover border border-[var(--border)]", src: "{banner}", alt: "" }
                                }
                                div { class: "space-y-1",
                                    div { class: LABEL, "{plan.channels.len()} channels, in this order" }
                                    div { class: "max-h-36 overflow-y-auto text-xs text-[var(--text-muted)] font-mono space-y-0.5 pr-1",
                                        for (i, c) in plan.channels.iter().enumerate() {
                                            div { key: "{i}", class: "flex gap-2",
                                                span { class: "text-[var(--text-dim)] w-3 shrink-0",
                                                    if c.kind == ChannelKind::Voice { "🔊" } else { "#" }
                                                }
                                                span { class: "truncate", "{c.name}" }
                                                if c.read_only { span { class: "text-[var(--text-dim)]", "read-only" } }
                                                if c.slowmode_secs > 0 { span { class: "text-[var(--text-dim)]", "slow {c.slowmode_secs}s" } }
                                            }
                                        }
                                    }
                                }
                                div { class: "space-y-1",
                                    div { class: LABEL, "{plan.roles.len()} roles" }
                                    div { class: "flex flex-wrap gap-1",
                                        for (i, r) in plan.roles.iter().enumerate() {
                                            span {
                                                key: "{i}",
                                                class: "px-2 py-0.5 rounded-full border border-[var(--border)] text-[11px]",
                                                style: if let Some(c) = &r.color { format!("color: {c};") } else { String::new() },
                                                "{r.name}"
                                            }
                                        }
                                    }
                                }
                                div { class: "space-y-1",
                                    div { class: LABEL, "{plan.emojis.len()} emojis" }
                                    div { class: "flex flex-wrap gap-1",
                                        for e in plan.emojis.iter().take(24) {
                                            img { key: "{e.shortcode}", class: "w-6 h-6 object-contain", src: "{e.image}", title: ":{e.shortcode}:", alt: "" }
                                        }
                                        if plan.emojis.len() > 24 {
                                            span { class: "text-[11px] text-[var(--text-dim)] self-center", "+{plan.emojis.len() - 24}" }
                                        }
                                    }
                                }
                                if !plan.skipped.is_empty() {
                                    div { class: "space-y-1",
                                        div { class: LABEL, "Not imported" }
                                        ul { class: "list-disc pl-4 text-xs text-[var(--text-muted)] space-y-0.5",
                                            for (i, s) in plan.skipped.iter().enumerate() {
                                                li { key: "{i}", "{s}" }
                                            }
                                        }
                                    }
                                }
                                p { class: "text-[11px] text-[var(--text-dim)] leading-relaxed",
                                    "About {plan.writes()} changes, sent one at a time so the server does not throttle them."
                                }
                                div { class: "flex justify-between",
                                    button { class: QUIET, onclick: move |_| step.set(Step::Token), "← Back" }
                                    button { class: BUTTON, onclick: move |_| start(plan_for_start.clone()), "Import" }
                                }
                            }
                        },
                        Step::Running => match progress() {
                            Progress::Working { stage, done, total } => {
                                let pct = (done * 100).checked_div(total).unwrap_or(0).min(100);
                                rsx! {
                                    div { class: "space-y-2 py-4",
                                        div { class: "text-xs text-[var(--text-muted)]", "{stage}" }
                                        div { class: "h-1.5 w-full rounded-full bg-[var(--bg2)] overflow-hidden",
                                            div { class: "h-full bg-[var(--accent)] transition-all", style: "width: {pct}%;" }
                                        }
                                        div { class: "text-[10px] text-[var(--text-dim)]", "{done} of {total}" }
                                    }
                                }
                            }
                            Progress::Done { guild_id, summary } => rsx! {
                                div { class: "space-y-3 py-2",
                                    div { class: "text-sm text-[var(--text)]", "Imported." }
                                    div { class: "text-xs text-[var(--text-muted)]", "{summary}" }
                                    p { class: "text-[11px] text-[var(--text-dim)]",
                                        "Anything the server refused showed as a toast while it ran. Share an invite to bring people over."
                                    }
                                    div { class: "flex justify-end gap-2",
                                        button { class: QUIET, onclick: move |_| on_close.call(()), "Close" }
                                        button {
                                            class: BUTTON,
                                            onclick: move |_| {
                                                state.write().selected_guild = Some(guild_id);
                                                state.write().dm_mode = false;
                                                on_close.call(());
                                            },
                                            "Open guild"
                                        }
                                    }
                                }
                            },
                            Progress::Failed(e) => rsx! {
                                div { class: "space-y-3 py-2",
                                    div { class: "text-xs text-[var(--danger)]", "{e}" }
                                    div { class: "flex justify-end",
                                        button { class: QUIET, onclick: move |_| on_close.call(()), "Close" }
                                    }
                                }
                            },
                            Progress::Idle => rsx! {
                                div { class: "text-xs text-[var(--text-muted)] py-6 text-center", "Starting…" }
                            },
                        },
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(id: &str, kind: u8, name: &str, pos: i64, parent: Option<&str>) -> DcChannel {
        DcChannel {
            id: id.into(),
            kind,
            name: Some(name.into()),
            position: Some(pos),
            parent_id: parent.map(Into::into),
            topic: None,
            rate_limit_per_user: None,
            permission_overwrites: Vec::new(),
        }
    }

    /// Discord draws uncategorised channels first, then categories by
    /// position, text above voice inside each. A forum has no home here.
    #[test]
    fn channels_flatten_in_discord_draw_order() {
        let chans = vec![
            ch("30", CATEGORY, "Later", 1, None),
            ch("20", CATEGORY, "First", 0, None),
            ch("5", VOICE, "Lounge", 0, Some("20")),
            ch("4", TEXT, "general", 1, Some("20")),
            ch("3", TEXT, "rules", 0, Some("20")),
            ch("2", TEXT, "welcome", 0, None),
            ch("6", ANNOUNCEMENT, "news", 0, Some("30")),
            ch("7", FORUM, "help", 1, Some("30")),
            ch("8", TEXT, "orphan", 0, Some("999")),
        ];
        let (flat, skipped) = flatten_channels(&chans);
        let names: Vec<&str> = flat.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            ["welcome", "orphan", "rules", "general", "Lounge", "news"]
        );
        assert_eq!(flat[4].kind, ChannelKind::Voice);
        assert!(flat[5].read_only, "an announcement channel is read-only");
        assert!(skipped.iter().any(|s| s.contains("1 forum")), "{skipped:?}");
        assert!(
            skipped.iter().any(|s| s.contains("2 categories")),
            "{skipped:?}"
        );
    }

    #[test]
    fn everyone_denied_send_is_read_only() {
        let mut c = ch("1", TEXT, "readme", 0, None);
        c.permission_overwrites.push(DcOverwrite {
            id: "everyone:123456789012345678".into(),
            deny: (1u64 << 11).to_string(),
        });
        let (flat, _) = flatten_channels(&[c]);
        assert!(flat[0].read_only);
    }

    #[test]
    fn administrator_is_everything_and_bits_map_one_to_one() {
        assert_eq!(map_permissions("8").len(), 11);
        let bits = (1u64 << 1) | (1 << 2) | (1 << 13) | (1 << 40);
        let mapped = map_permissions(&bits.to_string());
        assert_eq!(
            mapped,
            vec![
                Permission::KickMembers,
                Permission::BanMembers,
                Permission::ManageMessages
            ]
        );
        assert!(map_permissions("not a number").is_empty());
    }

    #[test]
    fn roles_skip_everyone_and_bots_and_keep_hierarchy_order() {
        let roles = vec![
            DcRole {
                id: "g".into(),
                name: "@everyone".into(),
                color: 0,
                position: 0,
                permissions: "0".into(),
                managed: false,
            },
            DcRole {
                id: "2".into(),
                name: "Admin".into(),
                color: 0xff0000,
                position: 5,
                permissions: "8".into(),
                managed: false,
            },
            DcRole {
                id: "3".into(),
                name: "Bot".into(),
                color: 0,
                position: 3,
                permissions: "0".into(),
                managed: true,
            },
            DcRole {
                id: "4".into(),
                name: "Member".into(),
                color: 0,
                position: 1,
                permissions: "0".into(),
                managed: false,
            },
        ];
        let (planned, managed) = plan_roles("g", &roles);
        let names: Vec<&str> = planned.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["Member", "Admin"]);
        assert_eq!(planned[1].color.as_deref(), Some("#ff0000"));
        assert_eq!(planned[0].color, None);
        assert_eq!(managed, 1);
    }

    #[test]
    fn shortcodes_are_what_the_server_accepts() {
        assert_eq!(shortcode_of("PepeHands").as_deref(), Some("pepehands"));
        assert_eq!(shortcode_of("kek-lol!").as_deref(), Some("keklol"));
        assert_eq!(shortcode_of("x").as_deref(), None);
        assert_eq!(shortcode_of(&"a".repeat(40)).map(|s| s.len()), Some(32));
    }
}
