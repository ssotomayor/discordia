//! "Guild settings" dialog: branding (description + icon/banner images),
//! visibility (public directory vs invite-only), the invite code, custom emoji,
//! and the ban list. Everything here needs `ManageGuild` (bans need
//! `BanMembers`, emoji need `ManageEmojis`); the server enforces — the dialog
//! just won't be reachable without the menu entry.
//!
//! Guild-facing copy says "guild", never "server". In this project a *server*
//! is the host you connect to (`dioxusfun-server`, "Server URL", the self-host
//! flow), so Discord's habit of calling a guild a server collides with a term
//! that already means something else here.

use base64::Engine as _;
use dioxus::prelude::*;

use crate::identity::truncate_pubkey;
use crate::protocol::{ClientMessage, GuildVisibility, Id, JoinGate};
use crate::state::{use_app_state, use_gateway};

#[component]
pub fn GuildSettingsDialog(guild_id: Id, on_close: EventHandler<()>) -> Element {
    let state = use_app_state();
    let gateway = use_gateway();
    let identity = use_context::<crate::identity::Identity>();
    let settings = use_context::<Signal<crate::settings::ClientSettings>>();

    let guild = use_memo(move || {
        state
            .read()
            .guilds
            .iter()
            .find(|g| g.id == guild_id)
            .cloned()
    });
    let invite = use_memo(move || state.read().invites.get(&guild_id).cloned());
    let bans = use_memo(move || {
        state
            .read()
            .bans
            .get(&guild_id)
            .cloned()
            .unwrap_or_default()
    });
    let can_ban = state
        .read()
        .can(guild_id, crate::protocol::Permission::BanMembers);
    let can_emojis = state
        .read()
        .can(guild_id, crate::protocol::Permission::ManageEmojis);

    // Fetch the invite + ban list once on open (fetch-on-open pattern).
    {
        let gw = gateway.clone();
        let fetch_bans = can_ban;
        use_hook(move || {
            gw.send(ClientMessage::CreateInvite {
                guild_id,
                rotate: false,
            });
            gw.send(ClientMessage::FetchAuditLog { guild_id });
            if fetch_bans {
                gw.send(ClientMessage::FetchBans { guild_id });
            }
        });
    }

    // Local edit buffer for the description (submitted on Save).
    let mut description = use_signal(|| {
        state
            .read()
            .guilds
            .iter()
            .find(|g| g.id == guild_id)
            .and_then(|g| g.description.clone())
            .unwrap_or_default()
    });
    // (is_problem, message). Success and failure looked identical before —
    // both silent — and rendering a "done" message in warning orange would be
    // its own small lie, so the flag drives the colour.
    let mut upload_note = use_signal(|| None::<(bool, String)>);
    let mut copied = use_signal(|| false);

    let Some(g) = guild() else {
        return rsx! { Fragment {} };
    };
    let is_private = matches!(g.visibility, GuildVisibility::Private);

    // Submit branding (full-replace: keep current images unless re-picked).
    let save_branding = {
        let gateway = gateway.clone();
        let icon_image = g.icon_image.clone();
        let banner = g.banner.clone();
        move |_| {
            let d = description().trim().to_string();
            gateway.send(ClientMessage::SetGuildProfile {
                guild_id,
                description: if d.is_empty() { None } else { Some(d) },
                icon_image: icon_image.clone(),
                banner: banner.clone(),
            });
        }
    };

    rsx! {
        div {
            class: "dxf-backdrop-in fixed inset-0 z-50 flex items-center justify-center bg-black/50",
            onclick: move |_| on_close.call(()),
            div {
                class: "dxf-modal-in w-[26rem] max-h-[80vh] flex flex-col bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg shadow-xl overflow-hidden",
                onclick: move |e| e.stop_propagation(),
                div { class: "px-4 py-3 border-b border-[var(--border)] flex items-center",
                    h3 { class: "text-sm font-medium text-[var(--accent)] flex-1", "Guild settings — {g.name}" }
                    button {
                        class: "text-[var(--text-dim)] hover:text-[var(--text)] text-lg leading-none",
                        onclick: move |_| on_close.call(()),
                        "✕"
                    }
                }
                div { class: "flex-1 overflow-y-auto p-3 space-y-4",

                    // ----- Branding -----
                    div {
                        div { class: "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)] mb-1.5",
                            "Branding"
                        }
                        textarea {
                            class: "w-full bg-transparent border border-[var(--border)] focus:border-[var(--accent)] rounded px-2 py-1 text-xs text-[var(--text)] outline-none transition-colors resize-none",
                            rows: 2,
                            maxlength: 280,
                            placeholder: "Describe this guild…",
                            value: "{description}",
                            oninput: move |e| description.set(e.value()),
                        }
                        // What is actually set right now. Without this the only
                        // way to tell an upload had worked was to go hunting for
                        // where the image is used.
                        div { class: "flex items-center gap-3 mt-2",
                            div { class: "shrink-0 text-center",
                                if let Some(src) = g.icon_image.clone() {
                                    img {
                                        class: "w-10 h-10 rounded-md object-cover border border-[var(--border)]",
                                        src: "{src}", alt: "guild icon",
                                    }
                                } else {
                                    div { class: "w-10 h-10 rounded-md border border-dashed border-[var(--border)] flex items-center justify-center text-[10px] text-[var(--text-dim)]",
                                        "none"
                                    }
                                }
                                div { class: "text-[9px] text-[var(--text-dim)] mt-0.5", "Icon" }
                            }
                            div { class: "flex-1 min-w-0 text-center",
                                if let Some(src) = g.banner.clone() {
                                    img {
                                        class: "w-full h-10 rounded-md object-cover border border-[var(--border)]",
                                        src: "{src}", alt: "guild banner",
                                    }
                                } else {
                                    div { class: "w-full h-10 rounded-md border border-dashed border-[var(--border)] flex items-center justify-center text-[10px] text-[var(--text-dim)]",
                                        "no banner"
                                    }
                                }
                                div { class: "text-[9px] text-[var(--text-dim)] mt-0.5",
                                    "Banner — shown above the channel list"
                                }
                            }
                        }
                        div { class: "flex gap-2 mt-2",
                            // Icon + banner pickers: upload to Blossom, fall
                            // back to an embedded data URL (same path as
                            // profile avatars), then push the whole profile.
                            ImagePickButton {
                                label: "Icon…",
                                shape: crate::features::image_editor::CropShape::Square,
                                onpicked: {
                                    let gateway = gateway.clone();
                                    let banner = g.banner.clone();
                                    move |(url, note): (Option<String>, Option<String>)| {
                                        let ok = url.is_some();
                                        // A silent success looked identical to a
                                        // silent failure; say which happened.
                                        upload_note.set(match note {
                                            Some(n) => Some((true, n)),
                                            None if ok => Some((false, "Icon updated.".into())),
                                            None => None,
                                        });
                                        if ok {
                                            gateway.send(ClientMessage::SetGuildProfile {
                                                guild_id,
                                                description: {
                                                    let d = description.peek().trim().to_string();
                                                    if d.is_empty() { None } else { Some(d) }
                                                },
                                                icon_image: url,
                                                banner: banner.clone(),
                                            });
                                        }
                                    }
                                },
                                identity: identity.clone(),
                                settings,
                            }
                            ImagePickButton {
                                label: "Banner…",
                                shape: crate::features::image_editor::CropShape::Banner,
                                onpicked: {
                                    let gateway = gateway.clone();
                                    let icon_image = g.icon_image.clone();
                                    move |(url, note): (Option<String>, Option<String>)| {
                                        let ok = url.is_some();
                                        upload_note.set(match note {
                                            Some(n) => Some((true, n)),
                                            None if ok => Some((false, "Banner updated.".into())),
                                            None => None,
                                        });
                                        if ok {
                                            gateway.send(ClientMessage::SetGuildProfile {
                                                guild_id,
                                                description: {
                                                    let d = description.peek().trim().to_string();
                                                    if d.is_empty() { None } else { Some(d) }
                                                },
                                                icon_image: icon_image.clone(),
                                                banner: url,
                                            });
                                        }
                                    }
                                },
                                identity: identity.clone(),
                                settings,
                            }
                            div { class: "flex-1" }
                            button {
                                class: "rounded px-3 py-1 text-[10px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors",
                                onclick: save_branding,
                                "Save"
                            }
                        }
                        // What's actually accepted. Without this the only way to
                        // learn the rules was to trip over them.
                        div { class: "text-[10px] text-[var(--text-dim)] mt-1",
                            "Icon: square works best. Banner: wide (about 4:1). "
                            {crate::features::profiles::IMAGE_HELP}
                        }
                        if let Some((problem, note)) = upload_note() {
                            div {
                                class: "text-[10px] mt-1",
                                style: if problem { "color: var(--warn);" } else { "color: var(--up);" },
                                "{note}"
                            }
                        }
                    }

                    // ----- Visibility -----
                    div { class: "border-t border-[var(--border)] pt-3",
                        div { class: "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)] mb-1.5",
                            "Visibility"
                        }
                        label { class: "flex items-center gap-2 text-xs text-[var(--text)] cursor-pointer select-none",
                            input {
                                r#type: "checkbox",
                                checked: is_private,
                                onchange: {
                                    let gateway = gateway.clone();
                                    move |_| {
                                        gateway.send(ClientMessage::SetGuildVisibility {
                                            guild_id,
                                            visibility: if is_private {
                                                GuildVisibility::Public
                                            } else {
                                                GuildVisibility::Private
                                            },
                                        });
                                    }
                                },
                            }
                            span { "Private (invite-only)" }
                        }
                        div { class: "text-[10px] text-[var(--text-dim)] mt-1",
                            "Private guilds are hidden from the browse directory; people join with the invite code below."
                        }
                    }

                    // ----- Message retention -----
                    div { class: "border-t border-[var(--edge)] pt-3",
                        div { class: "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)] mb-1.5",
                            "Message retention"
                        }
                        RetentionRow { guild_id, current: g.retention_days }
                        div { class: "text-[10px] text-[var(--text-dim)] mt-1",
                            "Messages older than this are deleted (hourly sweep). Blank keeps everything forever."
                        }
                    }

                    // ----- Community safety -----
                    div { class: "border-t border-[var(--border)] pt-3",
                        div { class: "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)] mb-1.5",
                            "Community safety"
                        }
                        SafetyControls {
                            guild_id,
                            gate: g.join_gate,
                            rules: g.rules.clone(),
                            panic_mode: g.panic_mode,
                        }
                    }

                    // ----- Invite code -----
                    div { class: "border-t border-[var(--border)] pt-3",
                        div { class: "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)] mb-1.5",
                            "Invite code"
                        }
                        div { class: "flex items-center gap-2",
                            code { class: "flex-1 px-2 py-1 rounded border border-[var(--border)] text-xs text-[var(--text)] select-all truncate",
                                {invite().unwrap_or_else(|| "…".into())}
                            }
                            button {
                                class: "rounded px-2 py-1 text-[10px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors",
                                onclick: move |_| {
                                    if let Some(code) = invite() {
                                        // The code is minted server-side, so it
                                        // is not ours to trust into a literal.
                                        let code =
                                            crate::features::screenshare::js_str(&code);
                                        let js = format!(
                                            "navigator.clipboard && navigator.clipboard.writeText({code});"
                                        );
                                        let _ = document::eval(&js);
                                        copied.set(true);
                                    }
                                },
                                if copied() { "Copied!" } else { "Copy" }
                            }
                            button {
                                class: "rounded px-2 py-1 text-[10px] uppercase tracking-wider text-[var(--warn)] border border-[var(--warn)]/40 hover:bg-[var(--warn)]/10 transition-colors",
                                title: "Invalidate the current code and mint a new one",
                                onclick: {
                                    let gateway = gateway.clone();
                                    move |_| {
                                        copied.set(false);
                                        gateway.send(ClientMessage::CreateInvite { guild_id, rotate: true });
                                    }
                                },
                                "Rotate"
                            }
                        }
                    }

                    // ----- Custom emoji -----
                    if can_emojis {
                        EmojiSettings { guild_id }
                    }

                    // ----- Bans -----
                    if can_ban {
                        div { class: "border-t border-[var(--border)] pt-3",
                            div { class: "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)] mb-1.5",
                                "Bans"
                            }
                            if bans().is_empty() {
                                div { class: "text-xs text-[var(--text-dim)]", "Nobody is banned." }
                            }
                            for banned in bans().iter().cloned() {
                                {
                                    let gw = gateway.clone();
                                    let pk = banned.pubkey.clone();
                                    rsx! {
                                        div {
                                            key: "{banned.pubkey}",
                                            class: "flex items-center gap-2 py-1",
                                            span { class: "text-xs text-[var(--text)] truncate flex-1",
                                                title: "{banned.pubkey}",
                                                "{banned.username} "
                                                span { class: "font-mono text-[10px] text-[var(--text-dim)]",
                                                    "{truncate_pubkey(&banned.pubkey)}"
                                                }
                                            }
                                            button {
                                                class: "text-[10px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] rounded px-2 py-0.5 hover:border-[var(--accent)] transition-colors",
                                                onclick: move |_| gw.send(ClientMessage::UnbanMember {
                                                    guild_id,
                                                    user_pubkey: pk.clone(),
                                                }),
                                                "Unban"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Retention editor: a days field (blank = keep forever) + Save.
#[component]
fn RetentionRow(guild_id: Id, current: Option<u32>) -> Element {
    let gateway = use_gateway();
    let mut draft = use_signal(|| current.map(|d| d.to_string()).unwrap_or_default());

    rsx! {
        div { class: "flex items-center gap-2",
            input {
                class: "w-24 bg-transparent border border-[var(--edge)] focus:border-[var(--accent)] rounded px-2 py-1 text-xs text-[var(--text)] outline-none transition-colors",
                r#type: "number",
                min: "1",
                max: "3650",
                placeholder: "forever",
                value: "{draft}",
                oninput: move |e| draft.set(e.value()),
            }
            span { class: "text-xs text-[var(--text-muted)] flex-1", "days" }
            button {
                class: "rounded px-3 py-1 text-[10px] uppercase tracking-wider text-[var(--accent)] border border-[var(--edge)] hover:border-[var(--accent)] transition-colors",
                onclick: move |_| {
                    let days = draft().trim().parse::<u32>().ok();
                    gateway.send(ClientMessage::SetGuildRetention { guild_id, days });
                },
                "Save"
            }
        }
    }
}

/// Join-gate + panic + audit-log controls. The gate/rules are edited locally
/// and pushed on Save; panic mode toggles immediately (it's an emergency
/// switch). The audit log is read from state (fetched on dialog open).
#[component]
fn SafetyControls(
    guild_id: Id,
    gate: JoinGate,
    rules: Option<String>,
    panic_mode: bool,
) -> Element {
    let state = use_app_state();
    let gateway = use_gateway();

    let mut gate_draft = use_signal(|| gate);
    let mut rules_draft = use_signal(|| rules.clone().unwrap_or_default());
    let entries = use_memo(move || {
        state
            .read()
            .audit_logs
            .get(&guild_id)
            .cloned()
            .unwrap_or_default()
    });

    let gate_value = match gate_draft() {
        JoinGate::Open => "open",
        JoinGate::Rules => "rules",
        JoinGate::Pow => "pow",
    };

    rsx! {
        // Join gate selector.
        div { class: "flex items-center gap-2",
            span { class: "text-xs text-[var(--text-muted)] flex-1", "New members must…" }
            select {
                class: "bg-[var(--panel-solid)] border border-[var(--border)] focus:border-[var(--accent)] rounded px-2 py-1 text-xs text-[var(--text)] outline-none",
                value: "{gate_value}",
                onchange: move |e| {
                    gate_draft.set(match e.value().as_str() {
                        "rules" => JoinGate::Rules,
                        "pow" => JoinGate::Pow,
                        _ => JoinGate::Open,
                    });
                },
                option { value: "open", "join freely" }
                option { value: "rules", "accept rules" }
                option { value: "pow", "solve a challenge" }
            }
        }
        // Rules editor — only meaningful for the Rules gate.
        if matches!(gate_draft(), JoinGate::Rules) {
            textarea {
                class: "w-full mt-2 bg-transparent border border-[var(--border)] focus:border-[var(--accent)] rounded px-2 py-1 text-xs text-[var(--text)] outline-none transition-colors resize-none",
                rows: 3,
                maxlength: 1000,
                placeholder: "Rules new members must accept…",
                value: "{rules_draft}",
                oninput: move |e| rules_draft.set(e.value()),
            }
        }
        div { class: "flex items-center gap-2 mt-2",
            div { class: "text-[10px] text-[var(--text-dim)] flex-1",
                match gate_draft() {
                    JoinGate::Open => "Anyone can join instantly.",
                    JoinGate::Rules => "Members see the rules and must accept before joining.",
                    JoinGate::Pow => "Members' devices solve a proof-of-work — slows automated raids.",
                }
            }
            button {
                class: "rounded px-3 py-1 text-[10px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors",
                onclick: {
                    let gateway = gateway.clone();
                    move |_| {
                        let r = rules_draft().trim().to_string();
                        gateway.send(ClientMessage::SetJoinGate {
                            guild_id,
                            gate: gate_draft(),
                            rules: if r.is_empty() { None } else { Some(r) },
                        });
                    }
                },
                "Save"
            }
        }

        // Panic mode — emergency lockdown, toggles immediately.
        label { class: "flex items-center gap-2 mt-3 text-xs cursor-pointer select-none",
            input {
                r#type: "checkbox",
                checked: panic_mode,
                onchange: {
                    let gateway = gateway.clone();
                    move |_| {
                        gateway.send(ClientMessage::SetPanicMode { guild_id, on: !panic_mode });
                    }
                },
            }
            span {
                class: if panic_mode { "text-[var(--warn)] font-medium" } else { "text-[var(--text)]" },
                "🚨 Lockdown — reject all new joins"
            }
        }

        // Audit log.
        div { class: "mt-3",
            div { class: "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)] mb-1",
                "Audit log"
            }
            if entries().is_empty() {
                div { class: "text-[10px] text-[var(--text-dim)]", "No moderation actions yet." }
            }
            div { class: "max-h-32 overflow-y-auto space-y-0.5",
                for e in entries().iter().rev().take(50).cloned() {
                    {
                        // Who acted, by name where we know it. `actor_pubkey` has
                        // been written by every `audit()` call and persisted since
                        // the log existed, and nothing rendered it — so the log
                        // answered "what happened to whom" and not "who did it",
                        // which is the question an audit log is for.
                        //
                        // Resolved through the member list rather than shown raw,
                        // like every other pubkey in the UI. It falls back to the
                        // truncated key, which is the honest answer for an actor
                        // who has since left the guild.
                        let actor = state
                            .read()
                            .user_of(&e.actor_pubkey)
                            .map(|u| u.username.clone())
                            .unwrap_or_else(|| truncate_pubkey(&e.actor_pubkey));
                        rsx! {
                            div {
                                key: "{e.at_ms}-{e.action}-{e.target}",
                                class: "text-[10px] text-[var(--text-dim)] font-mono flex gap-2",
                                span {
                                    class: "truncate text-[var(--text-muted)] shrink-0",
                                    title: "{e.actor_pubkey}",
                                    "{actor}"
                                }
                                span { class: "text-[var(--accent)]", "{e.action}" }
                                span { class: "truncate", "{truncate_pubkey(&e.target)}" }
                                if !e.detail.is_empty() {
                                    span { class: "truncate text-[var(--text-muted)]", "{e.detail}" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A small "pick an image file" button that uploads via Blossom (with the
/// data-URL fallback) and reports `(url, note)` — the same pipeline profile
/// avatars use.
#[component]
fn ImagePickButton(
    label: &'static str,
    /// Shape the picked image is cropped to before upload.
    shape: crate::features::image_editor::CropShape,
    onpicked: EventHandler<(Option<String>, Option<String>)>,
    identity: crate::identity::Identity,
    settings: Signal<crate::settings::ClientSettings>,
) -> Element {
    // The picked image waits here while the user frames it. Uploading only
    // happens once they accept the crop.
    let mut editing = use_signal(|| None::<String>);
    rsx! {
        if let Some(src) = editing() {
            crate::features::image_editor::ImageEditor {
                src,
                shape,
                on_cancel: move |_| editing.set(None),
                on_apply: move |cropped: String| {
                    editing.set(None);
                    let identity = identity.clone();
                    let server = settings.read().blossom_server.clone();
                    let onpicked = onpicked;
                    spawn(async move {
                        // The crop is already a data URL; hand the bytes to the
                        // same upload path a raw pick used to take.
                        let bytes = crate::features::profiles::data_url_bytes(&cropped);
                        let mime = crate::features::profiles::data_url_mime(&cropped);
                        let result = crate::features::profiles::image_to_ref(
                            server, identity, bytes, mime,
                        )
                        .await;
                        onpicked.call(result);
                    });
                },
            }
        }
        label { class: "rounded px-3 py-1 text-[10px] uppercase tracking-wider text-[var(--text-muted)] border border-[var(--border)] hover:border-[var(--accent)] hover:text-[var(--accent)] transition-colors cursor-pointer",
            "{label}"
            input {
                r#type: "file",
                accept: "image/*",
                style: "display:none",
                onchange: move |evt| {
                    let files = evt.files();
                    let onpicked = onpicked;
                    spawn(async move {
                        let Some(file) = files.into_iter().next() else { return };
                        match file.read_bytes().await {
                            Ok(bytes) => {
                                // Normalise the mime BEFORE validating: the
                                // webview reports `application/octet-stream`
                                // for extensions it doesn't know, and passing
                                // that through produced a data URL the server
                                // refuses — which looked like "my image was
                                // rejected" for a perfectly good picture.
                                let mime = crate::features::profiles::image_mime(
                                    file.content_type(),
                                );
                                // Say what's wrong here rather than letting the
                                // upload fail later with a note about Blossom.
                                if let Err(msg) =
                                    crate::features::profiles::check_image(&bytes, &mime)
                                {
                                    onpicked.call((None, Some(msg)));
                                    return;
                                }
                                // Frame it first; the upload happens on accept.
                                editing.set(Some(crate::features::profiles::to_data_url(
                                    &bytes, &mime,
                                )));
                            }
                            Err(_) => {
                                onpicked.call((None, Some("Couldn't read that file.".into())));
                            }
                        }
                    });
                },
            }
        }
    }
}

/// Largest emoji image accepted before upload. Matches the server's cap, so a
/// too-big file is refused here with a message instead of bouncing back as a
/// generic error.
const MAX_EMOJI_BYTES: usize = 256_000;

/// Custom-emoji management for a guild. Needs `ManageEmojis` — which the guild
/// owner implicitly holds, like every other permission. The server re-checks
/// every operation here; this only decides what's worth showing.
#[component]
fn EmojiSettings(guild_id: Id) -> Element {
    let state = use_app_state();
    let gateway = use_gateway();

    let emojis = use_memo(move || state.read().emojis_of(guild_id).to_vec());
    let mut shortcode = use_signal(String::new);
    let mut pending = use_signal(|| None::<String>);
    let mut error = use_signal(|| None::<String>);
    let mut renaming = use_signal(|| None::<Id>);
    let mut rename_draft = use_signal(String::new);

    let count = emojis().len();
    let full = count >= crate::protocol::MAX_EMOJIS_PER_GUILD;

    let submit = {
        let gateway = gateway.clone();
        move |_| {
            let code = shortcode().trim().trim_matches(':').to_ascii_lowercase();
            let Some(image) = pending() else {
                error.set(Some("Pick an image first.".into()));
                return;
            };
            if !crate::protocol::valid_shortcode(&code) {
                error.set(Some(
                    "Name must be 2-32 characters of a-z, 0-9 or _.".into(),
                ));
                return;
            }
            error.set(None);
            gateway.send(ClientMessage::CreateGuildEmoji {
                guild_id,
                shortcode: code,
                image,
            });
            shortcode.set(String::new());
            pending.set(None);
        }
    };

    rsx! {
        div { class: "border-t border-[var(--border)] pt-3",
            div { class: "flex items-center gap-2 mb-1.5",
                div { class: "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)] flex-1",
                    "Custom emoji"
                }
                div { class: "text-[10px] text-[var(--text-dim)]",
                    "{count}/{crate::protocol::MAX_EMOJIS_PER_GUILD}"
                }
            }
            div { class: "text-[10px] text-[var(--text-dim)] mb-2",
                "Members of this guild type :name: to use them. PNG, JPEG, GIF or WebP, up to 256 KB."
            }

            // Add form.
            if !full {
                div { class: "flex items-center gap-2 mb-2",
                    label {
                        class: "shrink-0 w-8 h-8 rounded border border-dashed border-[var(--border)] flex items-center justify-center cursor-pointer text-[var(--text-muted)] hover:border-[var(--accent)] hover:text-[var(--accent)] transition-colors",
                        title: "Choose an image",
                        if let Some(img) = pending() {
                            img { src: "{img}", style: "max-height:100%;max-width:100%;" }
                        } else {
                            span { class: "text-sm leading-none", "+" }
                        }
                        input {
                            r#type: "file",
                            accept: "image/*",
                            class: "hidden",
                            onchange: move |evt: FormEvent| {
                                let files = evt.files();
                                spawn(async move {
                                    let Some(file) = files.into_iter().next() else { return };
                                    match file.read_bytes().await {
                                        Ok(bytes) => {
                                            // Fail here rather than bouncing off
                                            // the server, so the message can say
                                            // something useful.
                                            if bytes.len() > MAX_EMOJI_BYTES {
                                                error.set(Some("That image is over 256 KB.".into()));
                                                return;
                                            }
                                            let mime = file
                                                .content_type()
                                                .filter(|m| m.starts_with("image/"))
                                                .unwrap_or_else(|| "image/png".to_string());
                                            let b64 = base64::engine::general_purpose::STANDARD
                                                .encode(&bytes);
                                            error.set(None);
                                            pending.set(Some(format!("data:{mime};base64,{b64}")));
                                        }
                                        Err(_) => error.set(Some("Couldn't read that file.".into())),
                                    }
                                });
                            },
                        }
                    }
                    input {
                        class: "flex-1 bg-transparent border border-[var(--border)] rounded px-2 py-1 text-xs text-[var(--text)] focus:outline-none focus:border-[var(--accent)]",
                        placeholder: "name",
                        maxlength: crate::protocol::MAX_SHORTCODE_LEN as i64,
                        value: "{shortcode}",
                        oninput: move |e| shortcode.set(e.value()),
                    }
                    button {
                        class: "rounded px-3 py-1 text-[10px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors disabled:opacity-40",
                        disabled: pending().is_none() || shortcode().trim().is_empty(),
                        onclick: submit,
                        "Add"
                    }
                }
            } else {
                div { class: "text-[10px] text-[var(--warn)] mb-2",
                    "This guild has reached the emoji limit. Remove one to add another."
                }
            }
            if let Some(e) = error() {
                div { class: "text-[10px] text-[var(--danger)] mb-2", "{e}" }
            }

            if emojis().is_empty() {
                div { class: "text-xs text-[var(--text-dim)]", "No custom emoji yet." }
            }
            for e in emojis().iter().cloned() {
                {
                    let url = state.read().emoji_images.get(&e.image).cloned().unwrap_or_default();
                    let gw_del = gateway.clone();
                    let gw_ren = gateway.clone();
                    let id = e.id;
                    let code = e.shortcode.clone();
                    let is_renaming = renaming() == Some(id);
                    rsx! {
                        div { key: "{e.id}", class: "flex items-center gap-2 py-1",
                            if url.is_empty() {
                                div { class: "w-6 h-6 rounded bg-[var(--bg2)] shrink-0" }
                            } else {
                                img { src: "{url}", style: "height:1.5rem;width:auto;", alt: ":{code}:" }
                            }
                            if is_renaming {
                                input {
                                    class: "flex-1 bg-transparent border border-[var(--border)] rounded px-2 py-0.5 text-xs text-[var(--text)] focus:outline-none focus:border-[var(--accent)]",
                                    maxlength: crate::protocol::MAX_SHORTCODE_LEN as i64,
                                    value: "{rename_draft}",
                                    oninput: move |ev| rename_draft.set(ev.value()),
                                }
                                button {
                                    class: "text-[10px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] rounded px-2 py-0.5 hover:border-[var(--accent)] transition-colors",
                                    onclick: move |_| {
                                        let next = rename_draft().trim().trim_matches(':').to_ascii_lowercase();
                                        if crate::protocol::valid_shortcode(&next) {
                                            gw_ren.send(ClientMessage::RenameGuildEmoji {
                                                guild_id,
                                                emoji_id: id,
                                                shortcode: next,
                                            });
                                            renaming.set(None);
                                        }
                                    },
                                    "Save"
                                }
                            } else {
                                span { class: "text-xs text-[var(--text)] flex-1 font-mono", ":{code}:" }
                                button {
                                    class: "text-[10px] uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--text)] transition-colors",
                                    onclick: move |_| {
                                        rename_draft.set(code.clone());
                                        renaming.set(Some(id));
                                    },
                                    "Rename"
                                }
                            }
                            button {
                                class: "text-[10px] uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--danger)] transition-colors",
                                onclick: move |_| gw_del.send(ClientMessage::DeleteGuildEmoji {
                                    guild_id,
                                    emoji_id: id,
                                }),
                                "Remove"
                            }
                        }
                    }
                }
            }
        }
    }
}
