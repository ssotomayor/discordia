use dioxus::prelude::*;
use serde_json::Value;

const OUT_LONG_EDGE: u32 = 1024;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CropShape {
    Square,
    Banner,
}

impl CropShape {
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

    fn mime(self) -> &'static str {
        match self {
            CropShape::Square => "image/png",
            CropShape::Banner => "image/jpeg",
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
    let mut natural = use_signal(|| None::<(f64, f64)>);
    let mut zoom = use_signal(|| 1.0_f64);
    let mut dx = use_signal(|| 0.0_f64);
    let mut dy = use_signal(|| 0.0_f64);
    let mut pan = use_signal(|| None::<Pan>);
    let mut working = use_signal(|| false);

    let vp_w = shape.preview_w();
    let vp_h = vp_w / shape.aspect();

    {
        let src = src.clone();
        use_future(move || {
            let src = src.clone();
            async move {
                let js = format!(
                    "(() => {{ const i = new Image(); \
                       i.onload = () => dioxus.send({{ w: i.naturalWidth, h: i.naturalHeight }}); \
                       i.onerror = () => dioxus.send({{ w: 0, h: 0 }}); \
                       i.src = {}; }})()",
                    crate::features::screenshare::js_str(&src)
                );
                let mut eval = document::eval(&js);
                if let Ok(v) = eval.recv::<Value>().await {
                    let w = v.get("w").and_then(|x| x.as_f64()).unwrap_or(0.0);
                    let h = v.get("h").and_then(|x| x.as_f64()).unwrap_or(0.0);
                    if w > 0.0 && h > 0.0 {
                        natural.set(Some((w, h)));
                        zoom.set((vp_w / w).max(vp_h / h));
                    }
                }
            }
        });
    }

    let Some((nat_w, nat_h)) = natural() else {
        return rsx! {
            div { class: "dxf-backdrop-in fixed inset-0 z-[60] flex items-center justify-center bg-black/60",
                div { class: "text-xs text-[var(--text-dim)]", "Loading image…" }
            }
        };
    };

    let min_zoom = (vp_w / nat_w).max(vp_h / nat_h);
    let max_zoom = min_zoom * 5.0;
    let z = zoom().clamp(min_zoom, max_zoom);

    let slack_x = ((nat_w * z) - vp_w).max(0.0) / 2.0;
    let slack_y = ((nat_h * z) - vp_h).max(0.0) / 2.0;
    let cur_dx = dx().clamp(-slack_x, slack_x);
    let cur_dy = dy().clamp(-slack_y, slack_y);

    let src_for_apply = src.clone();
    let apply = move |_| {
        if working() {
            return;
        }
        working.set(true);
        let (out_w, out_h) = shape.output();
        let mime = crate::features::screenshare::js_str(shape.mime());
        let sw = vp_w / z;
        let sh = vp_h / z;
        let sx = (nat_w / 2.0) - (cur_dx / z) - sw / 2.0;
        let sy = (nat_h / 2.0) - (cur_dy / z) - sh / 2.0;
        let js = format!(
            "(() => {{ const i = new Image(); \
               i.onload = () => {{ try {{ \
                 const c = document.createElement('canvas'); \
                 c.width = {out_w}; c.height = {out_h}; \
                 const x = c.getContext('2d'); \
                 x.imageSmoothingQuality = 'high'; \
                 if ({mime} === 'image/jpeg') {{ x.fillStyle = '#ffffff'; x.fillRect(0,0,{out_w},{out_h}); }} \
                 x.drawImage(i, {sx}, {sy}, {sw}, {sh}, 0, 0, {out_w}, {out_h}); \
                 dioxus.send(c.toDataURL({mime}, 0.92)); \
               }} catch (e) {{ dioxus.send(''); }} }}; \
               i.onerror = () => dioxus.send(''); \
               i.src = {}; }})()",
            crate::features::screenshare::js_str(&src_for_apply)
        );
        let fallback = src_for_apply.clone();
        spawn(async move {
            let mut eval = document::eval(&js);
            match eval.recv::<String>().await {
                Ok(url) if !url.is_empty() => on_apply.call(url),
                _ => on_apply.call(fallback),
            }
        });
    };

    let round = if shape == CropShape::Square {
        "rounded-full"
    } else {
        "rounded-md"
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
                    "Position your image"
                }

                div {
                    class: "relative overflow-hidden border border-[var(--border)] mx-auto {round}",
                    style: "width: {vp_w}px; height: {vp_h}px; background: var(--bg2); cursor: grab; touch-action: none;",
                    onmousedown: move |e| {
                        let c = e.client_coordinates();
                        pan.set(Some(Pan { from_x: c.x, from_y: c.y, base_dx: cur_dx, base_dy: cur_dy }));
                    },
                    img {
                        src: "{src}",
                        draggable: false,
                        style: "position:absolute; left:50%; top:50%; max-width:none; \
                                width:{nat_w}px; height:{nat_h}px; \
                                transform: translate(-50%,-50%) translate({cur_dx}px,{cur_dy}px) scale({z}); \
                                transform-origin: center; user-select:none; pointer-events:none;",
                    }
                }

                if pan().is_some() {
                    div {
                        class: "fixed inset-0 z-50",
                        style: "cursor: grabbing;",
                        onmousemove: move |e| {
                            if let Some(p) = pan() {
                                let c = e.client_coordinates();
                                dx.set(p.base_dx + (c.x - p.from_x));
                                dy.set(p.base_dy + (c.y - p.from_y));
                            }
                        },
                        onmouseup: move |_| pan.set(None),
                    }
                }

                div { class: "flex items-center gap-2 mt-3",
                    span { class: "text-[10px] text-[var(--text-dim)]", "Zoom" }
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
                    "Drag the image to reposition it."
                }

                div { class: "flex gap-2 justify-end mt-3",
                    button {
                        class: "rounded px-3 py-1 text-[10px] uppercase tracking-wider text-[var(--text-muted)] border border-[var(--border)] hover:text-[var(--text)] transition-colors",
                        onclick: move |_| on_cancel.call(()),
                        "Cancel"
                    }
                    button {
                        class: "dxf-cta rounded px-3 py-1 text-[10px] uppercase tracking-wider disabled:opacity-50",
                        disabled: working(),
                        onclick: apply,
                        if working() { "Working…" } else { "Use image" }
                    }
                }
            }
        }
    }
}
