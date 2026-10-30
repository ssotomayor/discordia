use crate::{
    nostr::{
        dm_image::{Image, Status},
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
    let mut attempt = use_signal(|| None::<uuid::Uuid>);
    let viewer = use_hook(uuid::Uuid::new_v4);
    use_effect(move || {
        if let Some(image) = state.write().dm_images.get_mut(&message_id) {
            image.viewers.insert(viewer);
        }
    });
    let effect_owner = identity.pubkey.clone();
    use_effect(move || {
        let wanted = {
            let s = state.read();
            let allowed = s
                .dm_of(channel_id)
                .is_some_and(|dm| s.dm_standing(&dm.other_pubkey) == DmStanding::Inbox);
            s.dm_images
                .get(&message_id)
                .filter(|image| !image.upload && image.status == Status::Waiting && allowed)
                .cloned()
        };
        let Some(image) = wanted else {
            return;
        };
        let token = uuid::Uuid::new_v4();
        attempt.set(Some(token));
        if let Some(image) = state.write().dm_images.get_mut(&message_id) {
            image.status = Status::Loading(token);
        }
        let account = effect_owner.clone();
        spawn(async move {
            let result = crate::nostr::dm_image::receive(&account, &image.descriptor).await;
            let mut s = state.write();
            if !s
                .dm_images
                .get(&message_id)
                .is_some_and(|image| image.status == Status::Loading(token))
                || !s
                    .dm_of(channel_id)
                    .is_some_and(|dm| s.dm_standing(&dm.other_pubkey) == DmStanding::Inbox)
            {
                return;
            }
            match result {
                Ok(data) => {
                    if let Some(image) = s.dm_images.get_mut(&message_id) {
                        image.status = Status::Ready;
                        image.data = Some(std::sync::Arc::new(data));
                    }
                }
                Err(error) => {
                    if let Some(image) = s.dm_images.get_mut(&message_id) {
                        image.status = Status::Failed(error);
                    }
                }
            }
        });
    });
    use_drop(move || {
        if let Ok(mut s) = state.try_write()
            && let Some(image) = s.dm_images.get_mut(&message_id)
        {
            image.viewers.remove(&viewer);
            let ours =
                matches!(image.status, Status::Loading(token) if Some(token) == *attempt.peek());
            if !image.upload
                && (ours || (image.viewers.is_empty() && image.status == Status::Ready))
            {
                image.status = Status::Waiting;
            }
            if image.viewers.is_empty() {
                image.data = None;
            }
        }
    });
    let source = use_memo(move || {
        state
            .read()
            .dm_images
            .get(&message_id)?
            .data
            .clone()
            .map(crate::image_assets::chat_source)
    });
    let Some(Image {
        descriptor,
        status,
        upload,
        ..
    }) = state.read().dm_images.get(&message_id).cloned()
    else {
        return rsx! {};
    };
    let account = identity.pubkey.clone();
    let mut opening_full = use_signal(|| false);
    if status == Status::Ready {
        return rsx! {
            if let Some(src) = source() {
                img {
                    class: "mt-1 rounded-lg border border-[var(--border-strong)] bg-[var(--panel2)] object-contain block hover:border-[var(--accent)] hover:brightness-105 transition-all shadow-sm",
                    style: "max-width: 320px; max-height: 240px; width: auto; height: auto; cursor: zoom-in;",
                    src: "{src}",
                    alt: crate::i18n::text("attachment", &[]),
                    title: crate::i18n::text("Click to view full size", &[]),
                    onclick: move |_| {
                        if *opening_full.peek() {
                            return;
                        }
                        opening_full.set(true);
                        let desc = descriptor.clone();
                        let acc = account.clone();
                        spawn(async move {
                            if let Ok(full) = crate::nostr::dm_image::load_original(&acc, &desc).await {
                                state.write().image_viewer = Some(full);
                            }
                            opening_full.set(false);
                        });
                    },
                }
            }
        };
    }
    let label = match &status {
        Status::Uploading => "Uploading encrypted image…",
        Status::Loading(_) => "Loading encrypted image…",
        Status::Waiting => "Encrypted image — accept this conversation to load it.",
        Status::Evicted => "Image ready to load.",
        Status::Failed(error) => error,
        Status::Ready => "",
    };
    let retry_nostr = nostr.clone();
    rsx! {
        div {
            class: "mt-1 rounded-lg border border-[var(--border)] px-3 py-2 text-xs text-[var(--text-muted)] bg-[var(--panel2)] flex items-center justify-between",
            style: "max-width: 320px; min-height: 48px;",
            span { {crate::i18n::message(label)} }
            if matches!(status, Status::Failed(_) | Status::Evicted) {
                button {
                    r#type: "button",
                    class: "ml-3 text-[var(--accent)] hover:underline whitespace-nowrap",
                    onclick: move |_| {
                        if upload {
                            retry_nostr.send(NostrCmd::Retry { message_id });
                        } else if let Some(image) = state.write().dm_images.get_mut(&message_id) {
                            image.status = Status::Waiting;
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
