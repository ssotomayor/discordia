mod app;
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
    let window = WindowBuilder::new()
        .with_title("Discordia")
        .with_inner_size(LogicalSize::new(1280.0, 800.0))
        // Dioxus desktop dev builds default this to true. Turn it off so the
        // window behaves like any other window.
        .with_always_on_top(false);
    let window = mac_window(window);

    LaunchBuilder::new()
        .with_cfg(Config::new().with_window(window))
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
