//! Runnable playground for `dioxus-grid-layout`.
//!
//! Run with:
//!
//! ```bash
//! cargo run --example playground -p dioxus-grid-layout
//! ```

use dioxus::prelude::*;
use dioxus_grid_layout::{GridItem, GridLayout, GridPosition, use_layout_store};

fn main() {
    dioxus::launch(app);
}

const STYLE: &str = "
    html, body { margin: 0; height: 100%; background: #0f1115; color: #e5e7eb;
        font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; }
    .dioxus-grid-layout { padding: 16px; }
    .widget { border-radius: 8px; padding: 12px; display: flex; flex-direction: column; }
    .widget h2 { margin: 0 0 8px; font-size: 14px; font-weight: 600; opacity: 0.85; }
    .widget p { margin: 0; font-size: 12px; opacity: 0.7; }
    .w-a { background: #1e293b; border: 1px solid #334155; }
    .w-b { background: #3b1a4a; border: 1px solid #6b2580; }
    .w-c { background: #1e3a3a; border: 1px solid #2d6363; }
    .w-d { background: #3a2618; border: 1px solid #6b3c25; }
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

    rsx! {
        document::Style { {STYLE} }
        GridLayout { cols: 12, row_height: 60.0, gap: 12.0, store: store, editable: true,
            // Without explicit x/y/w/h here the store-driven path takes over.
            // Props still required (the v0.0.1 fallback), so pass placeholders
            // matching the initial store values.
            GridItem { id: "a", x: 0, y: 0, w: 4, h: 4, class: "widget w-a",
                h2 { "Widget A" }
                p { "drag me" }
            }
            GridItem { id: "b", x: 4, y: 0, w: 8, h: 2, class: "widget w-b",
                h2 { "Widget B" }
                p { "wide header" }
            }
            GridItem { id: "c", x: 4, y: 2, w: 4, h: 2, class: "widget w-c",
                h2 { "Widget C" }
                p { "drag me" }
            }
            GridItem { id: "d", x: 8, y: 2, w: 4, h: 4, pinned: true, class: "widget w-d",
                h2 { "Widget D" }
                p { "pinned (commit 2 will lock me)" }
            }
        }
    }
}
