use crate::{
    nostr::{
        dm_image::{Status, finish_load},
        service::{NostrCmd, NostrTx},
    },
    protocol::Id,
    state::{DmStanding, use_app_state},
};
use dioxus::prelude::*;

#[component]
pub(super) fn EncryptedImage(channel_id: Id, message_id: Id) -> Element {
    let mut state = use_app_state();
    let identity = use_context::<crate::identity::Identity>();
    let nostr = use_context::<NostrTx>();
    let viewer = use_hook(uuid::Uuid::new_v4);
    let allowed = move |s: &crate::state::AppState| {
        s.dm_of(channel_id)
            .is_some_and(|dm| s.dm_standing(&dm.other_pubkey) == DmStanding::Inbox)
    };
    use_effect(move || {
        let Some(previews) = crate::image_assets::preview_density() else {
            return;
        };
        let changed = state
            .read()
            .dm_images
            .get(&message_id)
            .is_some_and(|image| image.wants(viewer) != Some(!previews));
        if changed && let Some(image) = state.write().dm_images.get_mut(&message_id) {
            image.see(viewer, !previews);
        }
    });
    let effect_owner = identity.pubkey.clone();
    use_effect(move || {
        if crate::image_assets::preview_density().is_none() {
            return;
        }
        let due = {
            let s = state.read();
            allowed(&s)
                && s.dm_images
                    .get(&message_id)
                    .is_some_and(|image| *image.status() == Status::Waiting)
        };
        if !due {
            return;
        }
        let started = state
            .write()
            .dm_images
            .get_mut(&message_id)
            .and_then(|image| Some((image.start(viewer)?, image.descriptor.clone())));
        let Some(((token, original), descriptor)) = started else {
            return;
        };
        let account = effect_owner.clone();
        spawn(async move {
            let result = if original {
                crate::nostr::dm_image::load_original(&account, &descriptor).await
            } else {
                crate::nostr::dm_image::receive(&account, &descriptor).await
            };
            let mut s = state.write();
            let allowed = allowed(&s);
            finish_load(&mut s.dm_images, message_id, token, allowed, result);
        });
    });
    use_drop(move || {
        if let Ok(mut s) = state.try_write()
            && let Some(image) = s.dm_images.get_mut(&message_id)
        {
            image.leave(viewer);
        }
    });
    let source = use_memo(move || {
        state
            .read()
            .dm_images
            .get(&message_id)?
            .ready()
            .map(|(data, _)| crate::image_assets::chat_source(data.clone()))
    });
    let Some(image) = state.read().dm_images.get(&message_id).cloned() else {
        return rsx! {};
    };
    let upload = image.is_upload();
    let status = image.status().clone();
    let descriptor = image.descriptor;
    let account = identity.pubkey.clone();
    let details_name = descriptor.name.clone();
    let details_width = descriptor.width;
    let details_height = descriptor.height;
    if let Status::Ready { .. } = status {
        return rsx! {
            if let Some(src) = source() {
                super::image_attachment::ImageAttachment {
                    source:src,
                    size: details_width.zip(details_height),
                      zoomable:true,
                    onclick: move |_| {

                        let view={let s=state.peek();s.dm_images.get(&message_id).and_then(|image|image.ready().map(|(data,original)|{
                            let target=(!original).then(||super::image_viewer::Original::Dm {owner:account.clone(),descriptor:Box::new(descriptor.clone())});
                            super::image_viewer::ImageView::new(data.clone(),target)
                        }))};
                        if let Some(view)=view {state.write().image_viewer=Some(view);}
                    },
                }
            }
            EncryptedImageDetails { name: details_name }
        };
    }
    let label = match &status {
        Status::Uploading => "Uploading encrypted image…",
        Status::Loading { .. } => "Loading encrypted image…",
        Status::Waiting => "Encrypted image — accept this conversation to load it.",
        Status::Evicted => "Image ready to load.",
        Status::Failed(error) | Status::UploadFailed(error) => error,
        Status::Ready { .. } => "",
    };
    let retry_nostr = nostr.clone();
    rsx! {
        div {
            class: "mt-1 rounded-lg border border-[var(--border)] px-3 py-2 text-xs text-[var(--text-muted)] bg-[var(--panel2)] flex items-center justify-between",
            // Every state holds the picture's box: leaving a DM unloads its pictures,
            // so each one passes through these on every visit.
            style: "{super::image_attachment::frame(details_width.zip(details_height))} min-width: min(240px, 100%); min-height: 48px;",
            span { class: if matches!(status, Status::Loading { .. }) { "dxf-late" } else { "" }, {crate::i18n::message(label)} }
            if matches!(status, Status::Failed(_) | Status::UploadFailed(_) | Status::Evicted) {
                button {
                    r#type: "button",
                    class: "ml-3 text-[var(--accent)] hover:underline whitespace-nowrap",
                    onclick: move |_| {
                        if upload {
                            retry_nostr.send(NostrCmd::Retry { message_id });
                        } else if let Some(image) = state.write().dm_images.get_mut(&message_id) {
                            image.retry();
                        }
                    },
                    {crate::i18n::text(if status == Status::Evicted { "Load image" } else { "Retry" }, &[])}
                }
            }
            if upload {
                button {
                    r#type: "button",
                    class: "ml-3 text-[var(--text-muted)] hover:underline whitespace-nowrap",
                    onclick: move |_| nostr.send(NostrCmd::CancelImage { message_id }),
                    {crate::i18n::text("Cancel", &[])}
                }
            }
        }
        EncryptedImageDetails { name: details_name }
    }
}

/// Never mounts `EncryptedImage`: a view there is what starts a load, and into the cache.
#[component]
pub(super) fn ViewOnce(channel_id: Id, message_id: Id) -> Element {
    let mut state = use_app_state();
    let identity = use_context::<crate::identity::Identity>();
    let nostr = use_context::<NostrTx>();
    let mut opening = use_signal(|| false);
    let Some(image) = state.read().dm_images.get(&message_id).cloned() else {
        return rsx! {};
    };
    let (mine, accepted) = {
        let s = state.read();
        let mine = s.messages.get(&channel_id).is_some_and(|messages| {
            messages
                .iter()
                .any(|m| m.id == message_id && m.author.pubkey == identity.pubkey)
        });
        let accepted = s
            .dm_of(channel_id)
            .is_some_and(|dm| s.dm_standing(&dm.other_pubkey) == DmStanding::Inbox);
        (mine, accepted)
    };
    let status = image.status().clone();
    let upload = image.is_upload();
    if mine {
        let label = match &status {
            Status::Uploading => "Sending a view-once picture…",
            Status::UploadFailed(error) | Status::Failed(error) => error,
            _ => "View-once picture · not opened yet",
        };
        let retry = nostr.clone();
        return rsx! {
            div { class: "dxf-view-once",
                span { {crate::i18n::message(label)} }
                if matches!(status, Status::UploadFailed(_)) {
                    button { r#type: "button", onclick: move |_| retry.send(NostrCmd::Retry { message_id }), {crate::i18n::text("Retry", &[])} }
                }
                if upload {
                    button { r#type: "button", onclick: move |_| nostr.send(NostrCmd::CancelImage { message_id }), {crate::i18n::text("Cancel", &[])} }
                }
            }
        };
    }
    let descriptor = image.descriptor;
    rsx! {
        button {
            r#type: "button",
            class: "dxf-view-once dxf-view-once-open",
            disabled: opening() || !accepted,
            onclick: move |_| {
                if opening() {
                    return;
                }
                opening.set(true);
                let descriptor = descriptor.clone();
                let nostr = nostr.clone();
                spawn(async move {
                    match crate::nostr::dm_image::open_once(&descriptor).await {
                        Ok(data) => {
                            let view = super::image_viewer::ImageView::new(std::sync::Arc::new(data), None);
                            state.write().image_viewer = Some(view);
                            // Deleting is what makes it once; the viewer holds the only copy left.
                            nostr.send(NostrCmd::Delete { channel_id, message_id });
                        }
                        Err(error) => {
                            opening.set(false);
                            state.write().error_toast = Some(error);
                        }
                    }
                });
            },
            if opening() {
                span { class: "dx-spinner" }
                {crate::i18n::text("Opening…", &[])}
            } else if accepted {
                {crate::i18n::text("View-once picture · press to open", &[])}
            } else {
                {crate::i18n::text("View-once picture — accept this conversation to open it.", &[])}
            }
        }
    }
}

#[component]
fn EncryptedImageDetails(name: Option<String>) -> Element {
    rsx! {
        if let Some(name) = name {
            div {
                class: "mt-1 text-[10px] text-[var(--text-dim)]",
                style: "max-width:20rem;overflow-wrap:anywhere;",
                "{name}"
            }
        }
    }
}

#[component]
pub(crate) fn FileServerField() -> Element {
    let mut settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let mut custom = use_signal(String::new);
    let mut error = use_signal(|| None::<String>);
    let current = settings.read().dm_blob_server.clone();
    let choices = settings.read().blob_server_choices();
    rsx! {
        details { class: "text-[10px] text-[var(--text-dim)] mt-2",
            summary { class: "cursor-pointer", {crate::i18n::text("Encrypted DM images — file server", &[])} }
            p { class: "my-1", {crate::i18n::text("Images are encrypted before uploading. Maximum 10 MiB per image. The server must accept encrypted binary files.", &[])} }
            select { class: "rounded border border-[var(--border)] bg-[var(--panel)] text-[var(--text)] p-1 w-full", aria_label: crate::i18n::text("File server", &[]),
                onchange: move |event| {
                    let mut next = settings.peek().clone();
                    let value = event.value();
                    next.dm_blob_server = (!value.is_empty()).then_some(value);
                    crate::settings::save(&next); settings.set(next);
                },
                option { value: "", selected: current.is_none(), {crate::i18n::text("Off", &[])} }
                for url in choices { option { key: "{url}", value: "{url}", selected: current.as_deref() == Some(url.as_str()), "{url}" } }
            }
            div { class: "flex gap-1 mt-1",
                input { class: "min-w-0 flex-1 rounded border border-[var(--border)] bg-transparent p-1", placeholder: "https://blossom.example", value: "{custom}", oninput: move |event| custom.set(event.value()) }
                button { r#type: "button", class: "text-[var(--accent)]", onclick: move |_| {
                    match crate::nostr::dm_image::public_url(&custom()) {
                        Ok(url) => {
                            let mut next = settings.peek().clone();
                            let url = url.as_str().trim_end_matches('/').to_owned();
                            if !next.blob_servers.contains(&url) { next.blob_servers.push(url.clone()); next.blob_servers.truncate(8); }
                            next.dm_blob_server = Some(url); crate::settings::save(&next); settings.set(next); custom.set(String::new()); error.set(None);
                        }
                        Err(why) => error.set(Some(why)),
                    }
                }, {crate::i18n::text("Add", &[])} }
            }
            if let Some(why) = error() { p { class: "text-[var(--danger)]", "{why}" } }
        }
    }
}
