//! "Server settings" dialog for a guild: branding (description + icon/banner
//! images), visibility (public directory vs invite-only), the invite code, and
//! the ban list. Everything here needs `ManageGuild` (bans need `BanMembers`);
//! the server enforces — the dialog just won't be reachable without the menu
//! entry.

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
    let bans = use_memo(move || state.read().bans.get(&guild_id).cloned().unwrap_or_default());
    let can_ban = state.read().can(guild_id, crate::protocol::Permission::BanMembers);

    // Fetch the invite + ban list once on open (fetch-on-open pattern).
    {
        let gw = gateway.clone();
        let fetch_bans = can_ban;
        use_hook(move || {
            gw.send(ClientMessage::CreateInvite { guild_id, rotate: false });
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
    let mut upload_note = use_signal(|| None::<String>);
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
                    h3 { class: "text-sm font-medium text-[var(--accent)] flex-1", "Server settings — {g.name}" }
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
                            placeholder: "Describe this server…",
                            value: "{description}",
                            oninput: move |e| description.set(e.value()),
                        }
                        div { class: "flex gap-2 mt-2",
                            // Icon + banner pickers: upload to Blossom, fall
                            // back to an embedded data URL (same path as
                            // profile avatars), then push the whole profile.
                            ImagePickButton {
                                label: "Icon…",
                                onpicked: {
                                    let gateway = gateway.clone();
                                    let banner = g.banner.clone();
                                    move |(url, note): (Option<String>, Option<String>)| {
                                        upload_note.set(note);
                                        if url.is_some() {
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
                                onpicked: {
                                    let gateway = gateway.clone();
                                    let icon_image = g.icon_image.clone();
                                    move |(url, note): (Option<String>, Option<String>)| {
                                        upload_note.set(note);
                                        if url.is_some() {
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
                        if let Some(note) = upload_note() {
                            div { class: "text-[10px] text-[var(--warn)] mt-1", "{note}" }
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
                            "Private servers are hidden from the browse directory; people join with the invite code below."
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
                            gate: g.join_gate.clone(),
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
                                        let js = format!(
                                            "navigator.clipboard && navigator.clipboard.writeText('{code}');"
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
fn SafetyControls(guild_id: Id, gate: JoinGate, rules: Option<String>, panic_mode: bool) -> Element {
    let state = use_app_state();
    let gateway = use_gateway();

    let mut gate_draft = use_signal(|| gate.clone());
    let mut rules_draft = use_signal(|| rules.clone().unwrap_or_default());
    let entries = use_memo(move || {
        state.read().audit_logs.get(&guild_id).cloned().unwrap_or_default()
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
                    div {
                        key: "{e.at_ms}-{e.action}-{e.target}",
                        class: "text-[10px] text-[var(--text-dim)] font-mono flex gap-2",
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

/// A small "pick an image file" button that uploads via Blossom (with the
/// data-URL fallback) and reports `(url, note)` — the same pipeline profile
/// avatars use.
#[component]
fn ImagePickButton(
    label: &'static str,
    onpicked: EventHandler<(Option<String>, Option<String>)>,
    identity: crate::identity::Identity,
    settings: Signal<crate::settings::ClientSettings>,
) -> Element {
    rsx! {
        label { class: "rounded px-3 py-1 text-[10px] uppercase tracking-wider text-[var(--text-muted)] border border-[var(--border)] hover:border-[var(--accent)] hover:text-[var(--accent)] transition-colors cursor-pointer",
            "{label}"
            input {
                r#type: "file",
                accept: "image/*",
                style: "display:none",
                onchange: move |evt| {
                    let files = evt.files();
                    let identity = identity.clone();
                    let onpicked = onpicked;
                    let server = settings.read().blossom_server.clone();
                    spawn(async move {
                        let Some(file) = files.into_iter().next() else { return };
                        match file.read_bytes().await {
                            Ok(bytes) => {
                                let mime = file
                                    .content_type()
                                    .unwrap_or_else(|| "image/png".to_string());
                                let result = crate::features::profiles::image_to_ref(
                                    server,
                                    identity,
                                    bytes.to_vec(),
                                    mime,
                                )
                                .await;
                                onpicked.call(result);
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
