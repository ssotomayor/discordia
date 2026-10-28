use dioxus::prelude::*;
use std::sync::Arc;

const OUT_LONG_EDGE: u32 = 1024;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CropShape {
    Square,
    Banner,
}

#[cfg(test)]
mod tests {
    use super::*;
    use dioxus::core::{AttributeValue, ElementId, Mutation};
    use std::{any::Any, cell::RefCell, rc::Rc};

    fn pointer(
        dom: &mut VirtualDom,
        name: &str,
        id: ElementId,
        kind: &str,
        pointer: i32,
        x: f64,
        y: f64,
    ) -> Vec<Mutation> {
        let mut value = serde_json::to_value(dioxus::html::SerializedMouseData::default()).unwrap();
        value.as_object_mut().unwrap().extend(
            serde_json::json!({
                "buttons":1,"client_x":x,"client_y":y,"pointer_id":pointer,"width":1.0,"height":1.0,
                "pressure":0.5,"tangential_pressure":0.0,"tilt_x":0,"tilt_y":0,"twist":0,
                "pointer_type":kind,"is_primary":true
            })
            .as_object()
            .unwrap()
            .clone(),
        );
        let data: dioxus::html::SerializedPointerData = serde_json::from_value(value).unwrap();
        let data: Rc<dyn Any> = Rc::new(dioxus::html::PlatformEventData::new(Box::new(data)));
        dom.runtime().handle_event(name, Event::new(data, true), id);
        dom.render_immediate_to_vec().edits
    }

    #[tokio::test]
    async fn touch_and_mouse_pan_the_crop_and_export_an_opaque_square() {
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(800, 400)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        let source = crate::features::profiles::to_data_url(bytes.get_ref(), "image/png");
        for kind in ["touch", "mouse"] {
            type Export = Rc<RefCell<Option<String>>>;
            let output: Export = Rc::new(RefCell::new(None));
            let mut dom = VirtualDom::new_with_props(
                |(source, output): (String, Export)| {
                    rsx! {
                        ImageEditor { src: source, shape: CropShape::Square,
                            on_cancel: |_| {}, on_apply: move |image| *output.borrow_mut() = Some(image) }
                    }
                },
                (source.clone(), output.clone()),
            );
            dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
            let mut edits = dom.rebuild_to_vec().edits;
            let viewport = loop {
                if let Some(id) = edits.iter().find_map(|edit| match edit {
                    Mutation::NewEventListener { name, id } if name == "pointerdown" => Some(*id),
                    _ => None,
                }) {
                    break id;
                }
                tokio::time::timeout(std::time::Duration::from_secs(5), dom.wait_for_work())
                    .await
                    .unwrap();
                edits.extend(dom.render_immediate_to_vec().edits);
            };
            pointer(&mut dom, "pointerdown", viewport, kind, 7, 100.0, 100.0);
            let moved = pointer(&mut dom, "pointermove", viewport, kind, 7, 140.0, 120.0);
            assert!(
                moved.iter().any(|edit| matches!(edit,
                    Mutation::SetAttribute { name: "style", value: AttributeValue::Text(style), .. }
                    if style.contains("translate(40px,0px)")
                )),
                "{kind} did not pan the image"
            );
            assert!(pointer(&mut dom, "pointermove", viewport, kind, 8, 180.0, 120.0).is_empty());
            pointer(&mut dom, "pointercancel", viewport, kind, 7, 140.0, 120.0);
            assert!(pointer(&mut dom, "pointermove", viewport, kind, 7, 180.0, 120.0).is_empty());
            let apply = edits
                .iter()
                .filter_map(|edit| match edit {
                    Mutation::NewEventListener { name, id } if name == "click" => Some(*id),
                    _ => None,
                })
                .next_back()
                .unwrap();
            let click: Rc<dyn Any> = Rc::new(dioxus::html::PlatformEventData::new(Box::new(
                dioxus::html::SerializedMouseData::default(),
            )));
            dom.runtime()
                .handle_event("click", Event::new(click, true), apply);
            while output.borrow().is_none() {
                tokio::time::timeout(std::time::Duration::from_secs(5), dom.wait_for_work())
                    .await
                    .unwrap();
                dom.render_immediate_to_vec();
            }
            let decoded = crate::image_edit::decode(
                output.borrow().as_ref().unwrap(),
                crate::image_edit::MAX_AVATAR_INPUT_BYTES,
            )
            .unwrap();
            assert_eq!(decoded.pixels.dimensions(), (512, 512));
            for (x, y) in [(0, 0), (511, 0), (0, 511), (511, 511)] {
                assert_eq!(decoded.pixels.get_pixel(x, y)[3], 255);
            }
        }
    }
}

impl CropShape {
    pub(super) fn input_limit(self) -> usize {
        match self {
            Self::Square => crate::image_edit::MAX_AVATAR_INPUT_BYTES,
            Self::Banner => crate::image_edit::MAX_BANNER_INPUT_BYTES,
        }
    }

    fn aspect(self) -> f64 {
        match self {
            CropShape::Square => 1.0,
            CropShape::Banner => 3.0,
        }
    }

    fn output(self) -> (u32, u32) {
        match self {
            CropShape::Square => (512, 512),
            CropShape::Banner => (OUT_LONG_EDGE, (OUT_LONG_EDGE as f64 / 3.0) as u32),
        }
    }

    fn preview_w(self) -> f64 {
        match self {
            CropShape::Square => 260.0,
            CropShape::Banner => 360.0,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
struct Pan {
    pointer: i32,
    from_x: f64,
    from_y: f64,
    base_dx: f64,
    base_dy: f64,
}

#[component]
pub fn ImageEditor(
    src: String,
    shape: CropShape,
    on_cancel: EventHandler<()>,
    on_apply: EventHandler<String>,
) -> Element {
    let mut loaded = use_signal(|| None::<Result<Arc<crate::image_edit::DecodedImage>, String>>);
    let mut error = use_signal(|| None::<String>);
    let mut zoom = use_signal(|| 1.0_f64);
    let mut dx = use_signal(|| 0.0_f64);
    let mut dy = use_signal(|| 0.0_f64);
    let mut pan = use_signal(|| None::<Pan>);
    let mut working = use_signal(|| false);
    let viewport_id = use_hook(|| format!("crop-{}", crate::protocol::Id::new_v4()));

    let vp_w = shape.preview_w();
    let vp_h = vp_w / shape.aspect();

    {
        let src = src.clone();
        use_future(move || {
            let src = src.clone();
            async move {
                let result = tokio::task::spawn_blocking(move || {
                    crate::image_edit::decode(&src, shape.input_limit())
                })
                .await
                .map_err(|e| format!("Couldn't process that image: {e}"))
                .and_then(|result| result)
                .map(Arc::new);
                if let Ok(image) = &result {
                    zoom.set(
                        (vp_w / f64::from(image.pixels.width()))
                            .max(vp_h / f64::from(image.pixels.height())),
                    );
                }
                loaded.set(Some(result));
            }
        });
    }

    let Some(result) = loaded() else {
        return rsx! {
            div { class: "dxf-backdrop-in fixed inset-0 z-[60] flex items-center justify-center bg-black/60",
                div { class: "text-xs text-[var(--text-dim)]", {crate::i18n::text("Loading image…", &[])} }
            }
        };
    };
    let image = match result {
        Ok(image) => image,
        Err(message) => {
            return rsx! {
                div { class: "dxf-backdrop-in fixed inset-0 z-[60] flex items-center justify-center bg-black/60",
                    div { class: "bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg p-4",
                        p { class: "text-xs text-[var(--text-dim)]", {crate::i18n::message(&(message).to_string())} }
                        button { class: "mt-3 text-xs text-[var(--accent)]", onclick: move |_| on_cancel.call(()), {crate::i18n::text("Cancel", &[])} }
                    }
                }
            };
        }
    };
    let nat_w = f64::from(image.pixels.width());
    let nat_h = f64::from(image.pixels.height());
    let preview = image.preview.clone();
    let animated = image.is_animated();

    let min_zoom = (vp_w / nat_w).max(vp_h / nat_h);
    let max_zoom = min_zoom * 5.0;
    let z = zoom().clamp(min_zoom, max_zoom);

    let slack_x = ((nat_w * z) - vp_w).max(0.0) / 2.0;
    let slack_y = ((nat_h * z) - vp_h).max(0.0) / 2.0;
    let cur_dx = dx().clamp(-slack_x, slack_x);
    let cur_dy = dy().clamp(-slack_y, slack_y);

    let apply = move |_| {
        if working() {
            return;
        }
        working.set(true);
        error.set(None);
        let sw = vp_w / z;
        let sh = vp_h / z;
        let sx = (nat_w / 2.0) - (cur_dx / z) - sw / 2.0;
        let sy = (nat_h / 2.0) - (cur_dy / z) - sh / 2.0;
        let image = Arc::clone(&image);
        spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                crate::image_edit::crop_image(
                    &image,
                    [sx, sy, sw, sh],
                    shape.output(),
                    shape == CropShape::Banner,
                )
            })
            .await
            .map_err(|e| format!("Couldn't process that image: {e}"))
            .and_then(|result| result);
            working.set(false);
            match result {
                Ok(url) => on_apply.call(url),
                Err(message) => error.set(Some(message)),
            }
        });
    };

    rsx! {
        // Above the profile and guild dialogs (z-50) that open it, which sit
        // later in the DOM and would otherwise cover it.
        div {
            class: "dxf-backdrop-in fixed inset-0 z-[60] flex items-center justify-center bg-black/60",
            onclick: move |_| on_cancel.call(()),
            div {
                class: "dxf-modal-in bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg shadow-xl p-4",
                onclick: move |e| e.stop_propagation(),

                div { class: "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)] mb-2",
                    {crate::i18n::text("Position your image", &[])}
                }

                div {
                    id: "{viewport_id}",
                    class: "dxf-crop-viewport relative overflow-hidden border border-[var(--border)] mx-auto rounded-md",
                    style: "width: {vp_w}px; height: {vp_h}px; background-color: var(--bg2); cursor: grab; touch-action: none;",
                    onpointerdown: {
                        let viewport_id = viewport_id.clone();
                        move |e: PointerEvent| {
                            if !e.is_primary() || !e.held_buttons().contains(dioxus::html::input_data::MouseButton::Primary) { return; }
                            e.prevent_default();
                            let c = e.client_coordinates();
                            let pointer = e.pointer_id();
                            pan.set(Some(Pan { pointer, from_x: c.x, from_y: c.y, base_dx: cur_dx, base_dy: cur_dy }));
                            let id = serde_json::to_string(&viewport_id).unwrap_or_default();
                            let _ = document::eval(&format!("document.getElementById({id})?.setPointerCapture({pointer});"));
                        }
                    },
                    onpointermove: move |e: PointerEvent| {
                        if let Some(p) = pan() && p.pointer == e.pointer_id() {
                            e.prevent_default();
                            let c = e.client_coordinates();
                            dx.set((p.base_dx + c.x - p.from_x).clamp(-slack_x, slack_x));
                            dy.set((p.base_dy + c.y - p.from_y).clamp(-slack_y, slack_y));
                        }
                    },
                    onpointerup: move |e: PointerEvent| {
                        if pan().is_some_and(|p| p.pointer == e.pointer_id()) { pan.set(None); }
                    },
                    onpointercancel: move |_| pan.set(None),
                    onlostpointercapture: move |_| pan.set(None),
                    img {
                        src: "{preview}",
                        draggable: false,
                        style: "position:absolute; left:50%; top:50%; max-width:none; \
                                width:{nat_w}px; height:{nat_h}px; \
                                transform: translate(-50%,-50%) translate({cur_dx}px,{cur_dy}px) scale({z}); \
                                transform-origin: center; user-select:none; pointer-events:none;",
                    }
                }

                div { class: "flex items-center gap-2 mt-3",
                    span { class: "text-[10px] text-[var(--text-dim)]", {crate::i18n::text("Zoom", &[])} }
                    input {
                        r#type: "range",
                        min: "0",
                        max: "100",
                        value: "{((z - min_zoom) / (max_zoom - min_zoom) * 100.0) as u32}",
                        class: "flex-1 accent-[var(--accent)]",
                        oninput: move |e| {
                            let pct: f64 = e.value().parse().unwrap_or(0.0);
                            zoom.set(min_zoom + (max_zoom - min_zoom) * (pct / 100.0));
                        },
                    }
                }
                div { class: "text-[10px] text-[var(--text-dim)] mt-1",
                    {crate::i18n::text("Drag the image to reposition it.", &[])}
                }
                if animated {
                    div { class: "text-[10px] text-[var(--text-dim)] mt-1",
                        {crate::i18n::text("GIFs keep their animation; resolution adjusts to fit the 2 MiB limit.", &[])}
                    }
                }
                if let Some(message) = error() {
                    p { class: "text-xs text-[var(--text-dim)] mt-1", {crate::i18n::message(&(message).to_string())} }
                }

                div { class: "flex gap-2 justify-end mt-3",
                    button {
                        class: "rounded px-3 py-1 text-[10px] uppercase tracking-wider text-[var(--text-muted)] border border-[var(--border)] hover:text-[var(--text)] transition-colors",
                        onclick: move |_| on_cancel.call(()),
                        {crate::i18n::text("Cancel", &[])}
                    }
                    button {
                        class: "dxf-cta rounded px-3 py-1 text-[10px] uppercase tracking-wider disabled:opacity-50",
                        disabled: working(),
                        onclick: apply,
                        if working() { {crate::i18n::text("Working…", &[])} } else { {crate::i18n::text("Use image", &[])} }
                    }
                }
            }
        }
    }
}
