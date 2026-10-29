use base64::Engine as _;
use dioxus::prelude::*;

use crate::identity::discriminator;
use crate::protocol::ClientMessage;
use crate::state::{use_app_state, use_gateway};

#[component]
fn ActivityCard(activity: crate::protocol::Activity) -> Element {
    rsx! {
        div { class: "mt-3 rounded-xl border border-[var(--edge)] p-3", style: "background-color: var(--bg2); display: flow-root;",
            div { class: "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)] mb-1.5",
                {crate::i18n::label(activity.kind.verb())}
            }
            div { style: "float: left; width: 48px; height: 48px; margin-right: 10px; border-radius: 8px; overflow: hidden; display: flex; align-items: center; justify-content: center;",
                if let Some(image) = activity.image.as_deref() {
                    img { src: "{image}", alt: "{activity.name}", style: "width: 48px; height: 48px; object-fit: cover;" }
                } else {
                    span { style: "font-size: 28px;", "🎮" }
                }
            }
            div { class: "text-sm text-[var(--text)] font-medium break-words", "{activity.name}" }
            if let Some(details) = activity.details.as_deref() {
                div { class: "text-xs text-[var(--text-muted)] break-words mt-0.5", "{details}" }
            }
            if let Some(st) = activity.state.as_deref() {
                div { class: "text-xs text-[var(--text-muted)] break-words mt-0.5", "{st}" }
            }
            if let Some(start) = activity.started_ms {
                ActivityElapsed { started_ms: start }
            }
        }
    }
}

#[component]
fn ActivityElapsed(started_ms: i64) -> Element {
    let text = elapsed_since(started_ms, chrono::Utc::now().timestamp_millis())
        .map(|e| crate::i18n::text("{0} elapsed", &[e]))
        .unwrap_or_default();
    rsx! { div {
        class: "text-xs text-[var(--text-dim)] font-mono mt-1",
        "data-activity-start": "{started_ms}",
        "data-activity-clock": "",
        "data-activity-suffix": crate::i18n::label("elapsed"),
        "{text}"
    } }
}

#[component]
fn ProfileActivity(pubkey: String) -> Element {
    let state = use_app_state();
    let activity = use_memo(use_reactive((&pubkey,), move |(pubkey,)| {
        state.read().activity_of(&pubkey).cloned()
    }));
    rsx! { if let Some(activity) = activity() { ActivityCard { activity } } }
}

/// "Playing Factorio" — the one line a member row has space for.
pub fn activity_line(a: &crate::protocol::Activity) -> String {
    format!("{} {}", crate::i18n::label(a.kind.verb()), a.name)
}

/// `1:04:12` past the hour mark, `04:12` under it, and nothing at all for a
/// clock that has not started or that claims to start in the future.
pub fn elapsed_since(started_ms: i64, now_ms: i64) -> Option<String> {
    if started_ms <= 0 || now_ms < started_ms {
        return None;
    }
    let total = (now_ms - started_ms) / 1000;
    let (h, m, sec) = (total / 3600, (total % 3600) / 60, total % 60);
    Some(if h > 0 {
        format!("{h}:{m:02}:{sec:02}")
    } else {
        format!("{m:02}:{sec:02}")
    })
}

pub fn status_color(status: &str) -> &'static str {
    match status {
        "away" => "var(--warn)",
        "dnd" => "var(--danger)",
        "offline" => "var(--text-dim)",
        _ => "var(--success)",
    }
}

pub(crate) const EMBED_MAX_BYTES: usize = crate::protocol::MAX_IMAGE_BYTES;

pub(crate) const IMAGE_HELP: &str = "PNG, JPEG, GIF or WebP. Originals: avatars up to 8 MB, banners up to 10 MB. Up to 2 MiB once cropped.";

pub(crate) fn check_image(bytes: &[u8], mime: &str, max_bytes: usize) -> Result<(), String> {
    if bytes.is_empty() {
        return Err("That file is empty.".into());
    }
    if !mime.starts_with("image/") {
        return Err(format!("That's a {mime} file, not an image. {IMAGE_HELP}"));
    }
    if bytes.len() > max_bytes {
        return Err(format!(
            "That image is {:.1} MB — too big to upload. {IMAGE_HELP}",
            bytes.len() as f64 / 1_000_000.0
        ));
    }
    Ok(())
}

pub(crate) fn to_data_url(bytes: &[u8], mime: &str) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    format!("data:{mime};base64,{b64}")
}

pub(crate) fn data_url_bytes(url: &str) -> Vec<u8> {
    url.split_once(";base64,")
        .and_then(|(_, b64)| base64::engine::general_purpose::STANDARD.decode(b64).ok())
        .unwrap_or_default()
}

pub(crate) fn image_mime(reported: Option<String>) -> String {
    reported
        .filter(|m| m.starts_with("image/"))
        .unwrap_or_else(|| "image/png".to_string())
}

fn read_profile_image(path: &std::path::Path, limit: usize) -> Result<String, String> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|_| "Couldn't read that file.")?;
    let size = file
        .metadata()
        .map_err(|_| "Couldn't read that file.")?
        .len();
    if size > limit as u64 {
        return Err(format!(
            "That image is {:.1} MB — too big to upload. {IMAGE_HELP}",
            size as f64 / 1_000_000.0
        ));
    }
    let mut bytes = Vec::new();
    file.take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "Couldn't read that file.")?;
    let format = image::guess_format(&bytes).map_err(|_| "That's not a supported image.")?;
    let mime = format.to_mime_type();
    check_image(&bytes, mime, limit)?;
    Ok(to_data_url(&bytes, mime))
}

fn pick_profile_image(
    limit: usize,
    editing: Signal<Option<String>>,
    error: Signal<Option<String>>,
    mut generation: Signal<u64>,
) {
    let request = generation.peek().wrapping_add(1);
    generation.set(request);
    spawn(async move {
        load_profile_image(
            request,
            generation,
            editing,
            error,
            async {
                crate::file_dialog::pick_file(&[(
                    crate::i18n::label("Image"),
                    crate::file_dialog::IMAGE_EXTENSIONS,
                )])
                .await
            },
            move |file| async move {
                tokio::task::spawn_blocking(move || read_profile_image(&file, limit))
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|result| result)
            },
        )
        .await;
    });
}

async fn load_profile_image<T, R: std::future::Future<Output = Result<String, String>>>(
    request: u64,
    generation: Signal<u64>,
    mut editing: Signal<Option<String>>,
    mut error: Signal<Option<String>>,
    pick: impl std::future::Future<Output = Result<Option<T>, String>>,
    read: impl FnOnce(T) -> R,
) {
    if *generation.peek() != request {
        return;
    }
    let picked = pick.await;
    if *generation.peek() != request {
        return;
    }
    let result = match picked {
        Ok(Some(file)) => read(file).await,
        Ok(None) => return,
        Err(message) => Err(message),
    };
    if *generation.peek() != request {
        return;
    }
    match result {
        Ok(image) => {
            error.set(None);
            editing.set(Some(image));
        }
        Err(message) => error.set(Some(message)),
    }
}

/// The server stores the picture and hands back an address, so the only
/// thing a client ever sends is the bytes themselves — never a link for every
/// other member's webview to go and fetch.
pub(crate) fn embed_image(cropped: String) -> (Option<String>, Option<String>) {
    let n = data_url_bytes(&cropped).len();
    if n > EMBED_MAX_BYTES {
        return (
            None,
            Some(format!(
                "This image is {n} bytes after cropping — the limit is 2 MiB (2,097,152 bytes)."
            )),
        );
    }
    (Some(cropped), None)
}

fn use_profile_image(
    pubkey: &str,
    banner: bool,
    account: bool,
) -> Memo<Option<crate::image_assets::ImageSource>> {
    let state = use_app_state();
    let pubkey = pubkey.to_owned();
    use_memo(use_reactive(
        (&pubkey, &account),
        move |(pubkey, account)| {
            let s = state.read();
            let personal = s
                .account_pictures
                .as_ref()
                .filter(|(owner, _)| owner == &pubkey)
                .and_then(|(_, pictures)| pictures.avatar.as_deref())
                .map(String::as_str);
            let address = if account {
                personal.or_else(|| s.profiles.get(&pubkey)?.avatar.as_deref())
            } else if banner {
                s.banner_address(&pubkey)
            } else {
                s.avatar_address(&pubkey)
            }?;
            let resolved = s.media_src(address)?;
            if let Some((owner, pictures)) = &s.account_pictures
                && owner == &pubkey
            {
                let cached = if banner {
                    &pictures.banner
                } else {
                    &pictures.avatar
                };
                if let Some(data) = cached
                    .as_ref()
                    .filter(|data| std::ptr::eq(data.as_str(), address))
                {
                    return Some(crate::image_assets::source(data.clone()));
                }
            }
            let data = match address.strip_prefix("media:") {
                Some(blob) => s
                    .emoji_images
                    .get_shared(blob)
                    .filter(|data| !data.is_empty()),
                None => Some(std::sync::Arc::new(resolved.to_owned())),
            }?;
            Some(crate::image_assets::source(data))
        },
    ))
}

#[component]
fn ProfileBanner(pubkey: String, accent: String) -> Element {
    let image = use_profile_image(&pubkey, true, false);
    rsx! {
        if let Some(b) = image() {
            img { class: "h-24 w-full object-cover block", src: "{b}", alt: crate::i18n::text("banner", &[]) }
        } else {
            div { class: "h-24 w-full", style: "background: linear-gradient(150deg, {accent}, transparent 80%), var(--bg2);" }
        }
    }
}

#[component]
pub fn Avatar(
    #[props(into)] pubkey: String,
    #[props(into)] name: String,
    #[props(into)] size: String,
    #[props(into, default = "text-xs".to_string())] text: String,
    #[props(default)] account: bool,
) -> Element {
    let image = use_profile_image(&pubkey, false, account);
    let avatar = image();
    let initial = name.chars().next().unwrap_or('?').to_ascii_uppercase();

    rsx! {
        div {
            class: "dxf-avatar rounded-md border border-[var(--border)] overflow-hidden shrink-0 flex items-center justify-center text-[var(--accent)] font-medium {size} {text}",
            if let Some(url) = avatar {
                img { class: "w-full h-full object-cover", src: "{url}", alt: "{name}", draggable: false }
            } else {
                "{initial}"
            }
        }
    }
}

#[component]
pub fn ProfileCard(
    #[props(default)] embedded: bool,
    #[props(default)] peer: Option<String>,
) -> Element {
    let mut state = use_app_state();
    let nostr = use_context::<crate::nostr::service::NostrTx>();

    let profile = use_memo(move || {
        let snapshot = state.read();
        let pubkey = peer.clone().or_else(|| snapshot.profile_card.clone())?;
        let name = snapshot.display_name(&pubkey);
        let bio = snapshot.bio_of(&pubkey).map(str::to_owned);
        let status = snapshot.presence_of(&pubkey).to_string();
        let custom_status = snapshot
            .profile_of(&pubkey)
            .and_then(|p| p.custom_status.clone())
            .filter(|s| !s.trim().is_empty());
        let global = snapshot.global_xp_of(&pubkey).filter(|g| g.xp > 0);
        let is_self = snapshot
            .self_user
            .as_ref()
            .map(|u| u.pubkey == pubkey)
            .unwrap_or(false);
        drop(snapshot);

        let is_contact = state.read().contacts.contains(&pubkey);
        let xp = state
            .read()
            .selected_guild
            .filter(|_| !embedded)
            .and_then(|gid| {
                state
                    .read()
                    .members
                    .iter()
                    .find(|m| m.guild_id == gid && m.user.pubkey == pubkey)
                    .map(|m| m.xp)
            })
            .unwrap_or(0);
        let rules = state
            .read()
            .selected_guild
            .filter(|_| !embedded)
            .map(|gid| state.read().leveling_of(gid))
            .unwrap_or_default();
        let progress = crate::features::leveling::progress(&rules, xp);
        let member_roles: Vec<crate::protocol::Role> = {
            let s = state.read();
            s.selected_guild
                .filter(|_| !embedded)
                .map(|gid| {
                    let assigned = s
                        .members
                        .iter()
                        .find(|m| m.guild_id == gid && m.user.pubkey == pubkey)
                        .map(|m| m.roles.clone())
                        .unwrap_or_default();
                    let mut held: Vec<_> = s
                        .roles_of(gid)
                        .iter()
                        .filter(|r| assigned.contains(&r.id))
                        .cloned()
                        .collect();
                    held.sort_by_key(|r| r.position);
                    held
                })
                .unwrap_or_default()
        };

        Some((
            pubkey,
            name,
            bio,
            status,
            custom_status,
            global,
            is_self,
            is_contact,
            progress,
            member_roles,
        ))
    });
    let Some((
        pubkey,
        name,
        bio,
        status,
        custom_status,
        global,
        is_self,
        is_contact,
        progress,
        member_roles,
    )) = profile()
    else {
        return rsx! { Fragment {} };
    };
    let disc = discriminator(&pubkey);
    let dm_pubkey = pubkey.clone();
    let contact_pubkey = pubkey.clone();
    let nostr_contact = nostr.clone();
    let signature = crate::identity::color_signature(&pubkey, 15);
    let accent = crate::identity::signature_accent(&pubkey);
    let xp_pct = progress.pct;
    let xp_count = match progress.target {
        Some(target) => {
            crate::i18n::text("{0}/{1} XP", &[progress.xp.to_string(), target.to_string()])
        }
        None => crate::i18n::text("{0} XP", &[progress.xp.to_string()]),
    };
    let xp_next = match (progress.target, progress.next.clone()) {
        (Some(target), Some(next)) => {
            crate::i18n::text("{0} XP to {1}", &[(target - progress.xp).to_string(), next])
        }
        _ => crate::i18n::text("Highest rank", &[]),
    };
    let copy_pubkey = pubkey.clone();

    rsx! {
        div {
            class: if embedded { "panel-hover w-full h-full min-w-0 flex flex-col bg-[var(--panel)] border border-[var(--border)] rounded-xl overflow-hidden" } else { "dxf-backdrop-in fixed inset-0 z-50 flex items-center justify-center bg-black/50" },
            onclick: move |_| { if !embedded { state.write().profile_card = None; } },
            if embedded {
                div { class: "h-12 px-3.5 shrink-0 flex items-center border-b border-[var(--border)] font-semibold text-sm",
                    {crate::i18n::text("Profile", &[])}
                }
            }
            div {
                class: if embedded { "dxf-dm-profile dxf-no-drag w-full min-w-0 flex-1 min-h-0 overflow-y-auto bg-[var(--panel2)]" } else { "dxf-modal-in w-[22rem] bg-[var(--panel2)] border border-[var(--edge)] rounded-2xl shadow-2xl overflow-hidden" },
                onclick: move |e| e.stop_propagation(),
                ProfileBanner { pubkey: pubkey.clone(), accent: accent.clone() }
                div { class: "px-5 pb-5 -mt-10", style: "min-width: 0; overflow-wrap: anywhere;",
                    div { class: "inline-block rounded-2xl p-1", style: "background: var(--panel2);",
                        Avatar {
                            pubkey: pubkey.clone(),
                            name: name.clone(),
                            size: "w-20 h-20 rounded-xl",
                            text: "text-2xl",
                        }
                    }
                    div { class: "mt-3 flex items-center gap-2 flex-wrap",
                        span { class: "w-2.5 h-2.5 rounded-full shrink-0", style: "background-color:{status_color(&status)};", title: crate::i18n::message(&(status).to_string()) }
                        span { class: "dxf-display text-2xl font-bold", style: "color: {accent};",
                            "{name}"
                        }
                        span { class: "text-[var(--text-dim)] font-mono text-sm", "#{disc}" }
                        span { class: "flex items-center gap-1 px-2 py-0.5 rounded-md text-xs",
                            style: "background: color-mix(in srgb, var(--up) 12%, transparent); color: var(--up);",
                            {crate::i18n::text("✓ Key verified", &[])}
                        }
                    }
                    if let Some(cs) = custom_status {
                        div { class: "mt-1 text-sm text-[var(--text-muted)] italic", "{cs}" }
                    }
                    ProfileActivity { pubkey: pubkey.clone() }
                    div { class: "mt-3 rounded-xl border border-[var(--edge)] p-3", style: "background: var(--bg2);",
                        div { class: "flex items-center justify-between mb-1.5",
                            span { class: "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)]", {crate::i18n::text("Nostr public key", &[])} }
                            button {
                                class: "text-[10px] uppercase tracking-wider text-[var(--accent)] border border-[var(--edge)] rounded-md px-2 py-0.5 hover:border-[var(--accent)] transition-colors",
                                onclick: move |_| {
                                    crate::clipboard::copy_text(&copy_pubkey);
                                },
                                {crate::i18n::text("Copy", &[])}
                            }
                        }
                        div { class: "font-mono text-xs text-[var(--text-muted)] break-all leading-relaxed", "{pubkey}" }
                        div { class: "flex gap-1 mt-2.5",
                            for c in signature.iter() {
                                div { class: "h-2 flex-1 rounded-full", style: "background-color: {c};" }
                            }
                        }
                    }
                    if let Some(bio) = bio {
                        div { class: "mt-3 text-sm text-[var(--text-muted)] whitespace-pre-wrap break-words", "{bio}" }
                    }
                    if !member_roles.is_empty() {
                        div { class: "mt-3",
                            div { class: "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)] mb-1.5", {crate::i18n::text("Roles", &[])} }
                            div { class: "flex flex-wrap gap-1.5",
                                for role in member_roles.iter() {
                                    {
                                        let color = role.color.clone()
                                            .filter(|c| !c.trim().is_empty())
                                            .unwrap_or_else(|| "var(--text-muted)".into());
                                        rsx! {
                                            span {
                                                key: "{role.id}",
                                                class: "flex items-center gap-1.5 px-2 py-0.5 rounded-md border text-xs",
                                                style: "color: {color}; border-color: color-mix(in srgb, {color} 45%, transparent); background: color-mix(in srgb, {color} 10%, transparent);",
                                                span { class: "w-2 h-2 rounded-full shrink-0", style: "background-color: {color};" }
                                                "{role.name}"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if !embedded {
                    div { class: "mt-4 flex items-center gap-3",
                        title: crate::i18n::text("Earned on this server, counted by it", &[]),
                        span {
                            class: "dxf-display text-sm font-bold shrink-0",
                            style: match progress.color.clone() {
                                Some(c) => format!("color: {c};"),
                                None => "color: var(--accent);".to_string(),
                            },
                            "{progress.label}"
                        }
                        div { class: "flex-1 h-2 rounded-full overflow-hidden", style: "background: var(--bg2);",
                            div { class: "h-full rounded-full", style: "width: {xp_pct}%; background: linear-gradient(90deg, #8fb0ff, var(--accent));" }
                        }
                        span { class: "text-[10px] text-[var(--text-dim)] shrink-0", "{xp_count}" }
                    }
                    div { class: "mt-1 text-right text-[10px] text-[var(--text-dim)]", "{xp_next}" }
                    }
                    // Deliberately quiet, and deliberately labelled: this number
                    // is signed by the person it flatters and by nobody else.
                    if let Some(g) = global {
                        div { class: "mt-1.5 flex items-center gap-2",
                            span { class: "text-[10px] text-[var(--text-muted)] shrink-0",
                                {crate::i18n::text("Across servers: {0}", &[crate::features::leveling::global_label(g)])}
                            }
                            span {
                                class: "text-[9px] px-1.5 py-px rounded bg-white/[0.04] text-[var(--text-dim)] uppercase tracking-wider shrink-0",
                                title: crate::i18n::text("Published by this key to Nostr and checked by nobody. It gates nothing.", &[]),
                                {crate::i18n::text("self-reported", &[])}
                            }
                        }
                    }
                    if !embedded { crate::features::bot_commands::BotCommandsSection { bot_pubkey: pubkey.clone() } }
                    if !is_self {
                        button {
                            class: "mt-4 w-full py-2 rounded-xl text-xs border border-[var(--border)] text-[var(--text-muted)] hover:text-[var(--text)] transition-all",
                            title: if is_contact { crate::i18n::text("Remove from your Nostr contact list. This list is public.", &[]) } else { crate::i18n::text("Add to your Nostr contact list, so they follow you to any server. This list is public.", &[]) },
                            onclick: move |_| {
                                nostr_contact.send(crate::nostr::service::NostrCmd::SetContact {
                                    peer: contact_pubkey.clone(),
                                    keep: !is_contact,
                                });
                            },
                            if is_contact { {crate::i18n::text("✓ In your contacts", &[])} } else { {crate::i18n::text("+ Add contact", &[])} }
                        }
                        if !embedded { button {
                            class: "dxf-cta mt-2 w-full py-2.5 rounded-xl text-sm transition-all",
                            onclick: move |_| {
                                nostr.send(crate::nostr::service::NostrCmd::Open {
                                    peer: dm_pubkey.clone(),
                                });
                                state.write().profile_card = None;
                            },
                            {crate::i18n::text("Message", &[])}
                        } }
                    }
                }
            }
        }
    }
}

#[component]
/// The trigger is whatever the caller puts inside: the account row is the one
/// place your own name, face and key are already on screen, so it opens itself
/// rather than growing an icon beside it.
pub fn ProfileEditor(class: String, children: Element) -> Element {
    let identity = use_context::<crate::identity::Identity>();
    let identity_name = identity.display_name.clone();
    let rename = try_use_context::<super::account::RenameAccount>();
    let profile_owner = use_signal(|| identity.pubkey);
    let mut state = use_app_state();
    let gateway = use_gateway();
    let nostr = use_context::<crate::nostr::service::NostrTx>();

    let mut open = use_signal(|| false);
    let mut display_name = use_signal(String::new);
    let mut original_name = use_signal(String::new);
    let mut avatar = use_signal::<Option<String>>(|| None);
    let mut banner = use_signal::<Option<String>>(|| None);
    let mut bio = use_signal(String::new);
    let mut status = use_signal(|| "online".to_string());
    let mut custom_status = use_signal(String::new);
    let mut show_crown = use_signal(|| true);
    let mut show_operator = use_signal(|| true);
    let mut err = use_signal::<Option<String>>(|| None);
    let mut editing_avatar = use_signal(|| None::<String>);
    let mut editing_banner = use_signal(|| None::<String>);
    let mut image_generation = use_signal(|| 0_u64);
    let mut reset_images = move || {
        image_generation.with_mut(|n| *n = n.wrapping_add(1));
        editing_avatar.set(None);
        editing_banner.set(None);
    };
    let close = use_callback(move |()| {
        reset_images();
        open.set(false);
    });

    let mut load_current = move || {
        reset_images();
        let s = state.read();
        let name = s.display_name_or(&profile_owner.peek(), &identity_name);
        display_name.set(name.clone());
        original_name.set(name);
        let me = s.self_user.as_ref().map(|u| u.pubkey.clone());
        if let Some(pk) = me
            && let Some(p) = s.profile_of(&pk)
        {
            avatar.set(p.avatar.clone());
            banner.set(p.banner.clone());
            bio.set(p.bio.clone().unwrap_or_default());
            status.set(p.status.clone().unwrap_or_else(|| "online".into()));
            custom_status.set(p.custom_status.clone().unwrap_or_default());
            show_crown.set(!p.hide_guild_owner);
            show_operator.set(!p.hide_operator);
            return;
        }
        if let Some(local) = crate::profile::load(&profile_owner.peek()) {
            avatar.set(local.avatar);
            banner.set(local.banner);
            bio.set(local.bio.unwrap_or_default());
            status.set(local.status.unwrap_or_else(|| "online".into()));
            custom_status.set(local.custom_status.unwrap_or_default());
            show_crown.set(!local.hide_guild_owner);
            show_operator.set(!local.hide_operator);
        }
    };

    let mut save = move || {
        if let Some(rename) = rename {
            if display_name.peek().trim().is_empty() {
                return;
            }
            let name = crate::protocol::canonical_username(&display_name());
            if name != *original_name.peek() {
                if let Err(message) = rename.0.call(name.clone()) {
                    err.set(Some(message));
                    return;
                }
                original_name.set(name);
            }
        }
        let bio_val = bio();
        let bio_opt = {
            let t = bio_val.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.chars().take(280).collect::<String>())
            }
        };
        let custom_val = custom_status();
        let custom_opt = {
            let t = custom_val.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.chars().take(80).collect::<String>())
            }
        };
        let status_opt = Some(status());
        // The local copy is what gets re-sent to the *next* server, and a
        // `media:` address only means something to the server that issued it.
        let portable = |img: Option<String>| {
            img.map(|i| state.read().media_src(&i).map(str::to_string).unwrap_or(i))
        };
        let local = crate::profile::LocalProfile {
            avatar: portable(avatar()),
            banner: portable(banner()),
            bio: bio_opt.clone(),
            status: status_opt.clone(),
            custom_status: custom_opt.clone(),
            hide_guild_owner: !show_crown(),
            hide_operator: !show_operator(),
            name: crate::profile::load(&profile_owner.peek()).and_then(|p| p.name),
            updated_at: chrono::Utc::now().timestamp_millis(),
        };
        let kind = |v: &Option<String>| match v {
            None => "none".to_string(),
            Some(s) if s.starts_with("data:") => format!("data-url ({} chars)", s.len()),
            Some(s) => format!("url {s}"),
        };
        crate::dlog!(
            "[profile] save avatar={} banner={}",
            kind(&avatar()),
            kind(&banner())
        );
        if let Err(error) = crate::profile::save(&profile_owner.peek(), &local) {
            err.set(Some(error));
            return;
        }
        let updated_at = local.updated_at;
        state.write().account_pictures = Some((profile_owner.peek().clone(), local.clone().into()));
        nostr.send(crate::nostr::service::NostrCmd::PublishProfile(local));
        gateway.send(ClientMessage::SetProfile {
            avatar: avatar(),
            banner: banner(),
            bio: bio_opt,
            status: status_opt,
            custom_status: custom_opt,
            hide_guild_owner: !show_crown(),
            hide_operator: !show_operator(),
            updated_at,
        });
        close.call(());
    };

    let (owns_a_guild, is_operator) = {
        let s = state.read();
        let me = s.self_user.as_ref().map(|u| u.pubkey.as_str());
        let owns = me.is_some_and(|pk| s.guilds.iter().any(|g| g.owner_pubkey == pk));
        (owns, s.is_operator)
    };

    rsx! {
        button {
            class: "{class}",
            title: crate::i18n::text("Edit your profile", &[]),
            aria_label: crate::i18n::text("Edit your profile", &[]),
            onclick: move |_| { load_current(); err.set(None); open.set(true); },
            {children}
        }

        if let Some(src) = editing_avatar() {
            crate::features::image_editor::ImageEditor {
                src,
                shape: crate::features::image_editor::CropShape::Square,
                on_cancel: move |_| reset_images(),
                on_apply: move |cropped: String| {
                    editing_avatar.set(None);
                    let (val, note) = embed_image(cropped);
                    err.set(note);
                    avatar.set(val);
                },
            }
        }
        if let Some(src) = editing_banner() {
            crate::features::image_editor::ImageEditor {
                src,
                shape: crate::features::image_editor::CropShape::Banner,
                on_cancel: move |_| reset_images(),
                on_apply: move |cropped: String| {
                    editing_banner.set(None);
                    let (val, note) = embed_image(cropped);
                    err.set(note);
                    banner.set(val);
                },
            }
        }
        if open() {
            div {
                class: "dxf-backdrop-in fixed inset-0 z-50 flex items-center justify-center bg-black/50",
                onclick: move |_| close.call(()),
                div {
                    class: "dxf-modal-in w-80 max-h-[90vh] overflow-y-auto bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg shadow-xl p-4",
                    onclick: move |e| e.stop_propagation(),
                    h3 { class: "text-sm font-medium text-[var(--accent)] mb-3", {crate::i18n::text("Edit profile", &[])} }

                    if rename.is_some() {
                        label { class: "block mb-3 text-xs text-[var(--text-muted)]",
                            {crate::i18n::text("Display name", &[])}
                            input {
                                class: "w-full mt-1 bg-transparent border border-[var(--border)] focus:border-[var(--accent)] rounded px-2 py-1.5 text-sm text-[var(--text)] outline-none transition-colors",
                                r#type: "text",
                                aria_label: crate::i18n::text("Display name", &[]),
                                maxlength: crate::protocol::MAX_USERNAME_LEN as i64,
                                value: "{display_name}",
                                oninput: move |e| display_name.set(crate::protocol::truncate_username(&e.value())),
                            }
                            span { class: "block mt-1 text-[10px] text-[var(--text-dim)]", {crate::i18n::text("This changes your account name across Discordia.", &[])} }
                        }
                    }

                    div { class: "flex items-center gap-3 mb-3",
                        div { class: "w-16 h-16 rounded-md border border-[var(--border)] overflow-hidden flex items-center justify-center text-[var(--text-dim)] text-xs shrink-0",
                            if let Some(url) = avatar().and_then(|a| state.read().media_src(&a).map(str::to_string)) {
                                img { class: "w-full h-full object-cover", src: "{url}", alt: crate::i18n::text("avatar preview", &[]) }
                            } else {
                                {crate::i18n::text("none", &[])}
                            }
                        }
                        div { class: "flex flex-col gap-1",
                            button {
                                r#type: "button",
                                class: "px-3 py-1.5 rounded text-[10px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors cursor-pointer text-center",
                                onclick: move |_| pick_profile_image(crate::image_edit::MAX_AVATAR_INPUT_BYTES, editing_avatar, err, image_generation),
                                {crate::i18n::text("Choose image", &[])}
                            }
                            if avatar().is_some() {
                                button {
                                    r#type: "button",
                                    class: "px-3 py-1 text-[10px] uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--danger)] transition-colors",
                                    onclick: move |_| { reset_images(); avatar.set(None); },
                                    {crate::i18n::text("Remove", &[])}
                                }
                            }
                        }
                    }

                    div { class: "mb-3",
                        div { class: "text-[10px] uppercase tracking-wider text-[var(--text-muted)] mb-1", {crate::i18n::text("Banner", &[])} }
                        div { class: "h-16 rounded-md border border-[var(--border)] overflow-hidden bg-[var(--accent-soft)] flex items-center justify-center text-[var(--text-dim)] text-xs",
                            if let Some(url) = banner().and_then(|b| state.read().media_src(&b).map(str::to_string)) {
                                img { class: "w-full h-full object-cover", src: "{url}", alt: crate::i18n::text("banner preview", &[]) }
                            } else {
                                {crate::i18n::text("no banner", &[])}
                            }
                        }
                        div { class: "flex gap-2 mt-1",
                            button {
                                r#type: "button",
                                class: "px-3 py-1 rounded text-[10px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors cursor-pointer",
                                onclick: move |_| pick_profile_image(crate::image_edit::MAX_BANNER_INPUT_BYTES, editing_banner, err, image_generation),
                                {crate::i18n::text("Choose banner", &[])}
                            }
                            if banner().is_some() {
                                button {
                                    r#type: "button",
                                    class: "px-3 py-1 text-[10px] uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--danger)] transition-colors",
                                    onclick: move |_| { reset_images(); banner.set(None); },
                                    {crate::i18n::text("Remove", &[])}
                                }
                            }
                        }
                    }

                    div { class: "text-[10px] uppercase tracking-wider text-[var(--text-muted)] mb-1", {crate::i18n::text("Status", &[])} }
                    div { class: "flex gap-1 mb-3",
                        for (id, label, color) in [
                            ("online", "Online", "var(--success)"),
                            ("away", "Away", "var(--warn)"),
                            ("dnd", "Do not disturb", "var(--danger)"),
                        ] {
                            {
                                let selected = status() == id;
                                let ring = if selected { "border-[var(--accent)] text-[var(--text)]" } else { "border-[var(--border)] text-[var(--text-muted)]" };
                                rsx! {
                                    button {
                                        key: "{id}",
                                        r#type: "button",
                                        class: "flex items-center gap-1.5 px-2 py-1 rounded border text-[11px] transition-colors {ring}",
                                        onclick: move |_| status.set(id.to_string()),
                                        span { class: "w-2 h-2 rounded-full", style: "background-color:{color};" }
                                        {crate::i18n::label(label)}
                                    }
                                }
                            }
                        }
                    }
                    input {
                        class: "w-full mb-3 bg-transparent border border-[var(--border)] focus:border-[var(--accent)] rounded px-2 py-1.5 text-sm text-[var(--text)] outline-none transition-colors",
                        r#type: "text",
                        placeholder: crate::i18n::text("Custom status (optional)", &[]),
                        maxlength: 80,
                        value: "{custom_status}",
                        oninput: move |e| custom_status.set(e.value()),
                    }

                    textarea {
                        class: "w-full h-20 bg-transparent border border-[var(--border)] focus:border-[var(--accent)] rounded px-2 py-1.5 text-sm text-[var(--text)] outline-none resize-none transition-colors",
                        placeholder: crate::i18n::text("Bio (optional)", &[]),
                        maxlength: 280,
                        value: "{bio}",
                        oninput: move |e| bio.set(e.value()),
                    }

                    if owns_a_guild || is_operator {
                        div { class: "mt-2 flex flex-col gap-1.5 text-[11px] text-[var(--text)]",
                            if owns_a_guild {
                                label { class: "flex items-center gap-2 cursor-pointer",
                                    input {
                                        r#type: "checkbox",
                                        checked: show_crown(),
                                        oninput: move |e| show_crown.set(e.value() == "true"),
                                    }
                                    {crate::i18n::text("Show a crown on guilds I own", &[])}
                                }
                            }
                            if is_operator {
                                label { class: "flex items-center gap-2 cursor-pointer",
                                    input {
                                        r#type: "checkbox",
                                        checked: show_operator(),
                                        oninput: move |e| show_operator.set(e.value() == "true"),
                                    }
                                    {crate::i18n::text("Show that I run this server", &[])}
                                }
                            }
                        }
                    }

                    SavedEmojiSection {}

                    div { class: "mt-2 text-[10px] text-[var(--text-dim)]", {crate::i18n::label(IMAGE_HELP)} }
                    if let Some(e) = err() {
                        div { class: "mt-2 text-[10px] text-[var(--danger)]", {crate::i18n::message(&(e).to_string())} }
                    }

                    div { class: "mt-3 flex gap-2 justify-end",
                        button {
                            class: "px-3 py-1.5 rounded text-xs uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--text-muted)] transition-colors",
                            aria_label: crate::i18n::text("Cancel", &[]),
                            onclick: move |_| close.call(()),
                            {crate::i18n::text("Cancel", &[])}
                        }
                        button {
                            class: "px-3 py-1.5 rounded text-xs uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors",
                            aria_label: crate::i18n::text("Save", &[]),
                            disabled: rename.is_some() && display_name.read().trim().is_empty(),
                            onclick: move |_| save(),
                            {crate::i18n::text("Save", &[])}
                        }
                    }
                }
            }
        }
    }
}

/// Applies at once rather than on Save, like the picker's right-click that
/// fills it: neither is part of the profile anyone else receives.
#[component]
fn SavedEmojiSection() -> Element {
    let mut state = use_app_state();
    crate::features::chat::use_saved_emoji_pictures();
    let saved_emoji: Vec<(crate::settings::SavedEmoji, String)> = {
        let s = state.read();
        s.saved_emoji
            .iter()
            .map(|e| {
                let url = s.emoji_images.get(&e.image).cloned().unwrap_or_default();
                (e.clone(), url)
            })
            .collect()
    };
    rsx! {
        div { class: "mt-3 pt-3 border-t border-[var(--border)]",
            div { class: "text-[10px] uppercase tracking-wider text-[var(--text-muted)] mb-1", {crate::i18n::text("Saved emoji", &[])} }
            if saved_emoji.is_empty() {
                div { class: "text-xs text-[var(--text-dim)]",
                    {crate::i18n::text("Right-click a guild's emoji in a picker to keep it here. Saved emoji work in DMs.", &[])}
                }
            }
            div { class: "flex flex-col gap-1 max-h-40 overflow-y-auto",
                for (e, url) in saved_emoji.iter().cloned() {
                    {
                        let image = e.image.clone();
                        rsx! {
                            div { key: "{e.image}", class: "flex items-center gap-2 text-xs",
                                span { class: "w-6 flex items-center justify-center shrink-0",
                                    if url.is_empty() {
                                        span { class: "text-[8px] text-[var(--text-dim)]", "…" }
                                    } else {
                                        img { src: "{url}", style: "height:1.4em;width:auto;" }
                                    }
                                }
                                span { class: "font-mono text-[var(--text)] shrink-0", ":{e.shortcode}:" }
                                span { class: "text-[var(--text-dim)] truncate flex-1", title: "{e.guild_name}", "{e.guild_name}" }
                                button {
                                    r#type: "button",
                                    class: "text-[10px] uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--danger)] transition-colors shrink-0",
                                    title: crate::i18n::text("Remove from my emoji", &[]),
                                    onclick: move |_| state.write().remove_saved_emoji(&image),
                                    "✕"
                                }
                            }
                        }
                    }
                }
            }
            div { class: "mt-2",
                crate::features::connect::BlobServerField {}
            }
        }
    }
}

#[cfg(test)]
mod profile_name_tests {
    use super::*;
    use dioxus::core::{AttributeValue, ElementId, Mutation};
    use std::{any::Any, cell::RefCell, rc::Rc};

    fn labeled(edits: &[Mutation], label: &str) -> ElementId {
        let label = crate::i18n::text(label, &[]);
        edits
            .iter()
            .find_map(|edit| match edit {
                Mutation::SetAttribute {
                    name: "aria-label",
                    value: AttributeValue::Text(value),
                    id,
                    ..
                } if *value == label => Some(*id),
                _ => None,
            })
            .unwrap()
    }

    fn click(dom: &VirtualDom, id: ElementId) {
        let data: Rc<dyn Any> = Rc::new(dioxus::html::PlatformEventData::new(Box::new(
            dioxus::html::SerializedMouseData::default(),
        )));
        dom.runtime()
            .handle_event("click", Event::new(data, true), id);
    }

    fn input(dom: &VirtualDom, id: ElementId, value: &str) {
        let data: dioxus::html::SerializedFormData =
            serde_json::from_value(serde_json::json!({"value": value})).unwrap();
        let data: Rc<dyn Any> = Rc::new(dioxus::html::PlatformEventData::new(Box::new(data)));
        dom.runtime()
            .handle_event("input", Event::new(data, true), id);
    }

    #[test]
    fn in_server_name_editor_uses_live_name_and_preserves_failed_or_cancelled_edits() {
        dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
        type Requests = Rc<RefCell<Vec<String>>>;
        let requests: Requests = Rc::default();
        let mut dom = VirtualDom::new_with_props(
            |requests: Requests| {
                let state = use_signal(|| {
                    let mut s = crate::state::AppState::empty();
                    s.self_user = Some(crate::protocol::User {
                        pubkey: "profile-name-test".into(),
                        username: "Current name".into(),
                    });
                    s.profiles.insert(
                        "profile-name-test".into(),
                        crate::protocol::Profile {
                            pubkey: "profile-name-test".into(),
                            ..Default::default()
                        },
                    );
                    s
                });
                provide_context(state);
                use_context_provider(|| Signal::new(crate::settings::ClientSettings::default()));
                use_context_provider(|| {
                    let mut identity = crate::identity::Identity::restore_from_private_key(
                        "01".repeat(32),
                        "Stale name",
                    )
                    .unwrap();
                    identity.pubkey = "profile-name-test".into();
                    identity
                });
                use_context_provider(|| {
                    crate::state::GatewayTx(tokio::sync::mpsc::unbounded_channel().0)
                });
                use_context_provider(crate::nostr::service::NostrTx::test_channel);
                let rename = use_callback(move |name| {
                    requests.borrow_mut().push(name);
                    Err("rename failed".into())
                });
                provide_context(super::super::account::RenameAccount(rename));
                rsx! { ProfileEditor { class: "", "Profile" } }
            },
            requests.clone(),
        );
        let initial = dom.rebuild_to_vec();
        let trigger = labeled(&initial.edits, "Edit your profile");
        click(&dom, trigger);
        let opened = dom.render_immediate_to_vec();
        let field = labeled(&opened.edits, "Display name");
        let save = labeled(&opened.edits, "Save");
        let cancel = labeled(&opened.edits, "Cancel");
        assert!(opened.edits.iter().any(|edit| matches!(edit,
            Mutation::SetAttribute { name: "value", value: AttributeValue::Text(value), id, .. } if *id == field && *value == "Current name"
        )));
        input(&dom, field, " \u{202e}New name\u{2029} ");
        dom.render_immediate_to_vec();
        click(&dom, save);
        assert_eq!(*requests.borrow(), vec!["New name"]);
        assert!(
            dom.render_immediate_to_vec()
                .edits
                .iter()
                .any(|edit| matches!(edit,
                    Mutation::CreateTextNode { value, .. } if *value == "rename failed"
                ))
        );
        input(&dom, field, "   ");
        dom.render_immediate_to_vec();
        click(&dom, save);
        assert_eq!(requests.borrow().len(), 1);
        click(&dom, cancel);
        dom.render_immediate_to_vec();
        click(&dom, trigger);
        let reopened = dom.render_immediate_to_vec();
        let field = labeled(&reopened.edits, "Display name");
        assert!(reopened.edits.iter().any(|edit| matches!(edit,
            Mutation::SetAttribute { name: "value", value: AttributeValue::Text(value), id, .. } if *id == field && *value == "Current name"
        )));
        assert_eq!(requests.borrow().len(), 1);
    }
}

#[cfg(test)]
mod picker_race_tests {
    use super::*;
    use futures_util::poll;
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
    };
    use tokio::sync::oneshot;

    #[derive(Clone, Copy)]
    struct Draft {
        generation: Signal<u64>,
        avatar: Signal<Option<String>>,
        banner: Signal<Option<String>>,
        error: Signal<Option<String>>,
    }

    fn fixture() -> (VirtualDom, Draft) {
        type Export = Rc<RefCell<Option<Draft>>>;
        let export: Export = Rc::default();
        let mut dom = VirtualDom::new_with_props(
            |export: Export| {
                let draft = Draft {
                    generation: use_signal(|| 1),
                    avatar: use_signal(|| None),
                    banner: use_signal(|| None),
                    error: use_signal(|| None),
                };
                *export.borrow_mut() = Some(draft);
                rsx! {}
            },
            export.clone(),
        );
        dom.rebuild_in_place();
        let draft = export.borrow().unwrap();
        (dom, draft)
    }

    #[tokio::test]
    async fn a_newer_avatar_or_banner_wins_over_an_older_read_or_error() {
        for result in [Ok("old image".into()), Err("old error".into())] {
            for banner in [false, true] {
                let (_dom, mut draft) = fixture();
                let (finish, reading) = oneshot::channel();
                let mut old = Box::pin(load_profile_image(
                    1,
                    draft.generation,
                    draft.avatar,
                    draft.error,
                    std::future::ready(Ok(Some(()))),
                    |_| async move { reading.await.unwrap() },
                ));
                assert!(poll!(old.as_mut()).is_pending());
                draft.generation.set(2);
                let target = if banner { draft.banner } else { draft.avatar };
                load_profile_image(
                    2,
                    draft.generation,
                    target,
                    draft.error,
                    std::future::ready(Ok(Some(()))),
                    |_| std::future::ready(Ok("latest image".into())),
                )
                .await;
                finish.send(result.clone()).unwrap();
                old.await;
                assert_eq!(target.peek().as_deref(), Some("latest image"));
                assert!(draft.error.peek().is_none());
                if banner {
                    assert!(draft.avatar.peek().is_none());
                }
            }
        }
    }

    #[tokio::test]
    async fn a_stale_picker_never_reads_its_file_or_overwrites_the_current_error() {
        for picked in [Ok(Some(())), Ok(None), Err("old picker error".into())] {
            let (_dom, mut draft) = fixture();
            let reads = Rc::new(Cell::new(0));
            let counted = reads.clone();
            let (finish, picking) = oneshot::channel();
            let mut old = Box::pin(load_profile_image(
                1,
                draft.generation,
                draft.avatar,
                draft.error,
                async move { picking.await.unwrap() },
                move |_| {
                    counted.set(counted.get() + 1);
                    std::future::ready(Ok("old image".into()))
                },
            ));
            assert!(poll!(old.as_mut()).is_pending());
            draft.generation.set(2);
            load_profile_image::<(), _>(
                2,
                draft.generation,
                draft.banner,
                draft.error,
                std::future::ready(Err("current picker error".into())),
                |_| std::future::ready(Ok("unused".into())),
            )
            .await;
            finish.send(picked).unwrap();
            old.await;
            assert_eq!(reads.get(), 0);
            assert!(draft.avatar.peek().is_none());
            assert_eq!(draft.error.peek().as_deref(), Some("current picker error"));
        }
    }

    #[tokio::test]
    async fn cancelling_the_latest_selection_does_not_restore_an_older_pending_image() {
        let (_dom, mut draft) = fixture();
        let (finish, reading) = oneshot::channel();
        let mut old = Box::pin(load_profile_image(
            1,
            draft.generation,
            draft.avatar,
            draft.error,
            std::future::ready(Ok(Some(()))),
            |_| async move { reading.await.unwrap() },
        ));
        assert!(poll!(old.as_mut()).is_pending());
        draft.generation.set(2);
        load_profile_image::<(), _>(
            2,
            draft.generation,
            draft.avatar,
            draft.error,
            std::future::ready(Ok(None)),
            |_| std::future::ready(Err("cancelled pick must not be read".into())),
        )
        .await;
        finish.send(Ok("old image".into())).unwrap();
        old.await;
        assert!(draft.avatar.peek().is_none());
        assert!(draft.error.peek().is_none());
    }
}

#[cfg(test)]
mod account_avatar_tests {
    #[test]
    fn native_profile_picker_reads_an_extensionless_jpeg_and_keeps_it_crop_ready() {
        let root =
            std::env::temp_dir().join(format!("profile-picker-{}", crate::protocol::Id::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("selected-document");
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(4, 4)
            .write_to(&mut bytes, image::ImageFormat::Jpeg)
            .unwrap();
        std::fs::write(&path, bytes.get_ref()).unwrap();
        let url = read_profile_image(&path, crate::image_edit::MAX_AVATAR_INPUT_BYTES).unwrap();
        assert!(url.starts_with("data:image/jpeg;base64,"));
        assert_eq!(data_url_bytes(&url), *bytes.get_ref());
        assert!(crate::image_edit::decode(&url, crate::image_edit::MAX_AVATAR_INPUT_BYTES).is_ok());
        assert!(
            read_profile_image(&path, 16)
                .unwrap_err()
                .contains("too big")
        );
        std::fs::write(&path, b"This is not an image").unwrap();
        assert!(
            read_profile_image(&path, 1024)
                .unwrap_err()
                .contains("not a supported image")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    use super::*;
    use dioxus::core::{AttributeValue, Mutation};
    use std::{cell::RefCell, rc::Rc};

    #[test]
    fn cached_account_avatar_renders_and_updates_without_a_gateway() {
        type Export = Rc<RefCell<Option<Signal<crate::state::AppState>>>>;
        let export: Export = Rc::new(RefCell::new(None));
        let mut dom = VirtualDom::new_with_props(
            |export: Export| {
                let state = use_signal(|| {
                    let mut state = crate::state::AppState::empty();
                    state.account_pictures = Some((
                        "self".into(),
                        crate::profile::LocalProfile {
                            avatar: Some("data:image/png;base64,Zmlyc3Q=".into()),
                            ..Default::default()
                        }
                        .into(),
                    ));
                    state.profiles.insert(
                        "self".into(),
                        crate::protocol::Profile {
                            pubkey: "self".into(),
                            avatar: None,
                            ..Default::default()
                        },
                    );
                    state
                });
                provide_context(state);
                *export.borrow_mut() = Some(state);
                rsx! { Avatar { pubkey: "self", name: "Self", size: "w-8 h-8", account: true } }
            },
            export.clone(),
        );
        let has_source = |edits: &[Mutation], expected: &str| {
            edits.iter().any(|edit| matches!(edit,
            Mutation::SetAttribute { name: "src", value: AttributeValue::Text(src), .. } if *src == expected
        ))
        };
        let initial = dom.rebuild_to_vec().edits;
        assert!(initial.iter().any(|edit| matches!(
            edit,
            Mutation::SetAttribute {
                name: "draggable",
                value: AttributeValue::Bool(false),
                ..
            }
        )));
        assert!(has_source(&initial, "data:image/png;base64,Zmlyc3Q="));
        let mut state = export.borrow().unwrap();
        state.write().account_pictures = Some((
            "self".into(),
            crate::profile::LocalProfile {
                avatar: Some("data:image/png;base64,bmV3".into()),
                ..Default::default()
            }
            .into(),
        ));
        assert!(has_source(
            &dom.render_immediate_to_vec().edits,
            "data:image/png;base64,bmV3"
        ));
    }
}
