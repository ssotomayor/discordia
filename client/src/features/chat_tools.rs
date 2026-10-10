use base64::Engine as _;
use dioxus::prelude::*;

use crate::protocol::{Attachment, ClientMessage, FilePolicy, Id, MessageSearch, Permission};
use crate::state::{use_app_state, use_gateway};

fn read_file(path: &std::path::Path, maximum: u64) -> Result<(String, String), String> {
    use std::io::Read;
    let maximum = maximum.min(2_000_000);
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.is_empty() || bytes.len() as u64 > maximum {
        return Err(format!(
            "Choose a nonempty file up to {} KB.",
            maximum / 1000
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

fn search_query(
    text: &str,
    author: &str,
    after: &str,
    before: &str,
    files: bool,
    pins: bool,
) -> Result<MessageSearch, String> {
    let after_ms = date_ms(after, false)?;
    let before_ms = date_ms(before, true)?;
    if after_ms.zip(before_ms).is_some_and(|(a, b)| a >= b) {
        return Err("The end date must follow the start date.".into());
    }
    Ok(MessageSearch {
        text: text.trim().to_owned(),
        author: (!author.is_empty()).then(|| author.to_owned()),
        after_ms,
        before_ms,
        has_attachment: files,
        pinned_only: pins,
    })
}

#[component]
pub(super) fn ChatTools(channel_id: Id, mut panel: Signal<&'static str>) -> Element {
    let state = use_app_state();
    let gateway = use_gateway();
    let mut advanced = use_signal(|| false);
    let mut submitted = use_signal(|| None::<MessageSearch>);
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
    let fetch = gateway.clone();
    use_effect(move || {
        fetch.send(ClientMessage::FetchGuildFilePolicy { guild_id });
    });
    let search_gateway = gateway.clone();
    let search = move |page: u32, only_pins: bool, paging: bool| {
        let query = if paging {
            let Some(query) = submitted() else {
                return;
            };
            query
        } else if only_pins {
            MessageSearch {
                pinned_only: true,
                ..Default::default()
            }
        } else {
            match search_query(&text(), &author(), &after(), &before(), files(), pins()) {
                Ok(query) => query,
                Err(message) => {
                    error.set(Some(message));
                    return;
                }
            }
        };
        let id = uuid::Uuid::new_v4();
        submitted.set(Some(query.clone()));
        request.set(Some(id));
        offset.set(page);
        error.set(None);
        search_gateway.send(ClientMessage::SearchMessages {
            channel_id,
            request_id: id,
            offset: page,
            query,
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
    let mut pinned_search = search.clone();
    use_effect(move || {
        request.set(None);
        error.set(None);
        offset.set(0);
        submitted.set(None);
        if panel() == "pins" {
            pinned_search(0, true, false);
        }
    });
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
    let waiting = request().is_some() && results.is_none() && error().is_none();
    rsx! {
        div { class: "dxf-tools",
            onkeydown: move |event| { if event.key() == Key::Escape { panel.set(""); } }, style: "position:absolute;inset:0;pointer-events:none;z-index:20;",
            nav { aria_label: "Chat tools", style: "position:absolute;top:0;right:12px;height:48px;display:flex;align-items:center;gap:8px;pointer-events:auto;",
                button { title: "Pinned messages", aria_label: "Pinned messages", aria_expanded: panel() == "pins", class: "dxf-tools-icon", aria_pressed: panel() == "pins", onclick: move |_| panel.set(if panel() == "pins" { "" } else { "pins" }), "📌" }
                button { title: "Search messages", aria_label: "Search messages", aria_expanded: panel() == "search", class: "dxf-tools-icon", aria_pressed: panel() == "search", onclick: move |_| panel.set(if panel() == "search" { "" } else { "search" }), "🔍" }
            }
            if !panel().is_empty() {
            aside { class: "dxf-tools-panel", aria_label: "Chat tools panel", style: "position:absolute;top:48px;bottom:0;right:0;width:var(--chat-tools-width, min(420px,100%));display:flex;flex-direction:column;pointer-events:auto;background-color:var(--panel-solid);border-left:1px solid var(--border);box-shadow:-8px 0 24px #0003;",
                div { class: "flex items-center justify-between px-3 py-3 border-b border-[var(--border)]",
                    strong { if panel() == "search" { "Search messages" } else { "Pinned messages" } }
                    button { class: "dxf-tools-icon", title: "Close", aria_label: "Close chat tools", onclick: move |_| panel.set(""), "✕" }
                }
                div { style: "padding:12px;overflow-y:auto;min-height:0;flex:1;", class: "text-xs",
            if let Some(error) = error() { p { role: "alert", class: "dxf-tools-error", "{error}" } }
            if panel() == "search" {
                div { class: "dxf-tools-query", style: "display:flex;gap:8px;",
                    input { aria_label: "Search message text", placeholder: "Search this channel…", maxlength: 200, value: text(), style: "min-width:0;flex:1;padding:8px;border:1px solid var(--border);border-radius:6px;background-color:var(--bg2);",
                        oninput: move |e| text.set(e.value()),
                        onmounted: move |event| { spawn(async move { let _ = event.data().set_focus(true).await; }); },
                        onkeydown: { let mut search = search.clone(); move |event| { if event.key() == Key::Enter { event.prevent_default(); search(0, false, false); } } }
                    }
                    button { class: "dxf-tools-primary", disabled: waiting, onclick: { let mut search = search.clone(); move |_| search(0, false, false) }, "Search" }
                }
                button { class: "dxf-tools-filter-toggle", aria_expanded: advanced(), onclick: move |_| advanced.toggle(), if advanced() { "Hide filters ▴" } else { "Filters ▾" } }
                if advanced() { div { class: "dxf-tools-filters", style: "display:flex;flex-direction:column;gap:10px;padding-bottom:12px;",
                    label { "From ", select { value: author(), onchange: move |e| author.set(e.value()),
                        option { value: "", "Any author" }
                        for member in members { option { value: member.user.pubkey.clone(), "{member.user.username}" } }
                    } }
                    label { "After (UTC) ", input { r#type: "date", value: after(), oninput: move |e| after.set(e.value()) } }
                    label { "Through (UTC) ", input { r#type: "date", value: before(), oninput: move |e| before.set(e.value()) } }
                    label { input { r#type: "checkbox", checked: files(), onchange: move |e| files.set(e.checked()) } " Has attachment" }
                    label { input { r#type: "checkbox", checked: pins(), onchange: move |e| pins.set(e.checked()) } " Pinned only" }
                    button { onclick: move |_| { text.set(String::new()); author.set(String::new()); after.set(String::new()); before.set(String::new()); files.set(false); pins.set(false); request.set(None); submitted.set(None); offset.set(0); error.set(None); }, "Clear search" }
                } }
            }
            if panel() == "search" || panel() == "pins" {
                if let Some(results) = results {
                    if results.is_empty() { p { class: "py-4 text-[var(--text-dim)]", if panel() == "pins" { "No pinned messages in this channel." } else { "No messages match. Try another term or fewer filters." } } }
                    else { p { class: "py-2 text-[var(--text-dim)]", "Showing {offset() + 1}–{offset() + results.len() as u32}" } }
                    for message in &results { div { key: "{message.id}", class: "dxf-tools-result", super::chat::MessageRow { message: message.clone(), grouped: false } } }
                    div { class: "dxf-tools-pages",
                        if offset() > 0 { button { onclick: { let mut search = search.clone(); move |_| search(offset().saturating_sub(100), panel() == "pins", true) }, "Newer" } }
                        if results.len() == 100 { button { onclick: { let mut search = search.clone(); move |_| search(offset() + 100, panel() == "pins", true) }, "Older" } }
                    }
                } else if waiting { p { role: "status", class: "py-4", "Searching…" } }
                else if error().is_none() { p { class: "py-4 text-[var(--text-dim)]", "Search all messages in this channel. Add filters to narrow the results." } }
            }

                }
            }
            }
        }
    }
}

#[component]
pub(super) fn StickerPicker(guild_id: Id, channel_id: Id, on_close: EventHandler<()>) -> Element {
    let state = use_app_state();
    let gateway = use_gateway();
    let mut query = use_signal(String::new);
    let fetch = gateway.clone();
    use_hook(move || fetch.send(ClientMessage::FetchGuildStickers { guild_id }));
    let snapshot = state.read();
    let can_send = snapshot.can(guild_id, Permission::SendMessages)
        && snapshot.status == crate::state::ConnectionStatus::Ready;
    let loaded = snapshot.guild_stickers.contains_key(&guild_id);
    let stickers = snapshot
        .guild_stickers
        .get(&guild_id)
        .cloned()
        .unwrap_or_default();
    let search = query().trim().to_lowercase();
    let visible: Vec<_> = stickers
        .iter()
        .filter(|s| s.shortcode.to_lowercase().contains(&search))
        .collect();
    rsx! {
        div { role: "dialog", aria_label: "Choose a sticker", class: "dxf-pop-in dxf-sticker-picker absolute bottom-full right-3 mb-2 bg-[var(--panel-solid)] border border-[var(--border)] rounded-md shadow-lg z-30",
            style: "width:min(320px,calc(100% - 24px));padding:10px;",
            onkeydown: move |event| { if event.key() == Key::Escape { event.stop_propagation(); on_close.call(()); } },
            div { style: "display:flex;align-items:center;justify-content:space-between;margin-bottom:8px;",
                strong { class: "text-xs", "Stickers" }
                button { r#type: "button", title: "Close stickers", aria_label: "Close stickers", onclick: move |_| on_close.call(()), "✕" }
            }
            input { r#type: "search", placeholder: "Find a sticker…", aria_label: "Find a sticker", value: query(),
                style: "width:100%;padding:7px 9px;margin-bottom:8px;border-radius:6px;border:1px solid var(--border);background-color:var(--bg2);color:var(--text);font-size:12px;",
                oninput: move |event| query.set(event.value()),
                onmounted: move |event| { spawn(async move { let _ = event.data().set_focus(true).await; }); }
            }
            div { style: "max-height:260px;overflow-y:auto;display:grid;grid-template-columns:repeat(3,minmax(0,1fr));gap:6px;",
                for sticker in &visible {
                    button { key: "{sticker.id}", r#type: "button", class: "dxf-sticker-choice", disabled: !can_send, title: sticker.shortcode.clone(), aria_label: format!("Send sticker {}", sticker.shortcode),
                        onclick: { let send = gateway.clone(); let id = sticker.id; move |_| { send.send(ClientMessage::SendSticker { channel_id, sticker_id: id }); on_close.call(()); } },
                        if let Some(src) = snapshot.media_src(&sticker.image).map(str::to_owned) { img { src, alt: sticker.shortcode.clone(), style: "width:100%;height:72px;object-fit:contain;" } }
                        else { span { class: "text-xs", "{sticker.shortcode}" } }
                    }
                }
            }
            if visible.is_empty() {
                p { class: "text-xs text-[var(--text-dim)]", style: "padding:12px 4px;", if !loaded { "Loading stickers…" } else if stickers.is_empty() { "This guild has no stickers yet. Add them in Guild settings → Stickers." } else { "No stickers match your search." } }
            }
        }
    }
}

#[component]
pub(super) fn StickerSettings(guild_id: Id) -> Element {
    let state = use_app_state();
    let gateway = use_gateway();
    let mut sticker_name = use_signal(String::new);
    let mut sticker_image = use_signal(|| None::<String>);
    let mut error = use_signal(|| None::<String>);
    let fetch = gateway.clone();
    use_hook(move || fetch.send(ClientMessage::FetchGuildStickers { guild_id }));
    if !state.read().can(guild_id, Permission::ManageEmojis) {
        return rsx! {};
    }
    let stickers = state
        .read()
        .guild_stickers
        .get(&guild_id)
        .cloned()
        .unwrap_or_default();
    rsx! { div { class: "text-xs space-y-3",
        p { "PNG, JPEG, GIF or WebP, up to 2 MB." }
        div { class: "flex flex-wrap gap-3",
            for sticker in stickers { div { key: "{sticker.id}",
                if let Some(src) = state.read().media_src(&sticker.image).map(str::to_owned) { img { src, alt: sticker.shortcode.clone(), style: "width:80px;height:80px;object-fit:contain;" } }
                p { "{sticker.shortcode}" }
                button { onclick: { let g = gateway.clone(); let id = sticker.id; move |_| g.send(ClientMessage::DeleteGuildSticker { guild_id, sticker_id: id }) }, "Delete" }
            } }
        }
                div {
                    input { placeholder: "Sticker name", maxlength: 32, value: sticker_name(), oninput: move |e| sticker_name.set(e.value()) }
                    button { onclick: move |_| { spawn(async move {
                        if let Some(file) = rfd::AsyncFileDialog::new().add_filter("Image", &["png", "gif", "webp", "jpg"]).pick_file().await {
                            let path = file.path().to_owned();
                            match tokio::task::spawn_blocking(move || crate::chat_image::read_file(&path)).await {
                                Ok(Ok(image)) => { sticker_image.set(Some(image)); error.set(None); }
                                Ok(Err(e)) => error.set(Some(e)), Err(e) => error.set(Some(e.to_string())),
                            }
                        }
                    }); }, "Choose image" }
                    if let Some(src) = sticker_image() { img { src, style: "width: 80px; height: 80px; object-fit: contain;" } }
                    button { disabled: sticker_image().is_none() || sticker_name().trim().is_empty(),
                        onclick: { let g = gateway.clone(); move |_| {
                            if let Some(image) = sticker_image() { g.send(ClientMessage::CreateGuildSticker { guild_id, name: sticker_name(), image }); }
                        } }, "Add sticker" }
                }
        if let Some(message) = error() { p { "{message}" } }
    } }
}

pub(super) async fn choose_file(policy: FilePolicy) -> Result<Option<(String, String)>, String> {
    let Some(selected) = rfd::AsyncFileDialog::new().pick_file().await else {
        return Ok(None);
    };
    let path = selected.path().to_owned();
    tokio::task::spawn_blocking(move || read_file(&path, policy.max_bytes))
        .await
        .map_err(|e| e.to_string())?
        .map(Some)
}

#[component]
pub(super) fn FileLimits(guild_id: Id, policy: FilePolicy) -> Element {
    let gateway = use_gateway();
    let initial = policy.clone();
    let mut maximum = use_signal(move || (initial.max_bytes / 1000).to_string());
    let mut days = use_signal(move || policy.retention_days.to_string());
    let mut error = use_signal(|| None::<String>);
    rsx! {
        div { class: "flex gap-2 py-2",
            label { "Maximum KB ", input { r#type: "number", min: 1, max: 2000, value: maximum(), oninput: move |e| maximum.set(e.value()) } }
            label { "Days ", input { r#type: "number", min: 1, max: 365, value: days(), oninput: move |e| days.set(e.value()) } }
            button { onclick: move |_| {
                match (maximum().parse::<u64>(), days().parse::<u32>()) {
                    (Ok(kb), Ok(days)) if (1..=2000).contains(&kb) && (1..=365).contains(&days) => { error.set(None); gateway.send(ClientMessage::SetGuildFilePolicy { guild_id, policy: FilePolicy { max_bytes: kb * 1000, retention_days: days } }); }
                    _ => error.set(Some("Choose 1–2000 KB and 1–365 days.".into())),
                }
            }, "Save file limits" }
            if let Some(error) = error() { span { "{error}" } }
        }
    }
}

#[component]
pub(super) fn FileAttachment(attachment: Attachment) -> Element {
    let mut state = use_app_state();
    let mut busy = use_signal(|| false);
    let mut now = use_signal(|| chrono::Utc::now().timestamp_millis());
    use_future(move || async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            now.set(chrono::Utc::now().timestamp_millis());
        }
    });
    let expired = attachment.expires_ms.is_some_and(|ms| ms <= now());
    rsx! {
        div { class: "py-2 text-xs",
            span { "📎 {attachment.name} ({attachment.bytes / 1000} KB) " }
            if expired { span { "File expired" } }
            else { button { disabled: busy(), onclick: move |_| {
                busy.set(true);
                let attachment = attachment.clone();
                spawn(async move {
                    let result = async {
                        let Some(selected) = rfd::AsyncFileDialog::new().set_file_name(&attachment.name).save_file().await else { return Ok::<(), String>(()); };
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
                        if bytes.len() as u64 != attachment.bytes || bytes.len() > 2_000_000 { return Err("Invalid file size".into()); }
                        let path = selected.path().to_owned();
                        tokio::task::spawn_blocking(move || {
                            save_download(&path, &bytes)?;
                            open_download_folder(&path).map_err(|e| format!("The file was saved, but its folder could not be opened: {e}"))
                        }).await.map_err(|e| e.to_string())??;
                        Ok(())
                    }.await;
                    if let Err(error) = result { state.write().error_toast = Some(error); }
                    busy.set(false);
                });
            }, if busy() { "Downloading…" } else { "Save file…" } } }
        }
    }
}

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

fn open_download_folder(path: &std::path::Path) -> Result<(), String> {
    let folder = download_folder(path)?;
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
    fn search_dates_include_the_entire_last_day() {
        let query = search_query("  hello  ", "", "2024-02-29", "2024-02-29", true, false).unwrap();
        assert_eq!(query.text, "hello");
        assert_eq!(query.author, None);
        assert!(query.has_attachment);
        assert_eq!(
            query.before_ms.unwrap() - query.after_ms.unwrap(),
            86_400_000
        );
        assert_eq!(query.before_ms, date_ms("2024-03-01", false).unwrap());
    }

    #[test]
    fn invalid_search_dates_cannot_silently_drop_the_filter() {
        assert!(search_query("", "", "2025-02-29", "", false, false).is_err());
        assert!(search_query("", "", "2024-03-02", "2024-03-01", false, false).is_err());
        let query = search_query("", "author", "", "", false, true).unwrap();
        assert_eq!(query.author.as_deref(), Some("author"));
        assert!(query.pinned_only);
        assert_eq!(query.after_ms, None);
        assert_eq!(query.before_ms, None);
    }

    #[test]
    fn file_selection_obeys_the_guild_limit() {
        let path = std::env::temp_dir().join(format!("discordia {}.bin", uuid::Uuid::new_v4()));
        std::fs::write(&path, [42; 1001]).unwrap();
        assert!(read_file(&path, 1000).is_err());
        assert!(read_file(&path, 1001).is_ok());
        std::fs::write(&path, []).unwrap();
        assert!(read_file(&path, 1000).is_err());
        std::fs::remove_file(path).unwrap();
    }

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
