use dioxus::prelude::*;

use crate::giphy::Gif;

/// Every keystroke would spend the build's shared hourly GIPHY quota.
const DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(450);

#[derive(Clone, PartialEq)]
enum Results {
    Loading,
    Ready(Vec<Gif>),
    Failed(String),
}

#[component]
pub(super) fn GifPicker(
    mut open: Signal<bool>,
    on_open: EventHandler<()>,
    on_pick: EventHandler<String>,
) -> Element {
    let mut query = use_signal(String::new);
    let mut results = use_signal(|| Results::Loading);
    let mut generation = use_signal(|| 0u64);

    use_effect(move || {
        if !open() {
            return;
        }
        let text = query();
        let mine = generation.peek().wrapping_add(1);
        generation.set(mine);
        spawn(async move {
            if !text.trim().is_empty() {
                tokio::time::sleep(DEBOUNCE).await;
            }
            if *generation.peek() != mine {
                return;
            }
            results.set(Results::Loading);
            let found = crate::giphy::search(&text).await;
            if *generation.peek() == mine {
                results.set(match found {
                    Ok(gifs) => Results::Ready(gifs),
                    Err(why) => Results::Failed(why),
                });
            }
        });
    });

    rsx! {
        button {
            r#type: "button",
            class: if open() {
                "w-8 h-8 shrink-0 flex items-center justify-center rounded-lg border border-[var(--accent)] bg-[var(--panel2)] text-[var(--accent)] transition-colors"
            } else {
                "w-8 h-8 shrink-0 flex items-center justify-center rounded-lg border border-[var(--border)] bg-[var(--panel2)] text-[var(--text-muted)] hover:text-[var(--accent)] hover:border-[var(--accent)] transition-colors"
            },
            title: crate::i18n::text("GIFs", &[]),
            aria_label: crate::i18n::text("GIFs", &[]),
            aria_pressed: open(),
            onmousedown: move |e| e.prevent_default(),
            onclick: move |_| {
                open.toggle();
                if open() {
                    on_open.call(());
                }
            },
            span { class: "block w-4 h-4", dangerous_inner_html: super::icons::GIF }
        }
        if open() {
            div { class: "fixed inset-0 z-20", onclick: move |_| open.set(false) }
            div { class: "dxf-gif-popover dxf-pop-in absolute bottom-full right-3 mb-2 z-30 flex flex-col bg-[var(--panel-solid)] border border-[var(--border)] rounded-xl shadow-lg text-xs text-[var(--text)]",
                input {
                    class: "m-2 shrink-0 bg-transparent border border-[var(--border)] rounded px-2 py-1 text-xs text-[var(--text)] focus:outline-none focus:border-[var(--accent)]",
                    aria_label: crate::i18n::text("Search GIPHY…", &[]),
                    placeholder: crate::i18n::text("Search GIPHY…", &[]),
                    value: "{query}",
                    onmounted: move |e| async move {
                        if !cfg!(target_os = "android") {
                            let _ = e.set_focus(true).await;
                        }
                    },
                    oninput: move |e| query.set(e.value()),
                    onkeydown: move |e: KeyboardEvent| {
                        if e.key() == Key::Escape {
                            e.prevent_default();
                            e.stop_propagation();
                            open.set(false);
                        }
                    },
                }
                div { class: "dxf-gif-scroll px-2",
                    match results() {
                        Results::Loading => rsx! {
                            p { class: "py-6 text-center text-[var(--text-dim)]", {crate::i18n::text("Loading…", &[])} }
                        },
                        Results::Failed(why) => rsx! {
                            p { class: "py-6 text-center text-[var(--danger)]", {crate::i18n::message(&why)} }
                        },
                        Results::Ready(gifs) if gifs.is_empty() => rsx! {
                            p { class: "py-6 text-center text-[var(--text-dim)]", {crate::i18n::text("No GIFs found.", &[])} }
                        },
                        Results::Ready(gifs) => rsx! {
                            div { class: "dxf-gif-grid",
                                for gif in gifs {
                                    button {
                                        key: "{gif.id}",
                                        r#type: "button",
                                        class: "dxf-gif-cell rounded overflow-hidden border border-transparent hover:border-[var(--accent)]",
                                        style: "aspect-ratio: {gif.width} / {gif.height};",
                                        title: "{gif.title}",
                                        onclick: {
                                            let id = gif.id.clone();
                                            move |_| {
                                                open.set(false);
                                                on_pick.call(crate::giphy::link(&id));
                                            }
                                        },
                                        img { src: "{gif.preview}", alt: "{gif.title}", loading: "lazy", referrerpolicy: "no-referrer" }
                                    }
                                }
                            }
                        },
                    }
                }
                div { class: "h-7 shrink-0 px-2 flex items-center justify-end border-t border-[var(--border)] text-[10px] text-[var(--text-dim)]",
                    {crate::i18n::text("Powered by GIPHY", &[])}
                }
            }
        }
    }
}

/// A message that is one GIPHY link, drawn as the GIF; the link stays if it will not load.
#[component]
pub(super) fn GifEmbed(id: String, link: Element) -> Element {
    let mut failed = use_signal(|| false);
    if failed() {
        return link;
    }
    rsx! {
        img {
            class: "dxf-gif-embed rounded-md",
            src: crate::giphy::embed_url(&id),
            alt: "GIF",
            title: "GIF via GIPHY",
            loading: "lazy",
            referrerpolicy: "no-referrer",
            onerror: move |_| failed.set(true),
        }
    }
}
