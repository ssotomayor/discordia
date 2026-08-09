//! Profile UI — the reusable `Avatar`, the click-to-view `ProfileCard`, and
//! the self-service `ProfileEditor`. Profiles are client-owned (see
//! `crate::profile`) and looked up by pubkey from `AppState.profiles`.

use base64::Engine as _;
use dioxus::prelude::*;

use crate::identity::discriminator;
use crate::protocol::ClientMessage;
use crate::state::{use_app_state, use_gateway};

/// CSS color for a presence status string.
pub fn status_color(status: &str) -> &'static str {
    match status {
        "away" => "var(--warn)",
        "dnd" => "var(--danger)",
        "offline" => "var(--text-dim)",
        _ => "var(--success)",
    }
}

/// Sanity ceiling on how big a file we'll even read for upload.
pub(crate) const MAX_UPLOAD_BYTES: usize = 15_000_000;
/// Largest image we'll embed as a data URL when Blossom is unavailable (kept
/// under the server's data-URL cap).
///
/// This is the number that actually matters to a user: an image under it always
/// works, because the embed fallback can carry it even when no media server is
/// reachable. Above it we are betting on Blossom, which may be down, may not
/// accept anonymous uploads, or may not be configured at all.
pub(crate) const EMBED_MAX_BYTES: usize = 2_000_000;

/// The formats and size we tell users about, in one place so every picker in
/// the app says the same thing.
pub(crate) const IMAGE_HELP: &str =
    "PNG, JPEG, GIF or WebP. Under 2 MB always works; larger needs a reachable Blossom media server.";

/// Pre-flight an image the user just picked, without uploading it.
///
/// Returns `Err(message)` with something the user can act on. The point is to
/// fail here rather than let the upload path fail later with a note about
/// Blossom, which tells someone who just wanted to set a guild icon nothing
/// they can use.
pub(crate) fn check_image(bytes: &[u8], mime: &str) -> Result<(), String> {
    if bytes.is_empty() {
        return Err("That file is empty.".into());
    }
    if !mime.starts_with("image/") {
        return Err(format!("That's a {mime} file, not an image. {IMAGE_HELP}"));
    }
    if bytes.len() > MAX_UPLOAD_BYTES {
        return Err(format!(
            "That image is {:.1} MB — too big to upload. {IMAGE_HELP}",
            bytes.len() as f64 / 1_000_000.0
        ));
    }
    Ok(())
}

/// Wrap raw bytes as a `data:` URL, which is what the crop editor and every
/// `<img src>` in the app want.
pub(crate) fn to_data_url(bytes: &[u8], mime: &str) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    format!("data:{mime};base64,{b64}")
}

/// Decode a `data:` URL back to bytes. Returns empty on anything malformed —
/// callers treat that as a failed upload, which is the honest outcome.
pub(crate) fn data_url_bytes(url: &str) -> Vec<u8> {
    url.split_once(";base64,")
        .and_then(|(_, b64)| base64::engine::general_purpose::STANDARD.decode(b64).ok())
        .unwrap_or_default()
}

/// The mime declared by a `data:` URL, defaulting to PNG.
pub(crate) fn data_url_mime(url: &str) -> String {
    url.strip_prefix("data:")
        .and_then(|rest| rest.split_once(';'))
        .map(|(mime, _)| mime.to_string())
        .filter(|m| m.starts_with("image/"))
        .unwrap_or_else(|| "image/png".to_string())
}

/// Best guess at an image mime type for a picked file.
///
/// The webview reports `File.type` from the extension and can hand back
/// `application/octet-stream` for anything it doesn't recognise. Passing that
/// through builds a `data:application/octet-stream;...` URL, which the server
/// rejects outright ("must be http(s) or data:image") — a confusing bounce for
/// what is a perfectly good picture. Fall back to PNG: browsers sniff the real
/// format from the bytes when rendering, and the server only checks the prefix.
pub(crate) fn image_mime(reported: Option<String>) -> String {
    reported
        .filter(|m| m.starts_with("image/"))
        .unwrap_or_else(|| "image/png".to_string())
}

/// Upload an image to Blossom, returning `(value, note)`: on success the value
/// is the Blossom URL; on failure it falls back to an embedded data URL (if it
/// fits) and a note explaining what happened. Shared with the guild-branding
/// editor (`guild_settings.rs`).
pub(crate) async fn image_to_ref(
    server: String,
    identity: crate::identity::Identity,
    bytes: Vec<u8>,
    mime: String,
) -> (Option<String>, Option<String>) {
    let n = bytes.len();
    eprintln!("[blossom] uploading {n} bytes ({mime}) to {server}");
    match crate::blossom::upload_blob(&server, bytes.clone(), &mime, &identity).await {
        Ok(url) => {
            eprintln!("[blossom] upload ok -> {url}");
            (Some(url), None)
        }
        Err(e) => {
            eprintln!("[blossom] upload failed: {e}");
            if n > EMBED_MAX_BYTES {
                (
                    None,
                    Some(format!(
                        "Couldn't upload to the media server ({e}), and this image is \
                         {:.1} MB — too large to embed instead. Pick one under 2 MB, or set \
                         a working Blossom server under Appearance.",
                        n as f64 / 1_000_000.0
                    )),
                )
            } else {
                let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                (
                    Some(format!("data:{mime};base64,{b64}")),
                    Some(format!(
                        "Media server unavailable ({e}) — embedded the image instead, which works \
                         but makes this guild's snapshot larger for everyone."
                    )),
                )
            }
        }
    }
}

/// Renders a user's avatar (looked up by pubkey) or, failing that, the first
/// letter of their name in a bordered box. `size` carries the Tailwind sizing
/// (e.g. "w-8 h-8") plus any extra classes like rings/opacity.
#[component]
pub fn Avatar(
    #[props(into)] pubkey: String,
    #[props(into)] name: String,
    #[props(into)] size: String,
    #[props(into, default = "text-xs".to_string())] text: String,
) -> Element {
    let state = use_app_state();
    let avatar = state.read().avatar_of(&pubkey).map(|s| s.to_string());
    let initial = name.chars().next().unwrap_or('?').to_ascii_uppercase();

    rsx! {
        div {
            class: "rounded-md border border-[var(--border)] overflow-hidden shrink-0 flex items-center justify-center text-[var(--accent)] font-medium {size} {text}",
            if let Some(url) = avatar {
                img { class: "w-full h-full object-cover", src: "{url}", alt: "{name}" }
            } else {
                "{initial}"
            }
        }
    }
}

/// Modal profile card, shown when `AppState.profile_card` is set (clicking a
/// member opens it). Large avatar, name, bio, and a Send-Message button.
#[component]
pub fn ProfileCard() -> Element {
    let mut state = use_app_state();
    let gateway = use_gateway();

    let snapshot = state.read();
    let Some(pubkey) = snapshot.profile_card.clone() else {
        return rsx! { Fragment {} };
    };
    let name = snapshot
        .user_of(&pubkey)
        .map(|u| u.username.clone())
        .unwrap_or_else(|| crate::identity::truncate_pubkey(&pubkey));
    let bio = snapshot
        .profile_of(&pubkey)
        .and_then(|p| p.bio.clone())
        .filter(|b| !b.trim().is_empty());
    let banner = snapshot.profile_of(&pubkey).and_then(|p| p.banner.clone());
    // Effective presence, not the self-set label: a user who is disconnected
    // reads "offline" here even if their profile still says "online".
    let status = snapshot.presence_of(&pubkey).to_string();
    let custom_status = snapshot
        .profile_of(&pubkey)
        .and_then(|p| p.custom_status.clone())
        .filter(|s| !s.trim().is_empty());
    let is_self = snapshot
        .self_user
        .as_ref()
        .map(|u| u.pubkey == pubkey)
        .unwrap_or(false);
    drop(snapshot);

    let disc = discriminator(&pubkey);
    let dm_pubkey = pubkey.clone();
    let signature = crate::identity::color_signature(&pubkey, 15);
    let accent = crate::identity::signature_accent(&pubkey);
    // Per-guild XP: show this member's level in the guild we're viewing.
    let xp = state
        .read()
        .selected_guild
        .and_then(|gid| {
            state
                .read()
                .members
                .iter()
                .find(|m| m.guild_id == gid && m.user.pubkey == pubkey)
                .map(|m| m.xp)
        })
        .unwrap_or(0);
    let (level, into, span) = crate::protocol::level_progress(xp);
    let xp_pct = (into as f64 / span.max(1) as f64 * 100.0) as u32;
    let copy_pubkey = pubkey.clone();

    rsx! {
        div {
            class: "dxf-backdrop-in fixed inset-0 z-50 flex items-center justify-center bg-black/50",
            onclick: move |_| state.write().profile_card = None,
            div {
                class: "dxf-modal-in w-[22rem] bg-[var(--panel2)] border border-[var(--edge)] rounded-2xl shadow-2xl overflow-hidden",
                onclick: move |e| e.stop_propagation(),
                // Banner tinted by the user's signature accent (or their image).
                if let Some(b) = banner {
                    img { class: "h-24 w-full object-cover block", src: "{b}", alt: "banner" }
                } else {
                    div { class: "h-24 w-full",
                        style: "background: linear-gradient(150deg, {accent}, transparent 80%), var(--bg2);"
                    }
                }
                div { class: "px-5 pb-5 -mt-10",
                    div { class: "inline-block rounded-2xl p-1", style: "background: var(--panel2);",
                        Avatar {
                            pubkey: pubkey.clone(),
                            name: name.clone(),
                            size: "w-20 h-20 rounded-xl",
                            text: "text-2xl",
                        }
                    }
                    div { class: "mt-3 flex items-center gap-2 flex-wrap",
                        span { class: "w-2.5 h-2.5 rounded-full shrink-0", style: "background:{status_color(&status)};", title: "{status}" }
                        span { class: "dxf-display text-2xl font-bold", style: "color: {accent};",
                            "{name}"
                        }
                        span { class: "text-[var(--text-dim)] font-mono text-sm", "#{disc}" }
                        span { class: "flex items-center gap-1 px-2 py-0.5 rounded-md text-xs",
                            style: "background: color-mix(in srgb, var(--up) 12%, transparent); color: var(--up);",
                            "✓ Key verified"
                        }
                    }
                    if let Some(cs) = custom_status {
                        div { class: "mt-1 text-sm text-[var(--text-muted)] italic", "{cs}" }
                    }
                    // Public-key block with copy + color signature.
                    div { class: "mt-3 rounded-xl border border-[var(--edge)] p-3", style: "background: var(--bg2);",
                        div { class: "flex items-center justify-between mb-1.5",
                            span { class: "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)]", "Nostr public key" }
                            button {
                                class: "text-[10px] uppercase tracking-wider text-[var(--accent)] border border-[var(--edge)] rounded-md px-2 py-0.5 hover:border-[var(--accent)] transition-colors",
                                onclick: move |_| {
                                    let js = format!("navigator.clipboard && navigator.clipboard.writeText('{copy_pubkey}');");
                                    let _ = document::eval(&js);
                                },
                                "Copy"
                            }
                        }
                        div { class: "font-mono text-xs text-[var(--text-muted)] break-all leading-relaxed", "{pubkey}" }
                        div { class: "flex gap-1 mt-2.5",
                            for c in signature.iter() {
                                div { class: "h-2 flex-1 rounded-full", style: "background: {c};" }
                            }
                        }
                    }
                    if let Some(bio) = bio {
                        div { class: "mt-3 text-sm text-[var(--text-muted)] whitespace-pre-wrap break-words", "{bio}" }
                    }
                    // Level + XP bar.
                    div { class: "mt-4 flex items-center gap-3",
                        span { class: "dxf-display text-sm font-bold text-[var(--accent)] shrink-0", "Lv {level}" }
                        div { class: "flex-1 h-2 rounded-full overflow-hidden", style: "background: var(--bg2);",
                            div { class: "h-full rounded-full", style: "width: {xp_pct}%; background: linear-gradient(90deg, #8fb0ff, var(--accent));" }
                        }
                        span { class: "text-[10px] text-[var(--text-dim)] shrink-0", "{into}/{span}" }
                    }
                    if !is_self {
                        button {
                            class: "dxf-cta mt-4 w-full py-2.5 rounded-xl text-sm transition-all",
                            onclick: move |_| {
                                gateway.send(ClientMessage::OpenDm { user_pubkey: dm_pubkey.clone() });
                                state.write().profile_card = None;
                            },
                            "Message"
                        }
                    }
                }
            }
        }
    }
}

/// Self-service profile editor: pick an avatar (file → base64 data URL) and a
/// bio, then save locally and publish to the host. Rendered as a small button
/// that expands into a modal.
#[component]
pub fn ProfileEditor() -> Element {
    let state = use_app_state();
    let gateway = use_gateway();
    let identity = use_context::<crate::identity::Identity>();
    let settings = use_context::<Signal<crate::settings::ClientSettings>>();
    // Separate clones for the two file pickers (Identity isn't Copy).
    // One clone per crop dialog; the pickers themselves no longer upload.
    let id_avatar_crop = identity.clone();
    let id_banner_crop = identity.clone();

    let mut open = use_signal(|| false);
    let mut avatar = use_signal::<Option<String>>(|| None);
    let mut banner = use_signal::<Option<String>>(|| None);
    let mut bio = use_signal(String::new);
    let mut status = use_signal(|| "online".to_string());
    let mut custom_status = use_signal(String::new);
    let mut err = use_signal::<Option<String>>(|| None);
    // A picked image waits in one of these while the user frames it; the crop
    // dialog uploads on accept. Two signals rather than one so the dialog knows
    // which shape to crop to and where the result belongs.
    let mut editing_avatar = use_signal(|| None::<String>);
    let mut editing_banner = use_signal(|| None::<String>);

    // Initialise the fields from our current profile each time the modal opens.
    let mut load_current = move || {
        let s = state.read();
        let me = s.self_user.as_ref().map(|u| u.pubkey.clone());
        if let Some(pk) = me {
            if let Some(p) = s.profile_of(&pk) {
                avatar.set(p.avatar.clone());
                banner.set(p.banner.clone());
                bio.set(p.bio.clone().unwrap_or_default());
                status.set(p.status.clone().unwrap_or_else(|| "online".into()));
                custom_status.set(p.custom_status.clone().unwrap_or_default());
                return;
            }
        }
        // Fall back to whatever is on disk.
        if let Some(local) = crate::profile::load() {
            avatar.set(local.avatar);
            banner.set(local.banner);
            bio.set(local.bio.unwrap_or_default());
            status.set(local.status.unwrap_or_else(|| "online".into()));
            custom_status.set(local.custom_status.unwrap_or_default());
        }
    };

    let mut save = move || {
        let bio_val = bio();
        let bio_opt = {
            let t = bio_val.trim();
            if t.is_empty() { None } else { Some(t.chars().take(280).collect::<String>()) }
        };
        let custom_val = custom_status();
        let custom_opt = {
            let t = custom_val.trim();
            if t.is_empty() { None } else { Some(t.chars().take(80).collect::<String>()) }
        };
        let status_opt = Some(status());
        let local = crate::profile::LocalProfile {
            avatar: avatar(),
            banner: banner(),
            bio: bio_opt.clone(),
            status: status_opt.clone(),
            custom_status: custom_opt.clone(),
        };
        let kind = |v: &Option<String>| match v {
            None => "none".to_string(),
            Some(s) if s.starts_with("data:") => format!("data-url ({} chars)", s.len()),
            Some(s) => format!("url {s}"),
        };
        eprintln!(
            "[profile] save avatar={} banner={}",
            kind(&avatar()),
            kind(&banner())
        );
        let _ = crate::profile::save(&local);
        gateway.send(ClientMessage::SetProfile {
            avatar: avatar(),
            banner: banner(),
            bio: bio_opt,
            status: status_opt,
            custom_status: custom_opt,
        });
        open.set(false);
    };

    rsx! {
        button {
            class: "w-7 h-7 flex items-center justify-center rounded text-[var(--text-muted)] hover:text-[var(--accent)] transition-colors",
            title: "Edit your profile",
            onclick: move |_| { load_current(); err.set(None); open.set(true); },
            dangerous_inner_html: crate::features::icons::USER,
        }

        // Crop dialogs, rendered above the editor they were opened from.
        if let Some(src) = editing_avatar() {
            crate::features::image_editor::ImageEditor {
                src,
                shape: crate::features::image_editor::CropShape::Square,
                on_cancel: move |_| editing_avatar.set(None),
                on_apply: move |cropped: String| {
                    editing_avatar.set(None);
                    let identity = id_avatar_crop.clone();
                    let server = settings.read().blossom_server.clone();
                    spawn(async move {
                        let bytes = data_url_bytes(&cropped);
                        let mime = data_url_mime(&cropped);
                        let (val, note) = image_to_ref(server, identity, bytes, mime).await;
                        err.set(note);
                        avatar.set(val);
                    });
                },
            }
        }
        if let Some(src) = editing_banner() {
            crate::features::image_editor::ImageEditor {
                src,
                shape: crate::features::image_editor::CropShape::Banner,
                on_cancel: move |_| editing_banner.set(None),
                on_apply: move |cropped: String| {
                    editing_banner.set(None);
                    let identity = id_banner_crop.clone();
                    let server = settings.read().blossom_server.clone();
                    spawn(async move {
                        let bytes = data_url_bytes(&cropped);
                        let mime = data_url_mime(&cropped);
                        let (val, note) = image_to_ref(server, identity, bytes, mime).await;
                        err.set(note);
                        banner.set(val);
                    });
                },
            }
        }
        if open() {
            div {
                class: "dxf-backdrop-in fixed inset-0 z-50 flex items-center justify-center bg-black/50",
                onclick: move |_| open.set(false),
                div {
                    class: "dxf-modal-in w-80 bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg shadow-xl p-4",
                    onclick: move |e| e.stop_propagation(),
                    h3 { class: "text-sm font-medium text-[var(--accent)] mb-3", "Edit profile" }

                    div { class: "flex items-center gap-3 mb-3",
                        // Live preview of the chosen avatar.
                        div { class: "w-16 h-16 rounded-md border border-[var(--border)] overflow-hidden flex items-center justify-center text-[var(--text-dim)] text-xs shrink-0",
                            if let Some(url) = avatar() {
                                img { class: "w-full h-full object-cover", src: "{url}", alt: "avatar preview" }
                            } else {
                                "none"
                            }
                        }
                        div { class: "flex flex-col gap-1",
                            label {
                                class: "px-3 py-1.5 rounded text-[10px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors cursor-pointer text-center",
                                "Choose image"
                                input {
                                    r#type: "file",
                                    accept: "image/*",
                                    class: "hidden",
                                    onchange: move |evt: FormEvent| {
                                        let files = evt.files();
                                        let mut err = err;
                                        spawn(async move {
                                            let Some(file) = files.into_iter().next() else { return };
                                            match file.read_bytes().await {
                                                Ok(bytes) => {
                                                    let mime = image_mime(file.content_type());

                                                    if let Err(msg) = check_image(&bytes, &mime) {

                                                        err.set(Some(msg));

                                                        return;

                                                    }

                                                    // Frame it before uploading; `editing_avatar` drives the

                                                    // crop dialog, which uploads on accept.

                                                    editing_avatar.set(Some(to_data_url(&bytes, &mime)));
                                                }
                                                Err(_) => err.set(Some("Couldn't read that file.".into())),
                                            }
                                        });
                                    },
                                }
                            }
                            if avatar().is_some() {
                                button {
                                    r#type: "button",
                                    class: "px-3 py-1 text-[10px] uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--danger)] transition-colors",
                                    onclick: move |_| avatar.set(None),
                                    "Remove"
                                }
                            }
                        }
                    }

                    // Banner: wide preview + picker.
                    div { class: "mb-3",
                        div { class: "text-[10px] uppercase tracking-wider text-[var(--text-muted)] mb-1", "Banner" }
                        div { class: "h-16 rounded-md border border-[var(--border)] overflow-hidden bg-[var(--accent-soft)] flex items-center justify-center text-[var(--text-dim)] text-xs",
                            if let Some(url) = banner() {
                                img { class: "w-full h-full object-cover", src: "{url}", alt: "banner preview" }
                            } else {
                                "no banner"
                            }
                        }
                        div { class: "flex gap-2 mt-1",
                            label {
                                class: "px-3 py-1 rounded text-[10px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors cursor-pointer",
                                "Choose banner"
                                input {
                                    r#type: "file",
                                    accept: "image/*",
                                    class: "hidden",
                                    onchange: move |evt: FormEvent| {
                                        let files = evt.files();
                                        let mut err = err;
                                        spawn(async move {
                                            let Some(file) = files.into_iter().next() else { return };
                                            match file.read_bytes().await {
                                                Ok(bytes) => {
                                                    let mime = image_mime(file.content_type());

                                                    if let Err(msg) = check_image(&bytes, &mime) {

                                                        err.set(Some(msg));

                                                        return;

                                                    }

                                                    // Frame it before uploading; `editing_banner` drives the

                                                    // crop dialog, which uploads on accept.

                                                    editing_banner.set(Some(to_data_url(&bytes, &mime)));
                                                }
                                                Err(_) => err.set(Some("Couldn't read that file.".into())),
                                            }
                                        });
                                    },
                                }
                            }
                            if banner().is_some() {
                                button {
                                    r#type: "button",
                                    class: "px-3 py-1 text-[10px] uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--danger)] transition-colors",
                                    onclick: move |_| banner.set(None),
                                    "Remove"
                                }
                            }
                        }
                    }

                    // Presence status.
                    div { class: "text-[10px] uppercase tracking-wider text-[var(--text-muted)] mb-1", "Status" }
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
                                        span { class: "w-2 h-2 rounded-full", style: "background:{color};" }
                                        "{label}"
                                    }
                                }
                            }
                        }
                    }
                    input {
                        class: "w-full mb-3 bg-transparent border border-[var(--border)] focus:border-[var(--accent)] rounded px-2 py-1.5 text-sm text-[var(--text)] outline-none transition-colors",
                        r#type: "text",
                        placeholder: "Custom status (optional)",
                        maxlength: 80,
                        value: "{custom_status}",
                        oninput: move |e| custom_status.set(e.value()),
                    }

                    textarea {
                        class: "w-full h-20 bg-transparent border border-[var(--border)] focus:border-[var(--accent)] rounded px-2 py-1.5 text-sm text-[var(--text)] outline-none resize-none transition-colors",
                        placeholder: "Bio (optional)",
                        maxlength: 280,
                        value: "{bio}",
                        oninput: move |e| bio.set(e.value()),
                    }

                    // Same guidance as the guild branding pickers, from the
                    // same constant — the rules shouldn't be learned by trial.
                    div { class: "mt-2 text-[10px] text-[var(--text-dim)]", {IMAGE_HELP} }
                    if let Some(e) = err() {
                        div { class: "mt-2 text-[10px] text-[var(--danger)]", "{e}" }
                    }

                    div { class: "mt-3 flex gap-2 justify-end",
                        button {
                            class: "px-3 py-1.5 rounded text-xs uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--text-muted)] transition-colors",
                            onclick: move |_| open.set(false),
                            "Cancel"
                        }
                        button {
                            class: "px-3 py-1.5 rounded text-xs uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors",
                            onclick: move |_| save(),
                            "Save"
                        }
                    }
                }
            }
        }
    }
}
