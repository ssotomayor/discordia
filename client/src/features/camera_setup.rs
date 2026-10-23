use dioxus::prelude::*;

use crate::camera_quality::CameraQuality;
use crate::state::{use_app_state, use_gateway};

#[component]
pub fn CameraSourcePicker() -> Element {
    let state = use_app_state();
    let open =
        use_memo(move || state.read().camera_picker && state.read().voice.channel_id.is_some());
    rsx! { if open() { CameraSetupDialog {} } }
}

#[component]
fn CameraSetupDialog() -> Element {
    let mut state = use_app_state();
    let mut settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let gateway = use_gateway();
    let epoch = use_hook(move || state.read().voice_session_epoch);
    let mut device = use_signal(move || {
        let cfg = settings.read();
        super::resolve_device(
            &state.read().available_cameras,
            cfg.camera_device_id.as_deref(),
            cfg.camera_device_label.as_deref(),
        )
        .unwrap_or_default()
    });
    let mut quality = use_signal(move || settings.read().camera_quality.normalized());
    #[cfg_attr(target_os = "android", allow(unused_mut, unused_variables))]
    let mut revision = use_signal(|| 0_u64);
    #[cfg(target_os = "windows")]
    let mut image = use_signal(String::new);
    #[cfg(not(target_os = "windows"))]
    let image = use_signal(String::new);
    #[cfg(target_os = "android")]
    let ready = use_signal(|| true);
    #[cfg(not(target_os = "android"))]
    let mut ready = use_signal(|| false);
    #[cfg_attr(target_os = "android", allow(unused_mut))]
    let mut error = use_signal(|| None::<String>);
    #[cfg_attr(target_os = "android", allow(unused_mut))]
    let mut dimensions = use_signal(|| None::<(u32, u32)>);
    let mut submitting = use_signal(|| false);
    use_hook(move || super::refresh_cameras(state));
    use_effect(|| {
        let _ = document::eval("document.getElementById('camera-setup-dialog')?.focus();");
    });

    #[cfg(target_os = "windows")]
    let controller = use_hook(crate::syscamera::preview_controller);
    #[cfg(target_os = "windows")]
    {
        let commands = controller.0.clone();
        use_effect(move || {
            let selected = device();
            let quality = quality();
            let next = *revision.peek() + 1;
            revision.set(next);
            ready.set(false);
            image.set(String::new());
            error.set(None);
            dimensions.set(None);
            if commands
                .send(crate::syscamera::PreviewCommand::Configure {
                    revision: next,
                    device: (!selected.is_empty()).then_some(selected),
                    quality,
                })
                .is_err()
            {
                error.set(Some("Camera preview worker stopped".into()));
            }
        });
        let frames = controller.1.clone();
        use_future(move || {
            let mut frames = frames.clone();
            async move {
                while frames.changed().await.is_ok() {
                    let frame = frames.borrow_and_update().clone();
                    if frame.revision != *revision.peek() {
                        continue;
                    }
                    ready.set(frame.image.is_some() && frame.error.is_none());
                    image.set(frame.image.unwrap_or_default());
                    error.set(frame.error);
                    dimensions.set(frame.dimensions);
                }
            }
        });
    }
    // Android publishes a native capture, and the camera cannot be opened twice:
    // the preview would hold the device the publication needs. The self view
    // comes from the published track once the camera starts.
    #[cfg(not(any(target_os = "android", target_os = "windows")))]
    {
        use_effect(move || {
            let selected = device();
            let quality = quality();
            let next = *revision.peek() + 1;
            revision.set(next);
            ready.set(false);
            error.set(None);
            dimensions.set(None);
            spawn(async move {
                let opts = serde_json::json!({"deviceId": selected, "width": quality.width(), "height": quality.height, "fps": quality.fps});
                let mut eval = document::eval(&format!(
                    "dioxus.send(await window.dxScreen.previewCamera({opts}, 'camera-setup-video'));"
                ));
                let result = eval.recv::<serde_json::Value>().await;
                if *revision.peek() != next {
                    return;
                }
                match result {
                    Ok(value) => {
                        if let Some(message) = value["error"].as_str() {
                            error.set(Some(message.into()));
                        } else {
                            ready.set(true);
                            dimensions.set(
                                value["width"]
                                    .as_u64()
                                    .zip(value["height"].as_u64())
                                    .map(|(w, h)| (w as u32, h as u32)),
                            );
                            super::refresh_cameras(state);
                        }
                    }
                    Err(e) => error.set(Some(format!("Camera preview failed: {e}"))),
                }
            });
        });
        use_drop(|| {
            let _ = document::eval("window.dxScreen.stopCameraPreview();");
        });
    }

    let close = move |_| {
        state.write().camera_picker = false;
    };
    let can_start = ready() && !submitting();
    rsx! {
        div {
            class: "dxf-backdrop-in fixed inset-0 z-50 flex items-center justify-center bg-black/50",
            onclick: close,
            onkeydown: move |e| { if e.key() == Key::Escape { state.write().camera_picker = false; } },
            div {
                id: "camera-setup-dialog", role: "dialog", "aria-modal": "true", "aria-labelledby": "camera-setup-title", tabindex: "-1",
                class: "dxf-modal-in max-h-[80vh] flex flex-col bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg shadow-xl overflow-hidden",
                style: "width: min(38rem, calc(100vw - 2rem));",
                onclick: move |e| e.stop_propagation(),
                div { class: "px-4 py-3 border-b border-[var(--border)] flex items-center",
                    h3 { id: "camera-setup-title", class: "text-sm font-medium text-[var(--accent)] flex-1", {crate::i18n::text("Set up your camera", &[])} }
                    button { title: crate::i18n::text("Close camera setup", &[]), onclick: close, "✕" }
                }
                div { class: "p-3 space-y-3 overflow-y-auto",
                    div { style: "aspect-ratio: 16 / 9; background: #000; position: relative; overflow: hidden; border-radius: 8px;",
                        if cfg!(target_os = "windows") && !image().is_empty() {
                            img { src: image(), alt: crate::i18n::text("Local camera preview", &[]), style: "width: 100%; height: 100%; object-fit: contain; transform: scaleX(-1);" }
                        }
                        div { id: "camera-setup-video", style: "width: 100%; height: 100%;" }
                        if !ready() { p { role: "status", class: "text-xs text-[var(--text-muted)]", style: "position: absolute; inset: 0; display: grid; place-items: center;", if error().is_some() { {crate::i18n::text("Preview unavailable", &[])} } else { {crate::i18n::text("Opening camera…", &[])} } } }
                    }
                    p { class: "text-xs text-[var(--text-muted)]", {crate::i18n::text("Only you can see this preview.", &[])} }
                    label { r#for: "camera-device", class: "text-xs text-[var(--text-muted)]", {crate::i18n::text("Camera", &[])} }
                    select { id: "camera-device", disabled: submitting(), value: device(), class: "w-full bg-[var(--panel-solid)] text-[var(--text)] border border-[var(--border)] rounded px-2 py-1 text-sm",
                        onchange: move |e| device.set(e.value()),
                        option { value: "", {crate::i18n::text("Default camera", &[])} }
                        for camera in state.read().available_cameras.clone() {
                            option { value: camera.id, "{camera.label}" }
                        }
                    }
                    div { style: "display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 12px;",
                        div {
                            label { r#for: "camera-resolution", class: "text-xs text-[var(--text-muted)]", {crate::i18n::text("Resolution", &[])} }
                            select { id: "camera-resolution", disabled: submitting(), value: quality().height.to_string(), class: "w-full bg-[var(--panel-solid)] text-[var(--text)] border border-[var(--border)] rounded px-2 py-1 text-sm",
                                onchange: move |e| { let current = quality(); quality.set(CameraQuality { height: e.value().parse().unwrap_or(720), ..current }.normalized()); },
                                for height in [480, 720, 1080] { option { value: "{height}", {crate::i18n::text("{0}p", &[(height).to_string()])} } }
                            }
                        }
                        div {
                            label { r#for: "camera-fps", class: "text-xs text-[var(--text-muted)]", {crate::i18n::text("Frames per second", &[])} }
                            select { id: "camera-fps", disabled: submitting(), value: quality().fps.to_string(), class: "w-full bg-[var(--panel-solid)] text-[var(--text)] border border-[var(--border)] rounded px-2 py-1 text-sm",
                                onchange: move |e| { let current = quality(); quality.set(CameraQuality { fps: e.value().parse().unwrap_or(30), ..current }.normalized()); },
                                for fps in [15, 30, 60] { option { value: "{fps}", {crate::i18n::text("{0} FPS", &[(fps).to_string()])} } }
                            }
                        }
                    }
                    p { class: "text-xs text-[var(--text-muted)]", {crate::i18n::text("Bitrate adjusts automatically. Your camera may use the nearest supported resolution and FPS.", &[])} }
                    if let Some((width, height)) = dimensions() { p { class: "text-xs text-[var(--text-muted)]", {crate::i18n::text("Preview capture: {0} × {1}", &[(width).to_string(), (height).to_string()])} } }
                    if let Some(message) = error() { p { role: "alert", class: "text-xs text-[var(--accent)]", {crate::i18n::message(&(message).to_string())} } }
                }
                div { class: "px-4 py-3 flex items-center gap-2 border-t border-[var(--border)]",
                    button { disabled: submitting(), onclick: close, {crate::i18n::text("Cancel", &[])} }
                    button {
                        disabled: !can_start, class: "px-3 py-2 rounded bg-[var(--accent)] text-white text-sm",
                        onclick: move |_| {
                            if !ready() || submitting() { return; }
                            submitting.set(true);
                            let gateway = gateway.clone();
                            #[cfg(target_os = "windows")]
                            let commands = controller.0.clone();
                            spawn(async move {
                                #[cfg(target_os = "windows")]
                                {
                                    let (done, stopped) = tokio::sync::oneshot::channel();
                                    if commands.send(crate::syscamera::PreviewCommand::Stop(done)).is_err() || stopped.await.is_err() {
                                        error.set(Some("Could not release the camera preview".into()));
                                        submitting.set(false);
                                        return;
                                    }
                                }
                                #[cfg(not(any(target_os = "android", target_os = "windows")))]
                                { let mut eval = document::eval("window.dxScreen.stopCameraPreview(); dioxus.send(true);"); if eval.recv::<bool>().await.is_err() { submitting.set(false); return; } }
                                if state.read().voice_session_epoch != epoch || !state.read().camera_picker { return; }
                                let selected = device();
                                let mut next = settings.read().clone();
                                next.camera_quality = quality().normalized();
                                next.camera_device_label = state.read().available_cameras.iter().find(|c| c.id == selected).map(|c| c.label.clone());
                                next.camera_device_id = (!selected.is_empty()).then_some(selected);
                                crate::settings::save(&next);
                                settings.set(next);
                                state.write().camera_picker = false;
                                super::toggle_camera(state, settings, &gateway, true);
                            });
                        },
                        if submitting() { {crate::i18n::text("Starting…", &[])} } else { {crate::i18n::text("Start camera", &[])} }
                    }
                }
            }
        }
    }
}
