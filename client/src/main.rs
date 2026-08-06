mod app;
mod blossom;
mod features;
mod host;
mod identity;
mod net;
mod profile;
mod protocol;
mod rendezvous;
mod session;
mod settings;
mod state;

use dioxus::LaunchBuilder;
use dioxus::desktop::{Config, WindowBuilder, tao::dpi::LogicalSize};

fn main() {
    // WebView2 (Chromium) treats our custom `dioxus://` asset origin as
    // insecure, which hides navigator.mediaDevices (getDisplayMedia) needed
    // for screen sharing. Explicitly allowlist it as secure.
    #[cfg(target_os = "windows")]
    unsafe {
        std::env::set_var(
            "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS",
            "--unsafely-treat-insecure-origin-as-secure=http://dioxus.index.html/",
        );
    }

    let window = WindowBuilder::new()
        .with_title("Discordia")
        .with_inner_size(LogicalSize::new(1280.0, 800.0))
        .with_always_on_top(false);
    let window = mac_window(window);
    LaunchBuilder::new()
        .with_cfg(Config::new().with_window(window).with_menu(None))
        .launch(app::App);
}

/// macOS: hide the titlebar bar but keep the traffic lights, and extend
/// the content view to the very top of the window. This is the "Discord-on
/// -macOS" look — no chrome strip, brand panel reaches the window edge,
/// traffic lights float over content.
#[cfg(target_os = "macos")]
fn mac_window(w: WindowBuilder) -> WindowBuilder {
    use dioxus::desktop::tao::platform::macos::WindowBuilderExtMacOS;
    w.with_titlebar_transparent(true)
        .with_fullsize_content_view(true)
}

#[cfg(not(target_os = "macos"))]
fn mac_window(w: WindowBuilder) -> WindowBuilder {
    w
}
