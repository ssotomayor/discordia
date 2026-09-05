use base64::Engine as _;
use dioxus::prelude::*;

use crate::identity::truncate_pubkey;
use crate::protocol::{ClientMessage, GuildVisibility, Id, JoinGate};
use crate::state::{use_app_state, use_gateway};

#[component]
pub fn GuildSettingsDialog(guild_id: Id, on_close: EventHandler<()>) -> Element {
    let state = use_app_state();
    let gateway = use_gateway();

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

    {
        let gw = gateway.clone();
        let fetch_bans = can_ban;
        use_hook(move || {
            gw.send(ClientMessage::CreateInvite {
                expires_in_secs: None,
                max_uses: None,
                guild_id,
                rotate: false,
            });
            gw.send(ClientMessage::FetchAuditLog { guild_id });
            if fetch_bans {
                gw.send(ClientMessage::FetchBans { guild_id });
            }
        });
    }

    let mut name = use_signal(|| {
        state
            .read()
            .guilds
            .iter()
            .find(|g| g.id == guild_id)
            .map(|g| g.name.clone())
            .unwrap_or_default()
    });
    let mut description = use_signal(|| {
        state
            .read()
            .guilds
            .iter()
            .find(|g| g.id == guild_id)
            .and_then(|g| g.description.clone())
            .unwrap_or_default()
    });
    let mut icon = use_signal(|| {
        state
            .read()
            .guilds
            .iter()
            .find(|g| g.id == guild_id)
            .and_then(|g| g.icon_image.clone())
    });
    let mut banner = use_signal(|| {
        state
            .read()
            .guilds
            .iter()
            .find(|g| g.id == guild_id)
            .and_then(|g| g.banner.clone())
    });
    let mut private = use_signal(|| {
        state
            .read()
            .guilds
            .iter()
            .find(|g| g.id == guild_id)
            .map(|g| matches!(g.visibility, GuildVisibility::Private))
            .unwrap_or(false)
    });
    let mut retention = use_signal(|| {
        state
            .read()
            .guilds
            .iter()
            .find(|g| g.id == guild_id)
            .and_then(|g| g.retention_days)
            .map(|d| d.to_string())
            .unwrap_or_default()
    });
    let mut gate = use_signal(|| {
        state
            .read()
            .guilds
            .iter()
            .find(|g| g.id == guild_id)
            .map(|g| g.join_gate)
            .unwrap_or_default()
    });
    let mut rules = use_signal(|| {
        state
            .read()
            .guilds
            .iter()
            .find(|g| g.id == guild_id)
            .and_then(|g| g.rules.clone())
            .unwrap_or_default()
    });
    let leveling = use_signal(|| {
        state
            .read()
            .guilds
            .iter()
            .find(|g| g.id == guild_id)
            .map(|g| g.leveling.clone())
            .unwrap_or_default()
    });
    let mut panic_mode = use_signal(|| {
        state
            .read()
            .guilds
            .iter()
            .find(|g| g.id == guild_id)
            .map(|g| g.panic_mode)
            .unwrap_or(false)
    });
    let mut upload_note = use_signal(|| None::<String>);
    let mut copied = use_signal(|| false);
    let mut saved = use_signal(|| false);
    let mut awaiting_echo = use_signal(|| false);

    // What we picked is a data URL; what comes back is a `media:` sentinel for
    // the same bytes, so adopting the echo is what stops the form reading dirty.
    use_effect(move || {
        let Some(g) = guild() else { return };
        if *awaiting_echo.peek() {
            awaiting_echo.set(false);
            icon.set(g.icon_image);
            banner.set(g.banner);
        }
    });

    let Some(g) = guild() else {
        return rsx! { Fragment {} };
    };

    let trimmed_name = name().trim().to_string();
    let desc_now = {
        let d = description().trim().to_string();
        (!d.is_empty()).then_some(d)
    };
    let rules_now = {
        let r = rules().trim().to_string();
        (!r.is_empty()).then_some(r)
    };
    let retention_now = retention()
        .trim()
        .parse::<u32>()
        .ok()
        .map(|d| d.clamp(1, 3650));
    let visibility_now = if private() {
        GuildVisibility::Private
    } else {
        GuildVisibility::Public
    };

    let name_changed = !trimmed_name.is_empty() && trimmed_name != g.name;
    let profile_changed =
        name_changed || desc_now != g.description || icon() != g.icon_image || banner() != g.banner;
    let visibility_changed = visibility_now != g.visibility;
    let retention_changed = retention_now != g.retention_days;
    let gate_changed = gate() != g.join_gate || rules_now != g.rules;
    let panic_changed = panic_mode() != g.panic_mode;
    // Compared through the same filter the server will apply, so re-ordering a
    // tier list into the order it already had does not read as an edit.
    let leveling_now = crate::protocol::sanitize_leveling(leveling());
    let leveling_changed = leveling_now != g.leveling;
    let dirty = profile_changed
        || visibility_changed
        || retention_changed
        || gate_changed
        || panic_changed
        || leveling_changed;
    let name_empty = trimmed_name.is_empty();

    let save_all = {
        let gateway = gateway.clone();
        move |_| {
            if name_empty {
                return;
            }
            if profile_changed {
                awaiting_echo.set(true);
                gateway.send(ClientMessage::SetGuildProfile {
                    guild_id,
                    name: Some(trimmed_name.clone()),
                    description: desc_now.clone(),
                    icon_image: icon(),
                    banner: banner(),
                });
            }
            if visibility_changed {
                gateway.send(ClientMessage::SetGuildVisibility {
                    guild_id,
                    visibility: visibility_now,
                });
            }
            if retention_changed {
                gateway.send(ClientMessage::SetGuildRetention {
                    guild_id,
                    days: retention_now,
                });
            }
            if gate_changed {
                gateway.send(ClientMessage::SetJoinGate {
                    guild_id,
                    gate: gate(),
                    rules: rules_now.clone(),
                });
            }
            if panic_changed {
                gateway.send(ClientMessage::SetPanicMode {
                    guild_id,
                    on: panic_mode(),
                });
            }
            if leveling_changed {
                gateway.send(ClientMessage::SetGuildLeveling {
                    guild_id,
                    leveling: leveling_now.clone(),
                });
            }
            saved.set(true);
        }
    };

    let icon_preview = icon().and_then(|i| state.read().media_src(&i).map(str::to_string));
    let banner_preview = banner().and_then(|b| state.read().media_src(&b).map(str::to_string));
    let gate_value = match gate() {
        JoinGate::Open => "open",
        JoinGate::Rules => "rules",
        JoinGate::Pow => "pow",
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

                    div {
                        div { class: "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)] mb-1.5",
                            "Identity"
                        }
                        input {
                            class: "w-full bg-transparent border border-[var(--border)] focus:border-[var(--accent)] rounded px-2 py-1 text-xs text-[var(--text)] outline-none transition-colors",
                            placeholder: "Guild name",
                            maxlength: 64,
                            value: "{name}",
                            oninput: move |e| { saved.set(false); name.set(e.value()); },
                        }
                        if name_empty {
                            div { class: "text-[10px] text-[var(--danger)] mt-1", "A guild needs a name." }
                        }
                        textarea {
                            class: "w-full mt-2 bg-transparent border border-[var(--border)] focus:border-[var(--accent)] rounded px-2 py-1 text-xs text-[var(--text)] outline-none transition-colors resize-none",
                            rows: 2,
                            maxlength: 280,
                            placeholder: "Describe this guild…",
                            value: "{description}",
                            oninput: move |e| { saved.set(false); description.set(e.value()); },
                        }
                        div { class: "flex items-center gap-3 mt-2",
                            div { class: "shrink-0 text-center",
                                if let Some(src) = icon_preview {
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
                                if let Some(src) = banner_preview {
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
                        div { class: "flex flex-wrap gap-2 mt-2",
                            ImagePickButton {
                                label: "Icon…",
                                shape: crate::features::image_editor::CropShape::Square,
                                onpicked: move |(url, note): (Option<String>, Option<String>)| {
                                    upload_note.set(note);
                                    if url.is_some() {
                                        saved.set(false);
                                        icon.set(url);
                                    }
                                },
                            }
                            if icon().is_some() {
                                button {
                                    class: "rounded px-2 py-1 text-[10px] uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--danger)] transition-colors",
                                    onclick: move |_| { saved.set(false); icon.set(None); },
                                    "Clear icon"
                                }
                            }
                            ImagePickButton {
                                label: "Banner…",
                                shape: crate::features::image_editor::CropShape::Banner,
                                onpicked: move |(url, note): (Option<String>, Option<String>)| {
                                    upload_note.set(note);
                                    if url.is_some() {
                                        saved.set(false);
                                        banner.set(url);
                                    }
                                },
                            }
                            if banner().is_some() {
                                button {
                                    class: "rounded px-2 py-1 text-[10px] uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--danger)] transition-colors",
                                    onclick: move |_| { saved.set(false); banner.set(None); },
                                    "Clear banner"
                                }
                            }
                        }
                        div { class: "text-[10px] text-[var(--text-dim)] mt-1",
                            "Icon: square works best. Banner: wide (about 4:1). "
                            {crate::features::profiles::IMAGE_HELP}
                        }
                        if let Some(note) = upload_note() {
                            div { class: "text-[10px] text-[var(--warn)] mt-1", "{note}" }
                        }
                    }

                    div { class: "border-t border-[var(--border)] pt-3",
                        div { class: "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)] mb-1.5",
                            "Visibility"
                        }
                        label { class: "flex items-center gap-2 text-xs text-[var(--text)] cursor-pointer select-none",
                            input {
                                r#type: "checkbox",
                                checked: private(),
                                onchange: move |_| { saved.set(false); private.toggle(); },
                            }
                            span { "Private (invite-only)" }
                        }
                        div { class: "text-[10px] text-[var(--text-dim)] mt-1",
                            "Private guilds are hidden from the browse directory; people join with the invite code below."
                        }
                    }

                    div { class: "border-t border-[var(--edge)] pt-3",
                        div { class: "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)] mb-1.5",
                            "Message retention"
                        }
                        div { class: "flex items-center gap-2",
                            input {
                                class: "w-24 bg-transparent border border-[var(--edge)] focus:border-[var(--accent)] rounded px-2 py-1 text-xs text-[var(--text)] outline-none transition-colors",
                                r#type: "number",
                                min: "1",
                                max: "3650",
                                placeholder: "forever",
                                value: "{retention}",
                                oninput: move |e| { saved.set(false); retention.set(e.value()); },
                            }
                            span { class: "text-xs text-[var(--text-muted)] flex-1", "days" }
                        }
                        div { class: "text-[10px] text-[var(--text-dim)] mt-1",
                            "Messages older than this are deleted (hourly sweep). Blank keeps everything forever."
                        }
                    }

                    div { class: "border-t border-[var(--border)] pt-3",
                        div { class: "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)] mb-1.5",
                            "Community safety"
                        }
                        div { class: "flex items-center gap-2",
                            span { class: "text-xs text-[var(--text-muted)] flex-1", "New members must…" }
                            select {
                                class: "bg-[var(--panel-solid)] border border-[var(--border)] focus:border-[var(--accent)] rounded px-2 py-1 text-xs text-[var(--text)] outline-none",
                                value: "{gate_value}",
                                onchange: move |e| {
                                    saved.set(false);
                                    gate.set(match e.value().as_str() {
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
                        if matches!(gate(), JoinGate::Rules) {
                            textarea {
                                class: "w-full mt-2 bg-transparent border border-[var(--border)] focus:border-[var(--accent)] rounded px-2 py-1 text-xs text-[var(--text)] outline-none transition-colors resize-none",
                                rows: 3,
                                maxlength: 1000,
                                placeholder: "Rules new members must accept…",
                                value: "{rules}",
                                oninput: move |e| { saved.set(false); rules.set(e.value()); },
                            }
                        }
                        div { class: "text-[10px] text-[var(--text-dim)] mt-2",
                            match gate() {
                                JoinGate::Open => "Anyone can join instantly.",
                                JoinGate::Rules => "Members see the rules and must accept before joining.",
                                JoinGate::Pow => "Members' devices solve a proof-of-work — slows automated raids.",
                            }
                        }
                        label { class: "flex items-center gap-2 mt-3 text-xs cursor-pointer select-none",
                            input {
                                r#type: "checkbox",
                                checked: panic_mode(),
                                onchange: move |_| { saved.set(false); panic_mode.toggle(); },
                            }
                            span {
                                class: if panic_mode() { "text-[var(--warn)] font-medium" } else { "text-[var(--text)]" },
                                "🚨 Lockdown — reject all new joins"
                            }
                        }
                        AuditLog { guild_id }
                    }

                    crate::features::guild_leveling::LevelingEditor { guild_id, draft: leveling }

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
                                        gateway.send(ClientMessage::CreateInvite {
                                    guild_id,
                                    rotate: true,
                                    expires_in_secs: None,
                                    max_uses: None,
                                });
                                    }
                                },
                                "Rotate"
                            }
                        }
                        div { class: "text-[10px] text-[var(--text-dim)] mt-1",
                            "Rotating takes effect at once — it is not held for the Save button."
                        }
                    }

                    if can_emojis {
                        EmojiSettings { guild_id }
                    }

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
                div { class: "px-3 py-2.5 border-t border-[var(--border)] flex items-center gap-2 shrink-0",
                    span { class: "text-[10px] text-[var(--text-dim)] flex-1",
                        if dirty {
                            "Unsaved changes."
                        } else if saved() {
                            "Saved."
                        }
                    }
                    button {
                        class: "rounded px-3 py-1 text-[10px] uppercase tracking-wider text-[var(--text-muted)] border border-[var(--border)] hover:border-[var(--border-strong)] transition-colors",
                        onclick: move |_| on_close.call(()),
                        if dirty { "Discard" } else { "Close" }
                    }
                    button {
                        class: "dxf-cta rounded px-4 py-1.5 text-[11px] uppercase tracking-wider",
                        disabled: !dirty || name_empty,
                        onclick: save_all,
                        "Save changes"
                    }
                }
            }
        }
    }
}

#[component]
fn AuditLog(guild_id: Id) -> Element {
    let state = use_app_state();
    let entries = use_memo(move || {
        state
            .read()
            .audit_logs
            .get(&guild_id)
            .cloned()
            .unwrap_or_default()
    });

    rsx! {
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
                        let actor = state.read().display_name(&e.actor_pubkey);
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

#[component]
fn ImagePickButton(
    label: &'static str,
    shape: crate::features::image_editor::CropShape,
    onpicked: EventHandler<(Option<String>, Option<String>)>,
) -> Element {
    let mut editing = use_signal(|| None::<String>);
    rsx! {
        if let Some(src) = editing() {
            crate::features::image_editor::ImageEditor {
                src,
                shape,
                on_cancel: move |_| editing.set(None),
                on_apply: move |cropped: String| {
                    editing.set(None);
                    onpicked.call(crate::features::profiles::embed_image(cropped));
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
                                let mime = crate::features::profiles::image_mime(
                                    file.content_type(),
                                );
                                if let Err(msg) =
                                    crate::features::profiles::check_image(&bytes, &mime)
                                {
                                    onpicked.call((None, Some(msg)));
                                    return;
                                }
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

const MAX_EMOJI_BYTES: usize = 256_000;

fn emoji_provenance(added_by: &str, adder: &str, created_ms: i64) -> Option<String> {
    let when = (created_ms > 0)
        .then(|| chrono::DateTime::from_timestamp_millis(created_ms))
        .flatten()
        .map(|t| t.format("%Y-%m-%d").to_string());
    match (added_by.is_empty(), when) {
        (false, Some(when)) => Some(format!("added by {adder} · {when}")),
        (false, None) => Some(format!("added by {adder}")),
        (true, Some(when)) => Some(format!("added {when}")),
        (true, None) => None,
    }
}

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
                    let adder = state.read().display_name(&e.added_by);
                    let provenance = emoji_provenance(&e.added_by, &adder, e.created_ms);
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
                                div { class: "flex-1 min-w-0",
                                    div { class: "text-xs text-[var(--text)] font-mono truncate", ":{code}:" }
                                    if let Some(line) = provenance {
                                        div {
                                            class: "text-[10px] text-[var(--text-dim)] truncate",
                                            title: "{e.added_by}",
                                            "{line}"
                                        }
                                    }
                                }
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

#[cfg(test)]
mod tests {
    use super::emoji_provenance;

    const WHEN: i64 = 1_786_708_800_000;

    #[test]
    fn a_complete_record_names_both() {
        assert_eq!(
            emoji_provenance("abc", "alice", WHEN).as_deref(),
            Some("added by alice · 2026-08-14")
        );
    }

    #[test]
    fn missing_halves_drop_out_rather_than_lying() {
        assert_eq!(
            emoji_provenance("abc", "alice", 0).as_deref(),
            Some("added by alice")
        );
        assert_eq!(
            emoji_provenance("", "", WHEN).as_deref(),
            Some("added 2026-08-14")
        );
        assert_eq!(emoji_provenance("", "", 0), None);
    }

    #[test]
    fn a_pre_epoch_timestamp_is_treated_as_absent() {
        assert_eq!(
            emoji_provenance("abc", "alice", -1).as_deref(),
            Some("added by alice")
        );
    }
}
