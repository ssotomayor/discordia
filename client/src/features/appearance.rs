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

    rsx! {
        button {
            class: "w-7 h-7 flex items-center justify-center rounded text-[var(--text-muted)] hover:text-[var(--accent)] transition-colors",
            title: "Appearance",
            onclick: move |_| { err.set(None); open.set(true); },
            dangerous_inner_html: crate::features::icons::SLIDERS,
        }

        if open() {
            div {
                class: "dxf-backdrop-in fixed inset-0 z-50 flex items-center justify-center bg-black/50",
                onclick: move |_| open.set(false),
                div {
                    class: "dxf-modal-in w-80 bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg shadow-xl p-4",
                    onclick: move |e| e.stop_propagation(),
                    h3 { class: "text-sm font-medium text-[var(--accent)] mb-3", "Appearance" }

                    // Theme swatches.
                    div { class: "text-[10px] uppercase tracking-wider text-[var(--text-muted)] mb-1.5", "Theme" }
                    div { class: "grid grid-cols-5 gap-2 mb-4",
                        for theme in THEMES.iter() {
                            {
                                let id = theme.id;
                                let selected = current.theme == id;
                                let ring = if selected { "ring-2 ring-[var(--accent)]" } else { "" };
                                rsx! {
                                    button {
                                        key: "{id}",
                                        class: "flex flex-col items-center gap-1 group",
                                        title: "{theme.label}",
                                        onclick: move |_| update(&|s| s.theme = id.to_string()),
                                        span {
                                            class: "w-9 h-9 rounded-md border border-[var(--border)] {ring}",
                                            style: "background-color: {theme.swatch};",
                                        }
                                        span { class: "text-[9px] text-[var(--text-dim)] group-hover:text-[var(--text-muted)]",
                                            "{theme.label}"
                                        }
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

                    // Background image.
                    div { class: "text-[10px] uppercase tracking-wider text-[var(--text-muted)] mb-1.5", "Background" }
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

                    div { class: "mt-3 flex justify-end",
                        button {
                            class: "px-3 py-1.5 rounded text-xs uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors",
                            onclick: move |_| open.set(false),
                            "Done"
                        }
                    }
                }
            }
        }
    }
}
