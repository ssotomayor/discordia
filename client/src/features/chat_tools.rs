use base64::Engine as _;
use dioxus::prelude::*;

use crate::protocol::{Attachment, ClientMessage, FilePolicy, Id, MessageSearch, Permission};
use crate::state::{use_app_state, use_gateway};

pub(super) fn read_file(
    path: &std::path::Path,
    max_bytes: usize,
) -> Result<(String, String), String> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take((max_bytes.min(crate::protocol::MAX_GUILD_UPLOAD_BYTES) + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.is_empty() || bytes.len() > max_bytes.min(crate::protocol::MAX_GUILD_UPLOAD_BYTES) {
        return Err(format!(
            "Choose a nonempty file up to {} KB.",
            max_bytes / 1000
        ));
    }
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or("Invalid file name")?
        .to_owned();
    Ok((
        name,
        format!(
            "data:application/octet-stream;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        ),
    ))
}

fn date_ms(value: &str, next_day: bool) -> Result<Option<i64>, String> {
    if value.is_empty() {
        return Ok(None);
    }
    let date = chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d").map_err(|_| "Invalid date")?;
    let date = if next_day {
        date.succ_opt().ok_or("Invalid date")?
    } else {
        date
    };
    Ok(Some(
        date.and_hms_opt(0, 0, 0)
            .ok_or("Invalid date")?
            .and_utc()
            .timestamp_millis(),
    ))
}

#[component]
pub(super) fn ChatTools(channel_id: Id) -> Element {
    let state = use_app_state();
    let gateway = use_gateway();
    let mut panel = use_signal(|| "");
    let mut text = use_signal(String::new);
    let mut author = use_signal(String::new);
    let mut after = use_signal(String::new);
    let mut before = use_signal(String::new);
    let mut files = use_signal(|| false);
    let mut pins = use_signal(|| false);
    let mut offset = use_signal(|| 0_u32);
    let mut request = use_signal(|| None::<Id>);
    let mut error = use_signal(|| None::<String>);
    let guild_id = state
        .read()
        .channels
        .iter()
        .find(|c| c.id == channel_id)
        .map(|c| c.guild_id);
    let Some(guild_id) = guild_id else {
        return rsx! {};
    };
    let manage_guild = state.read().can(guild_id, Permission::ManageGuild);
    let file_policy = state
        .read()
        .guild_file_policies
        .get(&guild_id)
        .cloned()
        .unwrap_or_default();
    let fetch = gateway.clone();
    use_effect(move || {
        fetch.send(ClientMessage::FetchGuildFilePolicy { guild_id });
    });
    let search_gateway = gateway.clone();
    let search = move |page: u32, only_pins: bool| {
        let dates = date_ms(&after(), false)
            .and_then(|start| date_ms(&before(), true).map(|end| (start, end)));
        let (after_ms, before_ms) = match dates {
            Ok(dates) => dates,
            Err(e) => {
                error.set(Some(e));
                return;
            }
        };
        if after_ms.zip(before_ms).is_some_and(|(a, b)| a >= b) {
            error.set(Some("The end date must follow the start date.".into()));
            return;
        }
        let id = uuid::Uuid::new_v4();
        request.set(Some(id));
        offset.set(page);
        error.set(None);
        search_gateway.send(ClientMessage::SearchMessages {
            channel_id,
            request_id: id,
            offset: page,
            query: MessageSearch {
                text: if only_pins { String::new() } else { text() },
                author: (!only_pins && !author().is_empty()).then_some(author()),
                after_ms: if only_pins { None } else { after_ms },
                before_ms: if only_pins { None } else { before_ms },
                has_attachment: !only_pins && files(),
                pinned_only: only_pins || pins(),
            },
        });
        spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            let finished = state
                .read()
                .message_search
                .as_ref()
                .is_some_and(|(cid, req, _)| *cid == channel_id && *req == id);
            if request() == Some(id) && !finished {
                request.set(None);
                error.set(Some(
                    "Search did not finish. Reconnect and try again.".into(),
                ));
            }
        });
    };
    let results = state
        .read()
        .message_search
        .as_ref()
        .filter(|(cid, id, _)| *cid == channel_id && Some(*id) == request())
        .map(|(_, _, m)| m.clone());
    let members: Vec<_> = state
        .read()
        .members_of(guild_id)
        .into_iter()
        .cloned()
        .collect();
    let title = match panel() {
        "search" => "Search messages",
        "pins" => "Pinned messages",
        "files" => "File limits",
        _ => "",
    };
    let mut toggle = move |name: &'static str| {
        panel.set(if panel() == name { "" } else { name });
        error.set(None);
    };
    rsx! {
        div { class: "relative z-30 ml-auto flex items-center gap-1.5 shrink-0",
            ToolButton { icon: super::icons::SEARCH, title: crate::i18n::text("Search messages", &[]), active: panel() == "search",
                on_click: move |_| toggle("search") }
            ToolButton { icon: super::icons::PIN, title: crate::i18n::text("Pinned messages", &[]), active: panel() == "pins",
                on_click: { let mut search = search.clone(); move |_| {
                    toggle("pins");
                    if panel() == "pins" { search(0, true); }
                } } }
            if manage_guild {
                ToolButton { icon: super::icons::SLIDERS, title: crate::i18n::text("File limits", &[]), active: panel() == "files",
                    on_click: move |_| toggle("files") }
            }
        }
        if !panel().is_empty() {
            div { class: "fixed inset-0 z-20", onclick: move |_| panel.set("") }
            // Anchored to the chat pane, not the header: `fixed` inside a grid
            // panel is positioned against the panel anyway (trap 22).
            div {
                class: "dxf-pop-in absolute z-30 flex flex-col bg-[var(--panel-solid)] border border-[var(--border)] rounded-xl shadow-lg text-xs text-[var(--text)]",
                style: "top: 3.25rem; right: 0.75rem; width: min(30rem, calc(100% - 1.5rem)); max-height: 70%;",
                div { class: "h-10 px-3 flex items-center justify-between border-b border-[var(--border)] shrink-0",
                    span { class: "text-[11px] uppercase tracking-wider text-[var(--text-dim)]", "{title}" }
                    button {
                        r#type: "button",
                        class: "w-7 h-7 flex items-center justify-center rounded-lg text-[var(--text-muted)] hover:text-[var(--text)] hover:bg-white/[0.04] transition-colors",
                        title: crate::i18n::text("Close", &[]),
                        onclick: move |_| panel.set(""),
                        span { class: "block w-4 h-4", dangerous_inner_html: super::icons::CLOSE }
                    }
                }
                div { class: "flex-1 min-h-0 overflow-y-auto p-3 flex flex-col gap-3",
                    if let Some(error) = error() { p { class: "text-[var(--danger)]", {crate::i18n::message(&(error).to_string())} } }
                    if panel() == "files" && manage_guild { FileLimits { guild_id, policy: file_policy } }
                    if panel() == "search" {
                        form { class: "flex flex-col gap-2",
                            onsubmit: { let mut search = search.clone(); move |e: FormEvent| { e.prevent_default(); search(0, false); } },
                            input { class: INPUT, placeholder: crate::i18n::text("Search messages", &[]), maxlength: 200, autofocus: true,
                                value: text(), oninput: move |e| text.set(e.value()) }
                            select { class: INPUT, value: author(), onchange: move |e| author.set(e.value()),
                                option { value: "", {crate::i18n::text("Any author", &[])} }
                                for member in members { option { value: member.user.pubkey.clone(), "{member.user.username}" } }
                            }
                            div { class: "flex gap-2",
                                label { class: "flex-1 min-w-0 flex flex-col gap-1 text-[var(--text-dim)]", {crate::i18n::text("From (UTC)", &[])},
                                    input { class: INPUT, r#type: "date", value: after(), oninput: move |e| after.set(e.value()) } }
                                label { class: "flex-1 min-w-0 flex flex-col gap-1 text-[var(--text-dim)]", {crate::i18n::text("Through (UTC)", &[])},
                                    input { class: INPUT, r#type: "date", value: before(), oninput: move |e| before.set(e.value()) } }
                            }
                            div { class: "flex items-center gap-4 text-[var(--text-muted)]",
                                label { class: "flex items-center gap-1.5 cursor-pointer",
                                    input { r#type: "checkbox", style: "accent-color: var(--accent);", checked: files(), onchange: move |e| files.set(e.checked()) }
                                    {crate::i18n::text("Has attachment", &[])} }
                                label { class: "flex items-center gap-1.5 cursor-pointer",
                                    input { r#type: "checkbox", style: "accent-color: var(--accent);", checked: pins(), onchange: move |e| pins.set(e.checked()) }
                                    {crate::i18n::text("Pinned", &[])} }
                                button { r#type: "submit", class: "dxf-cta ml-auto rounded-lg px-3 py-1 text-xs", {crate::i18n::text("Search", &[])} }
                            }
                        }
                    }
                    if panel() == "search" || panel() == "pins" {
                        if let Some(results) = results {
                            if results.is_empty() {
                                p { class: "py-4 text-center text-[var(--text-dim)]",
                                    if panel() == "pins" { {crate::i18n::text("Nothing is pinned here.", &[])} } else { {crate::i18n::text("No matching messages.", &[])} }
                                }
                            }
                            div { class: "flex flex-col border-t border-[var(--border)] pt-2",
                                for message in &results { super::chat::MessageRow { key: "{message.id}", message: message.clone(), grouped: false } }
                            }
                            if offset() > 0 || results.len() == 100 {
                                div { class: "flex justify-center gap-2",
                                    if offset() > 0 { button { class: SECONDARY, onclick: { let mut search = search.clone(); move |_| search(offset().saturating_sub(100), panel() == "pins") }, {crate::i18n::text("Newer", &[])} } }
                                    if results.len() == 100 { button { class: SECONDARY, onclick: { let mut search = search.clone(); move |_| search(offset() + 100, panel() == "pins") }, {crate::i18n::text("Older", &[])} } }
                                }
                            }
                        } else if request().is_some() {
                            p { class: "py-4 text-center text-[var(--text-dim)]", {crate::i18n::text("Searching…", &[])} }
                        }
                    }
                }
            }
        }
    }
}

#[component]
pub(super) fn StickerPicker(
    channel_id: Id,
    guild_id: Id,
    mut open: Signal<bool>,
    on_open: EventHandler<()>,
) -> Element {
    let state = use_app_state();
    let gateway = use_gateway();
    let mut error = use_signal(|| None::<String>);
    let mut sticker_name = use_signal(String::new);
    let mut sticker_image = use_signal(|| None::<String>);
    let manage = state.read().can(guild_id, Permission::ManageEmojis);
    let can_send = state.read().can(guild_id, Permission::SendMessages);
    let fetch = gateway.clone();
    use_effect(move || {
        fetch.send(ClientMessage::FetchGuildStickers { guild_id });
    });
    let stickers = state
        .read()
        .guild_stickers
        .get(&guild_id)
        .cloned()
        .unwrap_or_default();
    rsx! {
        ToolButton { icon: super::icons::STICKER, title: crate::i18n::text("Stickers", &[]), active: open(),
            on_click: move |_| {
                open.toggle();
                if open() { on_open.call(()); }
            }
        }
        if open() {
            div { class: "fixed inset-0 z-20", onclick: move |_| open.set(false) }
            div {
                class: "dxf-pop-in absolute bottom-full right-3 mb-2 z-30 flex flex-col bg-[var(--panel-solid)] border border-[var(--border)] rounded-xl shadow-lg text-xs text-[var(--text)]",
                style: "width: min(30rem, calc(100% - 1.5rem)); max-height: 70vh;",
                div { class: "h-10 px-3 flex items-center justify-between border-b border-[var(--border)] shrink-0",
                    span { class: "text-[11px] uppercase tracking-wider text-[var(--text-dim)]", {crate::i18n::text("Stickers", &[])} }
                    button {
                        r#type: "button",
                        class: "w-7 h-7 flex items-center justify-center rounded-lg text-[var(--text-muted)] hover:text-[var(--text)] hover:bg-white/[0.04] transition-colors",
                        title: crate::i18n::text("Close", &[]),
                        onclick: move |_| open.set(false),
                        span { class: "block w-4 h-4", dangerous_inner_html: super::icons::CLOSE }
                    }
                }
                div { class: "flex-1 min-h-0 overflow-y-auto p-3 flex flex-col gap-3",
                    if let Some(error) = error() { p { class: "text-[var(--danger)]", {crate::i18n::message(&(error).to_string())} } }
                    if stickers.is_empty() {
                        p { class: "py-4 text-center text-[var(--text-dim)]", {crate::i18n::text("No stickers yet.", &[])} }
                    }
                    div { class: "grid gap-2", style: "grid-template-columns: repeat(auto-fill, minmax(5.5rem, 1fr));",
                        for sticker in stickers {
                            div { key: "{sticker.id}", class: "relative",
                                button {
                                    r#type: "button",
                                    style: "aspect-ratio: 1;",
                                    class: "w-full flex items-center justify-center p-1.5 rounded-lg border border-[var(--border)] bg-[var(--panel2)] hover:border-[var(--accent)] transition-colors disabled:opacity-50",
                                    disabled: !can_send,
                                    title: ":{sticker.shortcode}:",
                                    onclick: { let g = gateway.clone(); let id = sticker.id; move |_| { g.send(ClientMessage::SendSticker { channel_id, sticker_id: id }); open.set(false); } },
                                    if let Some(src) = state.read().media_src(&sticker.image).map(str::to_owned) {
                                        img { src, alt: sticker.shortcode.clone(), class: "max-w-full max-h-full", style: "object-fit: contain;" }
                                    } else {
                                        span { class: "truncate text-[var(--text-dim)]", "{sticker.shortcode}" }
                                    }
                                }
                                if manage {
                                    button {
                                        r#type: "button",
                                        style: "top: 0.25rem; right: 0.25rem;",
                                        class: "absolute w-5 h-5 flex items-center justify-center rounded bg-[var(--panel-solid)] border border-[var(--border)] text-[var(--text-dim)] hover:text-[var(--danger)] transition-colors",
                                        title: crate::i18n::text("Delete sticker", &[]),
                                        onclick: { let g = gateway.clone(); let id = sticker.id; move |_| g.send(ClientMessage::DeleteGuildSticker { guild_id, sticker_id: id }) },
                                        span { class: "block w-3 h-3", dangerous_inner_html: super::icons::CLOSE }
                                    }
                                }
                            }
                        }
                    }
                    if manage {
                        div { class: "flex items-center gap-2 border-t border-[var(--border)] pt-3",
                            if let Some(src) = sticker_image() {
                                img { src, class: "w-8 h-8 shrink-0 rounded", style: "object-fit: contain;" }
                            }
                            input { class: INPUT, placeholder: crate::i18n::text("New sticker name", &[]), maxlength: 32, value: sticker_name(),
                            oninput: move |e| sticker_name.set(e.value()),
                            onkeydown: move |e: KeyboardEvent| { if e.key() == Key::Enter { e.prevent_default(); } },
                        }
                            button { r#type: "button", class: SECONDARY, onclick: move |_| { spawn(async move {
                                match crate::file_dialog::pick_file(&[(crate::i18n::label("Image"), &["png", "gif", "webp", "jpg"])]).await {
                                Ok(Some(path)) => {
                                    match tokio::task::spawn_blocking(move || crate::chat_image::read_file(&path)).await {
                                        Ok(Ok(image)) => { sticker_image.set(Some(image)); error.set(None); }
                                        Ok(Err(e)) => error.set(Some(e)), Err(e) => error.set(Some(e.to_string())),
                                    }
                                }
                                Ok(None) => {},
                                Err(message) => error.set(Some(message)),
                                }
                            }); }, {crate::i18n::text("Image…", &[])} }
                            button { r#type: "button", class: "dxf-cta shrink-0 rounded-lg px-3 py-1 text-xs disabled:opacity-50",
                                disabled: sticker_image().is_none() || sticker_name().trim().is_empty(),
                                onclick: { let g = gateway.clone(); move |_| {
                                    if let Some(image) = sticker_image() {
                                        g.send(ClientMessage::CreateGuildSticker { guild_id, name: sticker_name(), image });
                                        sticker_image.set(None);
                                        sticker_name.set(String::new());
                                    }
                                } }, {crate::i18n::text("Add", &[])} }
                        }
                    }
                }
            }
        }
    }
}

const INPUT: &str = "w-full min-w-0 bg-[var(--panel2)] border border-[var(--border)] focus:border-[var(--accent)] rounded-lg px-2.5 py-1.5 text-xs text-[var(--text)] outline-none transition-colors";
const SECONDARY: &str = "shrink-0 rounded-lg px-3 py-1 text-xs text-[var(--text-muted)] border border-[var(--border)] hover:text-[var(--accent)] hover:border-[var(--accent)] transition-colors";

#[component]
fn ToolButton(
    icon: &'static str,
    #[props(into)] title: String,
    active: bool,
    on_click: EventHandler<()>,
) -> Element {
    rsx! {
        button {
            r#type: "button",
            class: if active {
                "w-8 h-8 flex items-center justify-center rounded-lg border border-[var(--accent)] bg-[var(--panel2)] text-[var(--accent)] transition-colors"
            } else {
                "w-8 h-8 flex items-center justify-center rounded-lg border border-[var(--border)] bg-[var(--panel2)] text-[var(--text-muted)] hover:text-[var(--accent)] hover:border-[var(--accent)] transition-colors"
            },
            title,
            aria_pressed: active,
            onclick: move |_| on_click.call(()),
            span { class: "block w-4 h-4", dangerous_inner_html: icon }
        }
    }
}

#[component]
fn FileLimits(guild_id: Id, policy: FilePolicy) -> Element {
    let gateway = use_gateway();
    let initial = policy.clone();
    let mut files_kb = use_signal(move || (initial.max_bytes / 1000).to_string());
    let mut avatars_kb = use_signal(move || (initial.max_avatar_bytes / 1000).to_string());
    let mut stickers_kb = use_signal(move || (initial.max_sticker_bytes / 1000).to_string());
    let mut days = use_signal(move || policy.retention_days.to_string());
    let mut error = use_signal(|| None::<String>);
    rsx! {
        form { class: "flex flex-col gap-2.5",
            onsubmit: move |e: FormEvent| {
                e.prevent_default();
                match (
                    files_kb().parse::<u64>(),
                    avatars_kb().parse::<u64>(),
                    stickers_kb().parse::<u64>(),
                    days().parse::<u32>(),
                ) {
                    (Ok(f_kb), Ok(a_kb), Ok(s_kb), Ok(d))
                        if (1..=10_000).contains(&f_kb)
                            && (1..=10_000).contains(&a_kb)
                            && (1..=10_000).contains(&s_kb)
                            && (1..=365).contains(&d) =>
                    {
                        error.set(None);
                        gateway.send(ClientMessage::SetGuildFilePolicy {
                            guild_id,
                            policy: FilePolicy {
                                max_bytes: f_kb * 1000,
                                retention_days: d,
                                max_avatar_bytes: a_kb * 1000,
                                max_sticker_bytes: s_kb * 1000,
                            },
                        });
                    }
                    _ => error.set(Some("Choose 1–10000 KB for limits and 1–365 days for retention.".into())),
                }
            },
            div { class: "grid grid-cols-2 gap-2",
                label { class: "flex flex-col gap-1 text-[var(--text-dim)]",
                    {crate::i18n::text("Chat files & photos (KB)", &[])},
                    input { class: INPUT, r#type: "number", min: 1, max: 10000, value: files_kb(), oninput: move |e| files_kb.set(e.value()) }
                }
                label { class: "flex flex-col gap-1 text-[var(--text-dim)]",
                    {crate::i18n::text("Avatars & banners (KB)", &[])},
                    input { class: INPUT, r#type: "number", min: 1, max: 10000, value: avatars_kb(), oninput: move |e| avatars_kb.set(e.value()) }
                }
                label { class: "flex flex-col gap-1 text-[var(--text-dim)]",
                    {crate::i18n::text("Emojis & stickers (KB)", &[])},
                    input { class: INPUT, r#type: "number", min: 1, max: 10000, value: stickers_kb(), oninput: move |e| stickers_kb.set(e.value()) }
                }
                label { class: "flex flex-col gap-1 text-[var(--text-dim)]",
                    {crate::i18n::text("Keep for (days)", &[])},
                    input { class: INPUT, r#type: "number", min: 1, max: 365, value: days(), oninput: move |e| days.set(e.value()) }
                }
            }
            div { class: "flex items-center gap-2 mt-0.5",
                if let Some(error) = error() { span { class: "text-[var(--danger)] text-xs", {crate::i18n::message(&(error).to_string())} } }
                button { r#type: "submit", class: "dxf-cta ml-auto rounded-lg px-3 py-1 text-xs", {crate::i18n::text("Save", &[])} }
            }
        }
    }
}

#[component]
pub(super) fn FileAttachment(attachment: Attachment) -> Element {
    let mut state = use_app_state();
    let mut busy = use_signal(|| false);
    let mut download_error = use_signal(|| None::<String>);
    let mut now = use_signal(|| chrono::Utc::now().timestamp_millis());
    use_future(move || async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            now.set(chrono::Utc::now().timestamp_millis());
        }
    });
    let expired = attachment.expires_ms.is_some_and(|ms| ms <= now());
    let file_type = attachment
        .name
        .rsplit_once('.')
        .filter(|(name, extension)| {
            !name.is_empty()
                && !extension.is_empty()
                && extension.len() <= 5
                && extension.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
        .map(|(_, extension)| extension.to_ascii_uppercase())
        .unwrap_or_else(|| "FILE".into());
    let size = if attachment.bytes < 1000 {
        format!("{} B", attachment.bytes)
    } else if attachment.bytes < 1_000_000 {
        format!("{:.1} KB", attachment.bytes as f64 / 1000.0)
    } else {
        format!("{:.1} MB", attachment.bytes as f64 / 1_000_000.0)
    };
    let expiry = attachment
        .expires_ms
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map(|date| {
            format!(
                "Expires {}",
                date.with_timezone(&chrono::Local).format("%b %d, %Y")
            )
        });
    rsx! {
        div {
            class: "dxf-file-card",
            "data-expired": expired,
            role: "group",
            aria_label: crate::i18n::text("File attachment: {0}", &[(attachment . name).to_string()]),
            div { class: "dxf-file-type", aria_hidden: "true",
                span { dangerous_inner_html: super::icons::FILE }
                span { "{file_type}" }
            }
            div { class: "dxf-file-info",
                div { class: "dxf-file-name", title: "{attachment.name}", "{attachment.name}" }
                div { class: "dxf-file-meta",
                    span { "{size}" }
                    if expired { span { class: "dxf-file-expired", {crate::i18n::text("File expired", &[])} } }
                    else if let Some(expiry) = expiry { span { "{expiry}" } }
                }
            }
            button {
                class: "dxf-file-download",
                r#type: "button",
                disabled: busy() || expired,
                aria_label: crate::i18n::text("Download {0}", &[(attachment . name).to_string()]),
                aria_busy: busy(),
                title: if expired { crate::i18n::text("This file has expired", &[]) } else { crate::i18n::text("Save a copy to your computer", &[]) },
                onclick: move |_| {
                if busy() || expired { return; }
                busy.set(true);
                download_error.set(None);
                let attachment = attachment.clone();
                spawn(async move {
                    let result: Result<(), String> = async {
                        #[cfg(not(target_os = "android"))]
                        let Some(path) = crate::file_dialog::save_file(&attachment.name).await? else { return Ok::<(), String>(()); };
                        let mut data = None;
                        for _ in 0..100 {
                            if attachment.expires_ms.is_some_and(|ms| ms <= chrono::Utc::now().timestamp_millis()) { return Err("File expired".into()); }
                            data = state.read().media_src(&attachment.media).map(str::to_owned);
                            if data.is_some() { break; }
                            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                        }
                        let data = data.ok_or("The file is unavailable or expired.")?;
                        let payload = data.strip_prefix("data:application/octet-stream;base64,").ok_or("Invalid file data")?;
                        let bytes = base64::engine::general_purpose::STANDARD.decode(payload).map_err(|_| "Invalid file data")?;
                        if bytes.len() as u64 != attachment.bytes || bytes.len() > crate::protocol::MAX_GUILD_UPLOAD_BYTES { return Err("Invalid file size".into()); }
                        #[cfg(target_os = "android")]
                        crate::android::document(attachment.name.clone(), String::new(), Some(bytes)).await?;
                        #[cfg(not(target_os = "android"))]
                        tokio::task::spawn_blocking(move || {
                            save_download(&path, &bytes)?;
                            open_download_folder(&path).map_err(|e| format!("The file was saved, but its folder could not be opened: {e}"))
                        }).await.map_err(|e| e.to_string())??;
                        Ok(())
                    }.await;
                    if let Err(error) = result {
                        download_error.set(Some(error.clone()));
                        state.write().error_toast = Some(error);
                    }
                    busy.set(false);
                });
            },
                span { aria_hidden: "true", dangerous_inner_html: super::icons::DOWNLOAD }
                if busy() { {crate::i18n::text("Downloading…", &[])} } else { {crate::i18n::text("Download", &[])} }
            }
            if let Some(error) = download_error() {
                p { class: "dxf-file-error", role: "alert", {crate::i18n::message(&(error).to_string())} }
            }
        }
    }
}

#[cfg(not(target_os = "android"))]
fn save_download(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        let mut zone = path.as_os_str().to_owned();
        zone.push(":Zone.Identifier");
        std::fs::write(zone, b"[ZoneTransfer]\r\nZoneId=3\r\n")
            .map_err(|e| format!("Couldn't mark this download as untrusted: {e}"))?;
    }
    std::fs::write(path, bytes).map_err(|e| e.to_string())
}

#[cfg(not(target_os = "android"))]
fn download_folder(path: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let parent = path
        .parent()
        .ok_or("The download has no destination folder.")?;
    let folder = std::path::absolute(parent).map_err(|e| e.to_string())?;
    if !folder.is_dir() {
        return Err("The download folder is unavailable.".into());
    }
    Ok(folder)
}

#[cfg(not(target_os = "android"))]
fn open_download_folder(path: &std::path::Path) -> Result<(), String> {
    open_folder(&download_folder(path)?)
}

pub(crate) fn open_folder(folder: &std::path::Path) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::Win32::System::Com::{
            COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize,
        };
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        use windows::core::{PCWSTR, w};
        let folder: Vec<u16> = folder.as_os_str().encode_wide().chain(Some(0)).collect();
        unsafe {
            let initialized = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
            let result = ShellExecuteW(
                None,
                w!("explore"),
                PCWSTR(folder.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            );
            if initialized {
                CoUninitialize();
            }
            if result.0 as isize <= 32 {
                return Err("Windows Explorer could not open the folder.".into());
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        #[cfg(target_os = "macos")]
        let mut command = {
            let mut c = std::process::Command::new("/usr/bin/open");
            c.arg("--");
            c
        };
        #[cfg(not(target_os = "macos"))]
        let mut command = std::process::Command::new("xdg-open");
        let mut child = command.arg(folder).spawn().map_err(|e| e.to_string())?;
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executable_downloads_only_reveal_their_parent_folder() {
        let folder =
            std::env::temp_dir().join(format!("discordia download {}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&folder).unwrap();
        let file = folder.join("untrusted & payload.exe");
        assert_eq!(download_folder(&file).unwrap(), folder);
        std::fs::remove_dir(folder).unwrap();
    }

    #[test]
    fn unavailable_download_folders_cannot_be_opened() {
        let path = std::env::temp_dir()
            .join(uuid::Uuid::new_v4().to_string())
            .join("payload.exe");
        assert!(download_folder(&path).is_err());
    }
}
