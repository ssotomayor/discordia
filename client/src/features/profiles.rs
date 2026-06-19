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

/// Avatars are capped well below the server limit so frames stay small.
const MAX_AVATAR_BYTES: usize = 512_000;
/// Banners are wider, so a slightly larger cap.
const MAX_BANNER_BYTES: usize = 1_500_000;

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
    let status = snapshot
        .profile_of(&pubkey)
        .and_then(|p| p.status.clone())
        .unwrap_or_else(|| "online".into());
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

    rsx! {
        div {
            class: "dxf-backdrop-in fixed inset-0 z-50 flex items-center justify-center bg-black/50",
            onclick: move |_| state.write().profile_card = None,
            div {
                class: "dxf-modal-in w-72 bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg shadow-xl overflow-hidden",
                onclick: move |e| e.stop_propagation(),
                // Banner strip + overlapping avatar.
                if let Some(b) = banner {
                    div {
                        class: "h-20 bg-cover bg-center",
                        style: "background-image: url({b});",
                    }
                } else {
                    div { class: "h-16 bg-[var(--accent-soft)]" }
                }
                div { class: "px-4 pb-4 -mt-8",
                    Avatar {
                        pubkey: pubkey.clone(),
                        name: name.clone(),
                        size: "w-16 h-16 ring-2 ring-[var(--panel)]",
                        text: "text-xl",
                    }
                    div { class: "mt-2 flex items-center gap-1.5",
                        span { class: "w-2.5 h-2.5 rounded-full shrink-0", style: "background:{status_color(&status)};", title: "{status}" }
                        span { class: "text-base text-[var(--text)] font-medium",
                            "{name}"
                            span { class: "text-[var(--text-dim)] font-mono text-xs ml-1 font-normal", "#{disc}" }
                        }
                    }
                    if let Some(cs) = custom_status {
                        div { class: "mt-0.5 text-xs text-[var(--text-muted)] italic", "{cs}" }
                    }
                    div { class: "mt-0.5 text-[10px] text-[var(--text-dim)] font-mono break-all", "{pubkey}" }
                    if let Some(bio) = bio {
                        div { class: "mt-3 pt-3 border-t border-[var(--border)] text-sm text-[var(--text-muted)] whitespace-pre-wrap break-words",
                            "{bio}"
                        }
                    }
                    if !is_self {
                        button {
                            class: "mt-4 w-full py-2 rounded text-xs uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors",
                            onclick: move |_| {
                                gateway.send(ClientMessage::OpenDm { user_pubkey: dm_pubkey.clone() });
                                state.write().profile_card = None;
                            },
                            "Send Message"
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

    let mut open = use_signal(|| false);
    let mut avatar = use_signal::<Option<String>>(|| None);
    let mut banner = use_signal::<Option<String>>(|| None);
    let mut bio = use_signal(String::new);
    let mut status = use_signal(|| "online".to_string());
    let mut custom_status = use_signal(String::new);
    let mut err = use_signal::<Option<String>>(|| None);

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
                                        let mut avatar = avatar;
                                        let mut err = err;
                                        spawn(async move {
                                            let Some(file) = files.into_iter().next() else { return };
                                            match file.read_bytes().await {
                                                Ok(bytes) => {
                                                    if bytes.len() > MAX_AVATAR_BYTES {
                                                        err.set(Some("Image too large (max 512 KB).".into()));
                                                        return;
                                                    }
                                                    let mime = file
                                                        .content_type()
                                                        .filter(|m| m.starts_with("image/"))
                                                        .unwrap_or_else(|| "image/png".to_string());
                                                    let b64 = base64::engine::general_purpose::STANDARD
                                                        .encode(&bytes);
                                                    err.set(None);
                                                    avatar.set(Some(format!("data:{mime};base64,{b64}")));
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
                                        let mut banner = banner;
                                        let mut err = err;
                                        spawn(async move {
                                            let Some(file) = files.into_iter().next() else { return };
                                            match file.read_bytes().await {
                                                Ok(bytes) => {
                                                    if bytes.len() > MAX_BANNER_BYTES {
                                                        err.set(Some("Banner too large (max 1.5 MB).".into()));
                                                        return;
                                                    }
                                                    let mime = file
                                                        .content_type()
                                                        .filter(|m| m.starts_with("image/"))
                                                        .unwrap_or_else(|| "image/png".to_string());
                                                    let b64 = base64::engine::general_purpose::STANDARD
                                                        .encode(&bytes);
                                                    err.set(None);
                                                    banner.set(Some(format!("data:{mime};base64,{b64}")));
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
