use dioxus::prelude::*;

#[derive(Clone, Copy, PartialEq)]
struct Color {
    hue: f64,
    saturation: f64,
    value: f64,
}

impl Color {
    fn parse(hex: &str) -> Option<Self> {
        let hex = hex.strip_prefix('#')?;
        if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let number = u32::from_str_radix(hex, 16).ok()?;
        Some(Self::rgb([
            (number >> 16) as u8,
            (number >> 8) as u8,
            number as u8,
        ]))
    }

    fn rgb(rgb: [u8; 3]) -> Self {
        let [r, g, b] = rgb.map(|v| f64::from(v) / 255.0);
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let delta = max - min;
        let hue = if delta == 0.0 {
            0.0
        } else if max == r {
            60.0 * ((g - b) / delta).rem_euclid(6.0)
        } else if max == g {
            60.0 * ((b - r) / delta + 2.0)
        } else {
            60.0 * ((r - g) / delta + 4.0)
        };
        Self {
            hue,
            saturation: if max == 0.0 { 0.0 } else { delta / max },
            value: max,
        }
    }

    fn channels(self) -> [u8; 3] {
        let chroma = self.value * self.saturation;
        let x = chroma * (1.0 - ((self.hue / 60.0).rem_euclid(2.0) - 1.0).abs());
        let rgb = match (self.hue.rem_euclid(360.0) / 60.0) as u8 {
            0 => [chroma, x, 0.0],
            1 => [x, chroma, 0.0],
            2 => [0.0, chroma, x],
            3 => [0.0, x, chroma],
            4 => [x, 0.0, chroma],
            _ => [chroma, 0.0, x],
        };
        rgb.map(|v| {
            ((v + self.value - chroma) * 255.0)
                .round()
                .clamp(0.0, 255.0) as u8
        })
    }

    fn hex(self) -> String {
        let [r, g, b] = self.channels();
        format!("#{r:02x}{g:02x}{b:02x}")
    }
}

#[component]
pub(super) fn GuildAccentDialog(
    guild_id: crate::protocol::Id,
    on_close: EventHandler<()>,
) -> Element {
    let state = crate::state::use_app_state();
    let gateway = crate::state::use_gateway();
    let initial = state
        .read()
        .guilds
        .iter()
        .find(|g| g.id == guild_id)
        .and_then(|g| g.accent.clone());
    rsx! { Palette {
        initial,
        on_cancel: move |_| on_close.call(()),
        on_apply: move |accent| {
            let app = state.read();
            let current = app.guilds.iter().find(|g| g.id == guild_id);
            let changed = current.is_some_and(|g| g.accent != accent)
                && app.can(guild_id, crate::protocol::Permission::ManageGuild);
            drop(app);
            if changed {
                gateway.send(crate::protocol::ClientMessage::SetGuildAccent { guild_id, accent });
            }
            on_close.call(());
        },
    } }
}

#[component]
fn Palette(
    initial: Option<String>,
    on_apply: EventHandler<Option<String>>,
    on_cancel: EventHandler<()>,
) -> Element {
    let mut color = use_signal(|| {
        Color::parse(initial.as_deref().unwrap_or("#e0a06a")).unwrap_or(Color::rgb([224, 160, 106]))
    });
    let mut hex = use_signal(|| color.peek().hex());
    let mut draft = use_signal(|| initial.clone());
    let mut dragging = use_signal(|| false);
    let mut size = use_signal(|| (256.0, 160.0));
    let mut finished = use_signal(|| false);
    let mut select = move |next: Color| {
        color.set(next);
        hex.set(next.hex());
        draft.set(Some(next.hex()));
    };
    let mut cancel = move || {
        if !finished() {
            finished.set(true);
            on_cancel.call(());
        }
    };
    crate::mobile_back::use_handler(crate::mobile_back::CONTENT, true, move |_| cancel());
    let valid = draft().is_none() || Color::parse(&hex()).is_some();
    let current = color();
    let channels = current.channels();
    let preview = draft().unwrap_or_else(|| "var(--accent)".into());
    let caption = crate::i18n::text("Guild accent", &[]);
    rsx! {
        div {
            class: "dxf-accent-backdrop",
            onclick: move |_| cancel(),
            onkeydown: move |e| {
                if e.key() == Key::Escape { e.stop_propagation(); cancel(); }
                if e.key() == Key::Tab {
                    e.prevent_default(); e.stop_propagation();
                    let step = if e.modifiers().shift() { -1 } else { 1 };
                    let _ = document::eval(&format!("const p=document.getElementById('guild-accent-palette');const a=[...p.querySelectorAll('button:not(:disabled),input:not(:disabled)')];const i=a.indexOf(document.activeElement);a[i<0?({step}<0?a.length-1:0):(i+{step}+a.length)%a.length]?.focus();"));
                }
            },
            section {
                id: "guild-accent-palette", class: "dxf-accent-palette", role: "dialog",
                aria_modal: "true", aria_label: "{caption}", tabindex: "-1",
                onmounted: move |e| async move { let _ = e.set_focus(true).await; },
                onclick: move |e| e.stop_propagation(),
                h3 { "{caption}" }
                div {
                    class: "dxf-accent-field", role: "img", aria_label: crate::i18n::text("Color", &[]),
                    style: "background-color: hsl({current.hue} 100% 50%);",
                    onmounted: move |e| async move { if let Ok(rect) = e.get_client_rect().await { size.set((rect.size.width, rect.size.height)); } },
                    onresize: move |e| { if let Ok(rect) = e.get_content_box_size() { size.set((rect.width, rect.height)); } },
                    onpointerdown: move |e| {
                        e.prevent_default(); dragging.set(true);
                        let at = e.element_coordinates(); let (w, h) = size();
                        select(Color { saturation: (at.x / w.max(1.0)).clamp(0.0, 1.0), value: (1.0 - at.y / h.max(1.0)).clamp(0.0, 1.0), ..color() });
                    },
                    onpointermove: move |e| { if dragging() {
                        let at = e.element_coordinates(); let (w, h) = size();
                        select(Color { saturation: (at.x / w.max(1.0)).clamp(0.0, 1.0), value: (1.0 - at.y / h.max(1.0)).clamp(0.0, 1.0), ..color() });
                    } },
                    onpointerup: move |_| dragging.set(false),
                    onpointercancel: move |_| dragging.set(false),
                    onpointerleave: move |_| dragging.set(false),
                    span { class: "dxf-accent-cursor", style: "left: {current.saturation * 100.0}%; top: {(1.0 - current.value) * 100.0}%;" }
                }
                label { class: "dxf-accent-hue-label",
                    span { {crate::i18n::text("Hue", &[])} }
                    input { class: "dxf-accent-hue", r#type: "range", min: "0", max: "359", value: "{current.hue}",
                        oninput: move |e| { if let Ok(hue) = e.value().parse::<f64>() { select(Color { hue: hue.clamp(0.0, 359.0), ..color() }); } },
                    }
                }
                div { class: "dxf-accent-rgb",
                    for (index, name) in ["R", "G", "B"].into_iter().enumerate() {
                        label { "{name}",
                            input { r#type: "number", min: "0", max: "255", value: "{channels[index]}",
                                oninput: move |e| { if let Ok(value) = e.value().parse::<u8>() { let mut rgb = color().channels(); rgb[index] = value; select(Color::rgb(rgb)); } },
                            }
                        }
                    }
                }
                label { class: "dxf-accent-hex", "HEX",
                    input { r#type: "text", value: "{hex}", maxlength: "7", spellcheck: "false", aria_invalid: (!valid).to_string(), aria_label: crate::i18n::text("Hex color", &[]),
                        oninput: move |e| {
                            let value = e.value();
                            if let Some(next) = Color::parse(&value) {
                                color.set(next);
                                draft.set(Some(next.hex()));
                            } else {
                                draft.set(Some(value.clone()));
                            }
                            hex.set(value);
                        },
                    }
                    span { class: "dxf-accent-swatch", style: "background-color: {preview};" }
                }
                button { class: "dxf-accent-reset", r#type: "button", aria_label: crate::i18n::text("Theme default", &[]), aria_pressed: draft().is_none().to_string(), onclick: move |_| draft.set(None),
                    {crate::i18n::text("Theme default", &[])}
                    if draft().is_none() { " ✓" }
                }
                footer {
                    button { r#type: "button", aria_label: crate::i18n::text("Cancel", &[]), onclick: move |_| cancel(), {crate::i18n::text("Cancel", &[])} }
                    button { class: "dxf-accent-apply", r#type: "button", disabled: !valid || draft() == initial,
                        aria_label: crate::i18n::text("Apply", &[]),
                        onclick: move |_| {
                            let value = draft();
                            if !finished() && valid && value != initial {
                                finished.set(true); on_apply.call(value);
                            }
                        },
                        {crate::i18n::text("Apply", &[])}
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dioxus::core::{AttributeValue, ElementId, Mutation};
    use std::{any::Any, cell::RefCell, rc::Rc};

    type Applied = Rc<RefCell<Vec<Option<String>>>>;

    fn fixture_with(initial: Option<String>) -> (VirtualDom, Vec<Mutation>, Applied) {
        dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
        let applied: Applied = Rc::new(RefCell::new(Vec::new()));
        let mut dom = VirtualDom::new_with_props(
            |(initial, applied): (Option<String>, Applied)| {
                rsx! {
                    Palette { initial, on_apply: move |value| applied.borrow_mut().push(value), on_cancel: |_| {} }
                }
            },
            (initial, applied.clone()),
        );
        let edits = dom.rebuild_to_vec().edits;
        (dom, edits, applied)
    }

    fn fixture() -> (VirtualDom, Vec<Mutation>, Applied) {
        fixture_with(Some("#112233".to_owned()))
    }

    fn labeled(edits: &[Mutation], label: &str) -> ElementId {
        let translated = crate::i18n::text(label, &[]);
        edits
            .iter()
            .find_map(|edit| match edit {
                Mutation::SetAttribute {
                    name: "aria-label",
                    value: AttributeValue::Text(value),
                    id,
                    ..
                } if *value == translated => Some(*id),
                _ => None,
            })
            .unwrap()
    }

    fn input(dom: &mut VirtualDom, id: ElementId, value: &str) {
        let data: dioxus::html::SerializedFormData =
            serde_json::from_value(serde_json::json!({ "value": value })).unwrap();
        let data: Rc<dyn Any> = Rc::new(dioxus::html::PlatformEventData::new(Box::new(data)));
        dom.runtime()
            .handle_event("input", Event::new(data, true), id);
        dom.render_immediate_to_vec();
    }

    fn click(dom: &mut VirtualDom, id: ElementId) {
        let data: Rc<dyn Any> = Rc::new(dioxus::html::PlatformEventData::new(Box::new(
            dioxus::html::SerializedMouseData::default(),
        )));
        dom.runtime()
            .handle_event("click", Event::new(data, true), id);
    }

    #[test]
    fn editing_the_palette_does_not_try_to_update_document_styles() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        use tracing_subscriber::layer::SubscriberExt;
        struct Count(Arc<AtomicUsize>);
        impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Count {
            fn on_event(
                &self,
                event: &tracing::Event<'_>,
                _: tracing_subscriber::layer::Context<'_, S>,
            ) {
                if event.metadata().target().starts_with("dioxus_document")
                    && *event.metadata().level() == tracing::Level::WARN
                {
                    self.0.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        let warnings = Arc::new(AtomicUsize::new(0));
        let subscriber = tracing_subscriber::registry().with(Count(warnings.clone()));
        tracing::subscriber::with_default(subscriber, || {
            let (mut dom, edits, _) = fixture();
            let hex = labeled(&edits, "Hex color");
            for value in ["#223344", "#334455", "#aabbcc"] {
                input(&mut dom, hex, value);
            }
        });
        assert_eq!(warnings.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn palette_inputs_stage_locally_and_apply_emits_the_last_color_once() {
        let (mut dom, edits, applied) = fixture();
        let hex = labeled(&edits, "Hex color");
        let apply = labeled(&edits, "Apply");
        for value in ["#223344", "#334455", "#aabbcc"] {
            input(&mut dom, hex, value);
        }
        assert!(applied.borrow().is_empty());
        click(&mut dom, apply);
        click(&mut dom, apply);
        assert_eq!(&*applied.borrow(), &[Some("#aabbcc".into())]);
    }

    #[test]
    fn cancellation_blocks_any_later_apply_and_invalid_hex_is_never_applied() {
        let (mut dom, edits, applied) = fixture();
        input(&mut dom, labeled(&edits, "Hex color"), "#bad");
        click(&mut dom, labeled(&edits, "Apply"));
        assert!(applied.borrow().is_empty());
        input(&mut dom, labeled(&edits, "Hex color"), "#445566");
        click(&mut dom, labeled(&edits, "Cancel"));
        click(&mut dom, labeled(&edits, "Apply"));
        assert!(applied.borrow().is_empty());
    }

    #[test]
    fn clearing_the_accent_is_also_staged_until_apply() {
        let (mut dom, edits, applied) = fixture();
        click(&mut dom, labeled(&edits, "Theme default"));
        dom.render_immediate_to_vec();
        assert!(applied.borrow().is_empty());
        click(&mut dom, labeled(&edits, "Apply"));
        assert_eq!(&*applied.borrow(), &[None]);
    }

    #[test]
    fn rgb_hex_round_trips_and_invalid_values_are_refused() {
        for r in [0, 51, 128, 224, 255] {
            for g in [0, 40, 160, 255] {
                for b in [0, 106, 200, 255] {
                    let color = Color::rgb([r, g, b]);
                    assert_eq!(color.channels(), [r, g, b]);
                    assert_eq!(Color::parse(&color.hex()).unwrap().channels(), [r, g, b]);
                }
            }
        }
        for invalid in [
            "",
            "#fff",
            "#zzzzzz",
            "#fffffff",
            "#+00123",
            "#é1234",
            "#000000; color:red",
        ] {
            assert!(Color::parse(invalid).is_none());
        }
    }

    #[test]
    fn theme_default_accent_palette_disables_apply_until_edited() {
        let (mut dom, edits, applied) = fixture_with(None);
        let apply = labeled(&edits, "Apply");
        let reset = labeled(&edits, "Theme default");
        let hex = labeled(&edits, "Hex color");
        let pressed = edits.iter().any(|edit| {
            matches!(
                edit,
                Mutation::SetAttribute {
                    name: "aria-pressed",
                    value: AttributeValue::Text(val),
                    id,
                    ..
                } if *id == reset && *val == "true"
            )
        });
        assert!(pressed);
        click(&mut dom, apply);
        assert!(applied.borrow().is_empty());
        input(&mut dom, hex, "#bad");
        click(&mut dom, apply);
        assert!(applied.borrow().is_empty());
        input(&mut dom, hex, "#224466");
        click(&mut dom, apply);
        assert_eq!(&*applied.borrow(), &[Some("#224466".into())]);
    }
}
