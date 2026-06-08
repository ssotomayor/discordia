# dioxus-grid-layout

A draggable, resizable grid layout for [Dioxus](https://dioxuslabs.com/).
Inspired by [`react-grid-layout`](https://github.com/react-grid-layout/react-grid-layout).

> **Status: v0.0.1 — scaffold only.** Renders items statically via CSS grid.
> Drag, resize, and collision resolution land in subsequent commits on the
> `feature/dashboard-grid` branch.

## Goals

- Drop-in dashboard primitive for Dioxus apps: `GridLayout` container,
  `GridItem` children with `(x, y, w, h)`.
- Pointer-driven drag of items by a designated handle element.
- Pointer-driven resize from the bottom-right corner.
- Collision resolution — when an item is dropped/resized over another, the
  displaced item gets pushed down.
- Edit-mode toggle so drag/resize only kicks in when the host app enables it.
- Layout state is `Serialize`/`Deserialize` so the consumer can persist it
  (localStorage, a backend, anywhere).

## Non-goals (at least not for v0.x)

- Responsive breakpoints (md/lg/xl variants of the layout).
- Animation transitions on layout change.
- Touch gestures (mouse / pointer only).
- Built-in storage adapter — persistence is the host's choice.

## Example

```rust
use dioxus::prelude::*;
use dioxus_grid_layout::{GridLayout, GridItem};

#[component]
fn App() -> Element {
    rsx! {
        GridLayout { cols: 12, row_height: 60.0, gap: 10.0,
            GridItem { id: "a", x: 0, y: 0, w: 4, h: 3,
                div { "Widget A" }
            }
            GridItem { id: "b", x: 4, y: 0, w: 8, h: 2,
                div { "Widget B" }
            }
        }
    }
}
```

Run the bundled playground:

```bash
cargo run --example playground -p dioxus-grid-layout
```

## Roadmap

| Version | Adds |
|---|---|
| **0.0.1 (now)** | Static rendering. `GridLayout`/`GridItem` props, CSS-grid placement, `pinned` flag, serializable `LayoutSpec`. |
| **0.1** | Drag by handle, resize from bottom-right corner, collision resolution (push-down), `editable` toggle wired up. |
| **0.2** | `use_layout_store(initial)` hook for owned + signalled layout state. Layout change events. |
| **0.3** | Min/max bounds enforced. Hit-test optimizations. |
| **0.4** | Responsive breakpoints. |
| **0.5+** | Polish for crates.io release: docs, examples site, broader Dioxus version compat. |

## Layout

For now this crate lives inside the [`dioxusfun-monorepo`](https://github.com/tehsoto/dioxusfun-monorepo)
workspace as the dogfood consumer. Once the API stabilizes around v0.1–0.2,
it'll be extracted into its own repo and published to crates.io.

## License

Dual-licensed under MIT OR Apache-2.0.
