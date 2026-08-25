use dioxus::prelude::*;
use dioxus_grid_layout::{GridItem, GridLayout, GridPosition, NoDrag, use_layout_store};

fn main() {
    dioxus::launch(app);
}

const STYLE: &str = "
    :root {
        --bg: #0a0908;
        --panel: #0a0908;
        --border: rgba(190, 130, 90, 0.18);
        --border-strong: rgba(190, 130, 90, 0.35);
        --text: #d6d6d6;
        --text-muted: #888888;
        --text-dim: #5a5a5a;
        --accent: #e0a06a;
        --accent-soft: rgba(224, 160, 106, 0.10);
        --accent-strong: #ec8f3f;
    }
    html, body { margin: 0; height: 100%; background: var(--bg); color: var(--text);
        font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; }
    .dioxus-grid-layout { padding: 16px; }
    .widget {
        background: var(--panel);
        border: 1px solid var(--border);
        border-radius: 8px;
        padding: 12px;
        display: flex;
        flex-direction: column;
        gap: 6px;
        overflow: hidden;
    }
    .widget h2 { margin: 0; font-size: 13px; font-weight: 500; color: var(--accent); }
    .widget p { margin: 0; font-size: 12px; color: var(--text-muted); }
    .widget button {
        background: transparent;
        border: 1px solid var(--border);
        color: var(--text);
        border-radius: 4px;
        padding: 4px 10px;
        font-size: 12px;
        cursor: pointer;
        align-self: flex-start;
        transition: border-color 0.15s, color 0.15s;
    }
    .widget button:hover { border-color: var(--accent); color: var(--accent); }
    .pinned { border-color: var(--border-strong); }
    .pinned h2 { color: var(--text-muted); }
";

#[component]
fn app() -> Element {
    let store = use_layout_store(|| {
        vec![
            ("a".into(), GridPosition::new(0, 0, 4, 4)),
            ("b".into(), GridPosition::new(4, 0, 8, 2)),
            ("c".into(), GridPosition::new(4, 2, 4, 2)),
            ("d".into(), GridPosition::new(8, 2, 4, 4)),
        ]
    });

    let mut click_count = use_signal(|| 0u32);

    rsx! {
        document::Style { {STYLE} }
        GridLayout {
            cols: 12, row_height: 60.0, gap: 12.0,
            store: store, editable: true,
            on_change: move |snapshot: Vec<(String, GridPosition)>| {
                eprintln!("[playground] layout changed: {:?}", snapshot);
            },
            GridItem { id: "a", x: 0, y: 0, w: 4, h: 4, class: "widget",
                h2 { "Widget A" }
                p { "drag me anywhere" }
            }
            GridItem { id: "b", x: 4, y: 0, w: 8, h: 2, class: "widget",
                h2 { "Widget B" }
                p { "drag header — button below uses NoDrag" }
                NoDrag {
                    button {
                        onclick: move |_| click_count += 1,
                        "Clicked {click_count} time(s)"
                    }
                }
            }
            GridItem { id: "c", x: 4, y: 2, w: 4, h: 2, class: "widget",
                h2 { "Widget C" }
                p { "drag me" }
            }
            GridItem { id: "d", x: 8, y: 2, w: 4, h: 4, pinned: true, class: "widget pinned",
                h2 { "Widget D" }
                p { "pinned: never moves" }
            }
        }
    }
}
