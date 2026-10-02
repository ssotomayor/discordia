# Dioxus Desktop 0.7.10

Source: crates.io `dioxus-desktop` 0.7.10, upstream revision
`57d6794ad60b949e5bd8aa282f6f8c3dc97a365e`, `packages/desktop`.
Rust and JS sources are upstream except the developer-tool gates below.

| File | Local change |
|---|---|
| `Cargo.toml` | Disable default image formats; retain PNG and ICO for icons. The application supplies JPEG/GIF/WebP; arboard supplies BMP on Windows. |
| `Cargo.toml` | Defaults retain Tokio and transparent windows; developer tools remain available through the client's `devtools` feature. |
| `Cargo.toml` | Tungstenite 0.29 shares the version used by LiveKit. |
| `src/app.rs`, `src/webview.rs` | Gate developer-only toast code, inspector state/actions and imports alongside their callers. |
| `src/menubar.rs`, `src/desktop_context.rs` | Hide inactive inspector actions and gate the public inspector entry point. |

On upgrades, replace the upstream files, reapply these changes and compare the
client's Windows/macOS dependency trees. Preserve the bundled JS and `hash.txt`;
unchanged TypeScript does not require Bun during builds.

Licenses: `LICENSE-MIT`, `LICENSE-APACHE`.
