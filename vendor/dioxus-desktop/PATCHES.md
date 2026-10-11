# Dioxus Desktop 0.7.10

Source: crates.io `dioxus-desktop` 0.7.10, upstream revision
`57d6794ad60b949e5bd8aa282f6f8c3dc97a365e`, `packages/desktop`.
Local changes are listed below; bundled JS remains upstream.

| File | Local change |
|---|---|
| `Cargo.toml` | Disable default image formats; retain PNG and ICO for icons. The application supplies JPEG/GIF/WebP; arboard supplies BMP on Windows. |
| `Cargo.toml` | Defaults retain Tokio and transparent windows; developer tools remain available through the client's `devtools` feature. |
| `Cargo.toml` | Tungstenite 0.29 shares the version used by LiveKit. |
| `src/app.rs`, `src/webview.rs` | Gate developer-only toast code, inspector state/actions and imports alongside their callers. |
| `src/menubar.rs`, `src/desktop_context.rs` | Hide inactive inspector actions and gate the public inspector entry point. |
| `src/desktop_context.rs`, `src/app.rs` | `DesktopService::set_visible` also hides the wry webview and drops it to WebView2's low memory target, so a window closed to the tray stops rendering. |
| `src/edits.rs` | A disconnected edit socket never acknowledges unapplied DOM mutations. Requeue its in-flight and queued batches in order; release idle disconnected sockets. Reconnects ask the existing reader to check liveness and wait for its queue to retire before registering; live/busy duplicates cannot replace it. Native loopback regressions run in desktop CI. |
| `src/app.rs`, `src/webview.rs`, `src/windows_diagnostics.rs` | Log webview creation, initialization, first batch acknowledgement per connection and Windows navigation/process failures without socket keys or URLs. |

On upgrades, replace the upstream files, reapply these changes and compare the
client's Windows/macOS dependency trees. Preserve the bundled JS and `hash.txt`;
unchanged TypeScript does not require Bun during builds.

Licenses: `LICENSE-MIT`, `LICENSE-APACHE`.
