use crate::state::use_app_state;
use dioxus::prelude::*;

#[component]
pub fn ImageViewer() -> Element {
    let state = use_app_state();
    let image = use_memo(move || {
        state
            .read()
            .image_viewer
            .as_ref()
            .map(|data| crate::image_assets::chat_source(std::sync::Arc::new(data.clone())))
    });
    rsx! {
        for src in image() {
            ZoomViewer { key: "{src}", src }
        }
    }
}

#[component]
fn ZoomViewer(src: crate::image_assets::ImageSource) -> Element {
    let mut state = use_app_state();
    let instance = use_hook(|| uuid::Uuid::new_v4().to_string());
    let mut mounted = use_signal(|| false);
    crate::mobile_back::use_handler(crate::mobile_back::SETTINGS + 1, true, move |_| {
        state.write().image_viewer = None
    });
    {
        let instance = instance.clone();
        use_effect(move || {
            if !mounted() {
                return;
            }
            let instance = instance.clone();
            spawn(async move {
                let mut bridge = document::eval(&format!(
                    "return ({})({instance:?});",
                    include_str!("../../assets/image-viewer.js")
                ));
                while let Ok(action) = bridge.recv::<String>().await {
                    if action == "close" {
                        state.write().image_viewer = None;
                        break;
                    }
                }
            });
        });
    }
    use_drop(move || {
        document::eval(&format!(
            "if(window.__dxfImageViewerOff?.instance==={instance:?})window.__dxfImageViewerOff();"
        ));
    });
    rsx! {
        div {
            id: "dxf-image-viewer", class: "dxf-image-viewer", role: "dialog", aria_modal: "true",
            aria_label: crate::i18n::text("Image viewer", &[]), tabindex: "-1",
            onmounted: move |_| mounted.set(true),
            div { class: "dxf-image-stage", "data-viewer-stage": "",
                img { src: "{src}", draggable: "false", alt: crate::i18n::text("attachment", &[]) }
            }
            div { class: "dxf-image-controls", role: "toolbar", aria_label: crate::i18n::text("Image zoom", &[]),
                button { r#type: "button", "data-viewer-action": "out", title: crate::i18n::text("Zoom out", &[]), aria_label: crate::i18n::text("Zoom out", &[]),
                    span { aria_hidden: "true", "−" }
                }
                button { r#type: "button", "data-viewer-action": "reset", title: crate::i18n::text("Reset zoom", &[]), aria_label: crate::i18n::text("Reset zoom", &[]),
                    span { "data-viewer-zoom": "" }
                }
                button { r#type: "button", "data-viewer-action": "in", title: crate::i18n::text("Zoom in", &[]), aria_label: crate::i18n::text("Zoom in", &[]), dangerous_inner_html: super::icons::PLUS }
                button { r#type: "button", "data-viewer-action": "close", title: crate::i18n::text("Close", &[]), aria_label: crate::i18n::text("Close", &[]), dangerous_inner_html: super::icons::CLOSE }
            }
        }
    }
}
