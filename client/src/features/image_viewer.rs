use crate::state::use_app_state;
use dioxus::prelude::*;

use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone, PartialEq, Eq)]
pub(crate) enum Original {
    Community(String),
    Dm {
        owner: String,
        descriptor: crate::nostr::dm_image::Descriptor,
    },
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Load {
    Loading,
    Failed,
    Ready,
}
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ImageView {
    pub id: Uuid,
    pub data: Arc<String>,
    pub original: Option<Arc<String>>,
    pub target: Option<Original>,
    pub status: Load,
    pub attempt: Uuid,
}
impl ImageView {
    pub fn new(data: Arc<String>, target: Option<Original>) -> Self {
        let status = if target.is_some() {
            Load::Loading
        } else {
            Load::Ready
        };
        Self {
            id: Uuid::new_v4(),
            data,
            original: None,
            target,
            status,
            attempt: Uuid::new_v4(),
        }
    }
    fn loaded(&mut self, attempt: Uuid, data: Result<Arc<String>, ()>) {
        if self.attempt != attempt || self.status != Load::Loading {
            return;
        }
        match data {
            Ok(data) if !data.is_empty() => {
                if data == self.data {
                    self.status = Load::Ready;
                } else {
                    self.original = Some(data);
                }
            }
            _ => self.status = Load::Failed,
        }
    }
    fn finish(&mut self, attempt: Uuid, success: bool) {
        if self.attempt != attempt || self.status != Load::Loading {
            return;
        }
        if success && let Some(data) = self.original.take() {
            self.data = data;
            self.status = Load::Ready;
        } else if !success {
            self.original = None;
            self.status = Load::Failed;
        }
    }
    pub(crate) fn retry(&mut self) {
        self.original = None;
        self.attempt = Uuid::new_v4();
        self.status = Load::Loading;
    }
}
impl From<String> for ImageView {
    fn from(data: String) -> Self {
        Self::new(Arc::new(data), None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn view() -> ImageView {
        ImageView::new(
            Arc::new("preview".into()),
            Some(Original::Community("media:photo".into())),
        )
    }
    #[test]
    fn preview_remains_until_decode_and_session_survives_upgrade() {
        let mut view = view();
        let id = view.id;
        let token = view.attempt;
        view.loaded(token, Ok(Arc::new("original".into())));
        assert_eq!(view.data.as_str(), "preview");
        assert!(view.status == Load::Loading);
        view.finish(token, true);
        assert_eq!(view.id, id);
        assert_eq!(view.data.as_str(), "original");
        assert!(view.status == Load::Ready);
        view.finish(token, false);
        assert!(view.status == Load::Ready);
    }
    #[test]
    fn retry_rejects_stale_network_and_decode_results() {
        let mut view = view();
        let old = view.attempt;
        view.loaded(old, Err(()));
        assert!(view.status == Load::Failed);
        assert_eq!(view.data.as_str(), "preview");
        view.retry();
        view.loaded(old, Ok(Arc::new("stale".into())));
        view.finish(old, true);
        assert!(view.original.is_none());
        assert_eq!(view.data.as_str(), "preview");
        assert!(view.status == Load::Loading);
        view.loaded(view.attempt, Ok(Arc::new("fresh".into())));
        view.finish(view.attempt, false);
        assert!(view.original.is_none());
        assert!(view.status == Load::Failed);
        assert_eq!(view.data.as_str(), "preview");
    }
    #[test]
    fn already_original_source_needs_no_browser_upgrade() {
        let mut view = view();
        view.loaded(view.attempt, Ok(view.data.clone()));
        assert!(view.status == Load::Ready);
        assert!(view.original.is_none());
        let ready: ImageView = "original".to_string().into();
        assert!(ready.status == Load::Ready);
        assert!(ready.target.is_none());
    }

    #[test]
    fn retry_clears_negative_cache_and_ignores_a_replaced_viewer() {
        let mut state = crate::state::AppState::empty();
        let name = format!("{}.jpg", "d".repeat(64));
        let view = ImageView::new(
            Arc::new("preview".into()),
            Some(Original::Community(format!("media:{name}"))),
        );
        let id = view.id;
        let old = view.attempt;
        state.emoji_images.insert(name.clone(), String::new());
        state
            .emoji_requested
            .insert(name.clone(), std::time::Instant::now());
        state.image_viewer = Some(view);
        state.retry_image_viewer(Uuid::new_v4());
        assert_eq!(state.image_viewer.as_ref().unwrap().attempt, old);
        state.retry_image_viewer(id);
        assert_ne!(state.image_viewer.as_ref().unwrap().attempt, old);
        assert!(!state.emoji_images.contains_key(&name));
        assert!(!state.emoji_requested.contains_key(&name));
        state.image_viewer = None;
        state.retry_image_viewer(id);
        assert!(state.image_viewer.is_none());
    }
}

#[component]
pub fn ImageViewer() -> Element {
    let state = use_app_state();
    let image = use_memo(move || {
        state
            .read()
            .image_viewer
            .as_ref()
            .map(|view| (view.id, crate::image_assets::chat_source(view.data.clone())))
    });
    rsx! {
        for (id,src) in image() {
            ZoomViewer { key: "{id}", id, src }
        }
    }
}

#[component]
fn ZoomViewer(id: Uuid, src: crate::image_assets::ImageSource) -> Element {
    let mut state = use_app_state();

    let mut started = use_signal(|| None::<Uuid>);
    use_effect(move || {
        let Some(view) = state
            .read()
            .image_viewer
            .as_ref()
            .filter(|v| v.id == id && v.status == Load::Loading && v.original.is_none())
            .cloned()
        else {
            return;
        };
        let attempt = view.attempt;
        match view.target {
            Some(Original::Community(address)) => {
                let data = state.read().media_src_shared(&address);
                if let Some(data) = data
                    && let Some(view) = state.write().image_viewer.as_mut().filter(|v| v.id == id)
                {
                    view.loaded(attempt, Ok(data));
                }
            }
            Some(Original::Dm { owner, descriptor }) => {
                if *started.peek() == Some(attempt) {
                    return;
                }
                started.set(Some(attempt));
                spawn(async move {
                    let result = crate::nostr::dm_image::load_original(&owner, &descriptor)
                        .await
                        .map(Arc::new)
                        .map_err(|_| ());
                    if let Some(view) = state.write().image_viewer.as_mut().filter(|v| v.id == id) {
                        view.loaded(attempt, result);
                    }
                });
            }
            None => {}
        }
    });
    let mut timed = use_signal(|| None::<Uuid>);
    use_effect(move || {
        let Some(view) = state
            .read()
            .image_viewer
            .as_ref()
            .filter(|v| v.id == id && v.status == Load::Loading)
            .cloned()
        else {
            return;
        };
        if *timed.peek() == Some(view.attempt) {
            return;
        }
        timed.set(Some(view.attempt));
        spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            if let Some(current) = state.write().image_viewer.as_mut().filter(|v| v.id == id) {
                current.finish(view.attempt, false);
            }
        });
    });
    let candidate = use_memo(move || {
        state
            .read()
            .image_viewer
            .as_ref()
            .filter(|v| v.id == id)
            .and_then(|v| v.original.clone())
            .map(crate::image_assets::chat_source)
    });
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
                    if let Some((kind, token)) = action.split_once(':')
                        && matches!(kind, "loaded" | "error")
                        && let Ok(token) = Uuid::parse_str(token)
                        && let Some(view) =
                            state.write().image_viewer.as_mut().filter(|v| v.id == id)
                    {
                        view.finish(token, kind == "loaded");
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
    let (status, attempt, progressive) = {
        let s = state.read();
        let Some(view) = s.image_viewer.as_ref().filter(|v| v.id == id) else {
            return rsx! {};
        };
        (view.status, view.attempt, view.target.is_some())
    };
    rsx! {
        div {
            id: "dxf-image-viewer", class: "dxf-image-viewer", role: "dialog", aria_modal: "true",
            aria_label: crate::i18n::text("Image viewer", &[]), tabindex: "-1",
            "data-viewer-original":candidate().map(|s|s.to_string()),
            "data-viewer-token":"{attempt}",
            "data-viewer-progressive":progressive.then_some("true"),
            onmounted: move |_| mounted.set(true),
            div { class: "dxf-image-stage", "data-viewer-stage": "",
                img { src: "{src}", draggable: "false", alt: crate::i18n::text("attachment", &[]) }
            }

            if status!=Load::Ready {
                div {class:"dxf-image-status",role:"status",aria_live:"polite",
                    if status==Load::Loading { {crate::i18n::text("Loading image…",&[])} }
                    else {
                        {crate::i18n::text("Could not load image.",&[])}
                        button {r#type:"button",onclick:move |_|state.write().retry_image_viewer(id), {crate::i18n::text("Retry",&[])} }
                    }
                }
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
