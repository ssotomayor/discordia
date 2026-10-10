use base64::Engine as _;
use dioxus::prelude::*;

use crate::protocol::{Attachment, ClientMessage, FilePolicy, Id, MessageSearch, Permission};
use crate::state::{ConnectionStatus, use_app_state, use_gateway};

fn read_file(path: &std::path::Path) -> Result<(String, String), String> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(2_000_001)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.is_empty() || bytes.len() > 2_000_000 {
        return Err("Choose a nonempty file up to 2 MB.".into());
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
    let mut sticker_name = use_signal(String::new);
    let mut sticker_image = use_signal(|| None::<String>);
    let guild_id = state
        .read()
        .channels
        .iter()
        .find(|c| c.id == channel_id)
        .map(|c| c.guild_id);
    let Some(guild_id) = guild_id else {
        return rsx! {};
    };
    let manage = state.read().can(guild_id, Permission::ManageEmojis);
    let can_send = state.read().can(guild_id, Permission::SendMessages);
    let fetch = gateway.clone();
    use_effect(move || {
        fetch.send(ClientMessage::FetchGuildFilePolicy { guild_id });
        fetch.send(ClientMessage::FetchGuildStickers { guild_id });
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
    let stickers = state
        .read()
        .guild_stickers
        .get(&guild_id)
        .cloned()
        .unwrap_or_default();
    let members: Vec<_> = state
        .read()
        .members_of(guild_id)
        .into_iter()
        .cloned()
        .collect();
    rsx! {
        div { class: "px-3 py-2 border-b border-[var(--border)] text-xs", style: "max-height: 45%; overflow-y: auto;",
            div { class: "flex items-center gap-3",
                button { onclick: move |_| panel.set(if panel() == "search" { "" } else { "search" }), "Search" }
                button { onclick: { let mut search = search.clone(); move |_| { panel.set("pins"); search(0, true); } }, "Pins" }
                button { onclick: move |_| panel.set(if panel() == "stickers" { "" } else { "stickers" }), "Stickers" }
                if !panel().is_empty() { button { onclick: move |_| panel.set(""), "Close" } }
            }
            if let Some(error) = error() { p { "{error}" } }
            if panel() == "search" {
                div { class: "flex flex-wrap gap-2 py-2",
                    input { placeholder: "Search messages", maxlength: 200, value: text(), oninput: move |e| text.set(e.value()) }
                    select { value: author(), onchange: move |e| author.set(e.value()),
                        option { value: "", "Any author" }
                        for member in members { option { value: member.user.pubkey.clone(), "{member.user.username}" } }
                    }
                    label { "From (UTC) ", input { r#type: "date", value: after(), oninput: move |e| after.set(e.value()) } }
                    label { "Through (UTC) ", input { r#type: "date", value: before(), oninput: move |e| before.set(e.value()) } }
                    label { input { r#type: "checkbox", checked: files(), onchange: move |e| files.set(e.checked()) } "Has attachment" }
                    label { input { r#type: "checkbox", checked: pins(), onchange: move |e| pins.set(e.checked()) } "Pinned" }
                    button { onclick: { let mut search = search.clone(); move |_| search(0, false) }, "Search" }
                }
            }
            if panel() == "search" || panel() == "pins" {
                if let Some(results) = results {
                    if results.is_empty() { p { "No matching messages." } }
                    for message in &results { super::chat::MessageRow { key: "{message.id}", message: message.clone(), grouped: false } }
                    div { class: "flex gap-3",
                        if offset() > 0 { button { onclick: { let mut search = search.clone(); move |_| search(offset().saturating_sub(100), panel() == "pins") }, "Newer" } }
                        if results.len() == 100 { button { onclick: { let mut search = search.clone(); move |_| search(offset() + 100, panel() == "pins") }, "Older" } }
                    }
                } else if request().is_some() { p { "Searching…" } }
            }
            if panel() == "stickers" {
                div { class: "flex flex-wrap gap-3 py-2",
                    for sticker in stickers {
                        div { key: "{sticker.id}",
                            button { disabled: !can_send, title: sticker.shortcode.clone(),
                                onclick: { let g = gateway.clone(); let id = sticker.id; move |_| g.send(ClientMessage::SendSticker { channel_id, sticker_id: id }) },
                                if let Some(src) = state.read().media_src(&sticker.image).map(str::to_owned) { img { src, alt: sticker.shortcode.clone(), style: "width: 80px; height: 80px; object-fit: contain;" } }
                                else { "{sticker.shortcode}" }
                            }
                            if manage { button { title: "Delete sticker", onclick: { let g = gateway.clone(); let id = sticker.id; move |_| g.send(ClientMessage::DeleteGuildSticker { guild_id, sticker_id: id }) }, "Delete" } }
                        }
                    }
                }
                if manage {
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
            }
        }
    }
}

#[component]
pub(super) fn FileSender(guild_id: Id, channel_id: Id) -> Element {
    let state = use_app_state();
    let gateway = use_gateway();
    let mut file = use_signal(|| None::<(String, String)>);
    let mut caption = use_signal(String::new);
    let mut pending = use_signal(|| None::<Id>);
    let mut error = use_signal(|| None::<String>);
    let mut reading = use_signal(|| false);
    let policy = state
        .read()
        .guild_file_policies
        .get(&guild_id)
        .cloned()
        .unwrap_or_default();
    use_effect(move || {
        let result = state.read().file_upload_result.clone();
        if let Some((id, why)) = result
            && Some(id) == pending()
        {
            pending.set(None);
            if let Some(why) = why {
                error.set(Some(why));
            } else {
                file.set(None);
                caption.set(String::new());
                error.set(None);
            }
        }
    });
    rsx! {
        div { class: "py-2 flex flex-wrap gap-2",
            p { "Send as a downloadable file, without a preview." }
            p { "Limit: {policy.max_bytes / 1000} KB. Files expire after {policy.retention_days} days." }
            button { disabled: reading() || pending().is_some(), onclick: move |_| { spawn(async move {
                if let Some(selected) = rfd::AsyncFileDialog::new().pick_file().await {
                    reading.set(true);
                    let path = selected.path().to_owned();
                    let result = tokio::task::spawn_blocking(move || read_file(&path)).await;
                    reading.set(false);
                    match result { Ok(Ok(value)) => { file.set(Some(value)); error.set(None); }, Ok(Err(e)) => error.set(Some(e)), Err(e) => error.set(Some(e.to_string())) }
                }
            }); }, "Choose file" }
            if let Some((name, _)) = file() { span { "{name}" } }
            input { placeholder: "Optional message", maxlength: 2000, value: caption(), disabled: pending().is_some(), oninput: move |e| caption.set(e.value()) }
            button { disabled: file().is_none() || pending().is_some(), onclick: move |_| {
                if state.read().status != ConnectionStatus::Ready { error.set(Some("Reconnect before sending. Your draft has been kept.".into())); return; }
                if let Some((name, data_url)) = file() {
                    let id = uuid::Uuid::new_v4(); pending.set(Some(id)); error.set(None);
                    gateway.send(ClientMessage::SendFile { request_id: id, channel_id, name, data_url, content: caption(), reply_to: None });
                    spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                        if pending() == Some(id) { pending.set(None); error.set(Some("No upload confirmation. Your draft is kept; check the chat before retrying.".into())); }
                    });
                }
            }, if pending().is_some() { "Uploading…" } else { "Send file" } }
            if let Some(error) = error() { p { "{error}" } }
        }
        if state.read().can(guild_id, Permission::ManageGuild) { FileLimits { guild_id, policy } }
    }
}

#[component]
fn FileLimits(guild_id: Id, policy: FilePolicy) -> Element {
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
