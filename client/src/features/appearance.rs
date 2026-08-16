//! Appearance settings UI — theme picker + local background image. Reads and
//! mutates the `Signal<ClientSettings>` provided by `App`, persisting every
//! change to disk. All of this is local to the client; nothing is sent to a
//! host.

use base64::Engine as _;
use dioxus::prelude::*;

use crate::app::THEMES;
use crate::settings::{self, ClientSettings};

/// Backgrounds are local-only but still kept reasonable in size.
const MAX_BACKGROUND_BYTES: usize = 4_000_000;

/// `(id, label, preview-inline-style)` for the procedural background tiles.
/// Previews are miniatures of the real `.app-bg-*` rules from `app.rs`.
const BACKGROUND_TILES: &[(&str, &str, &str)] = &[
    (
        "grid",
        "Grid",
        "background-color:#0e0b08;background-image:linear-gradient(var(--edge) 1px,transparent 1px),linear-gradient(90deg,var(--edge) 1px,transparent 1px);background-size:12px 12px;",
    ),
    (
        "dots",
        "Dots",
        "background-color:#0e0b08;background-image:radial-gradient(var(--edge) 1.4px,transparent 1.4px);background-size:10px 10px;",
    ),
    (
        "aurora",
        "Aurora",
        "background:radial-gradient(circle at 25% 30%,color-mix(in srgb,var(--accent) 30%,transparent),transparent 55%),radial-gradient(circle at 75% 70%,color-mix(in srgb,var(--violet) 24%,transparent),transparent 55%),#0e0b08;",
    ),
    (
        "mesh",
        "Mesh",
        "background:#0e0b08,radial-gradient(circle at 20% 20%,var(--accent-soft),transparent 45%),radial-gradient(circle at 85% 80%,color-mix(in srgb,var(--violet) 14%,transparent),transparent 45%);",
    ),
    (
        "sunset",
        "Sunset",
        "background:linear-gradient(160deg,color-mix(in srgb,var(--accent) 16%,#0e0b08),#0e0b08 60%),radial-gradient(circle at 70% 15%,color-mix(in srgb,var(--accent) 32%,transparent),transparent 45%);",
    ),
    ("none", "None", "background:#0e0b08;"),
];

/// A small "theme" button that opens the appearance modal.
#[component]
pub fn AppearanceButton() -> Element {
    let mut settings = use_context::<Signal<ClientSettings>>();
    let mut open = use_signal(|| false);
    let mut err = use_signal::<Option<String>>(|| None);

    // Persist + apply a settings mutation in one place.
    let mut update = move |f: &dyn Fn(&mut ClientSettings)| {
        let mut next = settings.read().clone();
        f(&mut next);
        settings.set(next.clone());
        settings::save(&next);
    };

    let current = settings.read().clone();

    // `relative`, so the panel below can be positioned against this button
    // rather than against the viewport. Everything the popover needs is inside
    // this component, which is why anchoring it costs no plumbing: the button
    // and the panel have always been siblings here.
    rsx! {
        div { class: "relative",
            button {
                class: "w-7 h-7 flex items-center justify-center rounded text-[var(--text-muted)] hover:text-[var(--accent)] transition-colors",
                title: "Appearance",
                onclick: move |_| { err.set(None); open.set(true); },
                dangerous_inner_html: crate::features::icons::SLIDERS,
            }

            if open() {
                // Click-catcher only. It used to carry `bg-black/50`, which is
                // what made this read as a modal: dimming the app says "answer
                // me before doing anything else", and this panel is a set of
                // preferences you want to see applied to what is behind it.
                div {
                    class: "fixed inset-0 z-40",
                    onclick: move |_| open.set(false),
                }
                div {
                    // Anchored above the button, not below: the palette icon
                    // lives in `UserPanel`, at the *bottom* of the sidebar, so
                    // the comp's "anchored under the icon" would open
                    // off-screen here. It is anchored to the icon, above it.
                    //
                    // Deliberately phrased without naming the other class:
                    // `tailwind.css` scans `src/**/*.rs`, so a utility spelled
                    // in a comment is a rule emitted into the committed
                    // `tailwind.out.css`. Writing this one out cost a CI
                    // failure — the same trap `TODO.md` records for
                    // `Discordia.html`, whose description of a rule was keeping
                    // that rule alive.
                    class: "dxf-pop-in absolute bottom-full left-0 mb-2 z-50 w-80 bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg shadow-xl p-4",
                    onclick: move |e| e.stop_propagation(),
                    h3 { class: "text-sm font-medium text-[var(--accent)] mb-3", "Appearance" }

                    // Theme swatches — gradient tiles (comp 4).
                    div { class: "text-[10px] uppercase tracking-wider text-[var(--text-muted)] mb-1.5", "Theme" }
                    div { class: "grid grid-cols-5 gap-2 mb-4",
                        for theme in THEMES.iter() {
                            {
                                let id = theme.id;
                                let selected = current.theme == id;
                                let ring = if selected {
                                    "ring-2 ring-[var(--accent)] ring-offset-2 ring-offset-[var(--panel-solid)]"
                                } else {
                                    "border border-[var(--edge)]"
                                };
                                rsx! {
                                    button {
                                        key: "{id}",
                                        class: "flex flex-col items-center gap-1 group",
                                        title: "{theme.label}",
                                        onclick: move |_| update(&|s| s.theme = id.to_string()),
                                        span {
                                            class: "w-full h-11 rounded-lg {ring}",
                                            style: "background: linear-gradient(150deg, {theme.swatch}, transparent 78%), #12100e;",
                                        }
                                        span { class: "text-[9px] text-[var(--text-dim)] group-hover:text-[var(--text-muted)]",
                                            "{theme.label}"
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // Procedural background picker (comp 4).
                    div { class: "text-[10px] uppercase tracking-wider text-[var(--text-muted)] mb-1.5", "Background" }
                    div { class: "grid grid-cols-3 gap-2 mb-4",
                        for (id, label, preview) in BACKGROUND_TILES.iter().copied() {
                            {
                                let selected = current.pattern == id && current.background.is_none();
                                let ring = if selected {
                                    "ring-2 ring-[var(--accent)] ring-offset-2 ring-offset-[var(--panel-solid)]"
                                } else {
                                    "border border-[var(--edge)] hover:border-[var(--border-strong)]"
                                };
                                rsx! {
                                    button {
                                        key: "{id}",
                                        // Choosing a pattern clears any custom image.
                                        class: "h-14 rounded-lg flex items-center justify-center text-xs font-medium text-[var(--text)] {ring}",
                                        style: "{preview}",
                                        onclick: move |_| update(&|s| { s.pattern = id.to_string(); s.background = None; }),
                                        "{label}"
                                    }
                                }
                            }
                        }
                    }

                    // Accent color override.
                    div { class: "text-[10px] uppercase tracking-wider text-[var(--text-muted)] mb-1.5", "Accent" }
                    div { class: "flex items-center gap-2 mb-4",
                        input {
                            r#type: "color",
                            class: "w-8 h-8 rounded border border-[var(--border)] bg-transparent cursor-pointer",
                            value: "{current.accent.clone().unwrap_or_else(|| \"#e0a06a\".into())}",
                            oninput: move |e| {
                                let v = e.value();
                                update(&move |s| s.accent = Some(v.clone()));
                            },
                        }
                        span { class: "text-xs text-[var(--text-muted)] flex-1",
                            if current.accent.is_some() { "Custom accent" } else { "Theme default" }
                        }
                        if current.accent.is_some() {
                            button {
                                class: "px-2 py-1 text-[10px] uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--danger)] transition-colors",
                                onclick: move |_| update(&|s| s.accent = None),
                                "Reset"
                            }
                        }
                    }

                    // Custom background image (overrides the pattern above).
                    div { class: "text-[10px] uppercase tracking-wider text-[var(--text-muted)] mb-1.5", "Custom image" }
                    div { class: "h-20 rounded-md border border-[var(--border)] overflow-hidden bg-[var(--accent-soft)] flex items-center justify-center text-[var(--text-dim)] text-xs mb-2",
                        if let Some(url) = current.background.clone() {
                            img { class: "w-full h-full object-cover", src: "{url}", alt: "background preview" }
                        } else {
                            "no background"
                        }
                    }
                    div { class: "flex items-center gap-2 mb-3",
                        label {
                            class: "px-3 py-1 rounded text-[10px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors cursor-pointer",
                            "Choose image"
                            input {
                                r#type: "file",
                                accept: "image/*",
                                class: "hidden",
                                onchange: move |evt: FormEvent| {
                                    let files = evt.files();
                                    let mut settings = settings;
                                    let mut err = err;
                                    spawn(async move {
                                        let Some(file) = files.into_iter().next() else { return };
                                        match file.read_bytes().await {
                                            Ok(bytes) => {
                                                if bytes.len() > MAX_BACKGROUND_BYTES {
                                                    err.set(Some("Image too large (max 4 MB).".into()));
                                                    return;
                                                }
                                                let mime = file
                                                    .content_type()
                                                    .filter(|m| m.starts_with("image/"))
                                                    .unwrap_or_else(|| "image/png".to_string());
                                                let b64 = base64::engine::general_purpose::STANDARD
                                                    .encode(&bytes);
                                                err.set(None);
                                                let mut next = settings.read().clone();
                                                next.background = Some(format!("data:{mime};base64,{b64}"));
                                                settings.set(next.clone());
                                                settings::save(&next);
                                            }
                                            Err(_) => err.set(Some("Couldn't read that file.".into())),
                                        }
                                    });
                                },
                            }
                        }
                        if current.background.is_some() {
                            button {
                                class: "px-3 py-1 text-[10px] uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--danger)] transition-colors",
                                onclick: move |_| update(&|s| s.background = None),
                                "Remove"
                            }
                        }
                    }

                    // Dim slider (only meaningful with a background).
                    if current.background.is_some() {
                        div { class: "flex items-center gap-2 mb-1",
                            span { class: "text-[10px] uppercase tracking-wider text-[var(--text-muted)] w-10", "Dim" }
                            input {
                                r#type: "range",
                                class: "flex-1 accent-[var(--accent)]",
                                min: "0",
                                max: "90",
                                value: "{current.background_dim}",
                                oninput: move |e| {
                                    if let Ok(v) = e.value().parse::<u8>() {
                                        update(&move |s| s.background_dim = v);
                                    }
                                },
                            }
                            span { class: "text-[10px] text-[var(--text-dim)] w-8 text-right", "{current.background_dim}%" }
                        }
                    }

                    // Blossom media server — where profile images are uploaded.
                    div { class: "mt-4 text-[10px] uppercase tracking-wider text-[var(--text-muted)] mb-1.5", "Blossom media server" }
                    input {
                        r#type: "text",
                        class: "w-full bg-transparent border border-[var(--border)] focus:border-[var(--accent)] rounded px-2 py-1.5 text-xs font-mono text-[var(--text)] outline-none transition-colors",
                        placeholder: "https://blossom.band",
                        value: "{current.blossom_server}",
                        oninput: move |e| {
                            let v = e.value();
                            update(&move |s| s.blossom_server = v.clone());
                        },
                    }
                    div { class: "mt-1 text-[10px] text-[var(--text-dim)]",
                        "Hosts your avatar/banner so they have a URL. Falls back to embedding the image if upload fails."
                    }

                    if let Some(e) = err() {
                        div { class: "mt-2 text-[10px] text-[var(--danger)]", "{e}" }
                    }

                    div { class: "mt-4 pt-3 border-t border-[var(--edge)] text-[11px] text-[var(--text-dim)] leading-relaxed",
                        "Tip: drag panel headers to move, corners to resize. Toggle Edit layout for snap presets."
                    }

                    div { class: "mt-3 flex justify-end",
                        button {
                            class: "px-3 py-1.5 rounded-lg text-xs uppercase tracking-wider text-[var(--accent)] border border-[var(--edge)] hover:border-[var(--accent)] transition-colors",
                            onclick: move |_| open.set(false),
                            "Done"
                        }
                    }
                }
            }
        }
    }
}
