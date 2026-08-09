mod app;
mod blossom;
mod denoise;
mod emoji;
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
mod sysaudio;

use dioxus::LaunchBuilder;
use dioxus::desktop::{Config, WindowBuilder, tao::dpi::LogicalSize, tao::window::Icon};

fn main() {
    // WebView2 (Chromium) treats our asset origin as insecure, and an insecure
    // context hides `navigator.mediaDevices` entirely — which is what stops
    // screen sharing working on Windows. Allowlist the origin as secure.
    //
    // Both spellings are listed on purpose. Dioxus serves Windows from
    // `http://dioxus.index.html/` (see dioxus-desktop's BASE_URI), but Chromium
    // parses this flag as a list of *origins*, where a trailing slash may not
    // match. Passing both costs nothing and removes the guess.
    #[cfg(target_os = "windows")]
    unsafe {
        std::env::set_var(
            "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS",
            "--unsafely-treat-insecure-origin-as-secure=\
             http://dioxus.index.html,http://dioxus.index.html/",
        );
    }

    let window = WindowBuilder::new()
        .with_title("Discordia")
        .with_inner_size(LogicalSize::new(1280.0, 800.0))
        .with_always_on_top(false)
        .with_window_icon(load_window_icon());
    let window = mac_window(window);
    LaunchBuilder::new()
        .with_cfg(apply_menu(Config::new().with_window(window)))
        .launch(app::App);
}

/// macOS routes ⌘C/⌘V/⌘X/⌘A/⌘Z through the application menu bar: those
/// shortcuts are *owned* by the Edit menu's items, not by the webview. With no
/// menu there is nothing holding the accelerators, which is why ⌘-anything did
/// nothing in every input in the app.
///
/// So keep Dioxus's default menu bar on macOS — it draws in the system menu bar
/// at the top of the screen, not inside our window, so it costs us no chrome —
/// and only strip it on Windows/Linux, where a menu *would* paint a strip inside
/// the frameless window.
#[cfg(target_os = "macos")]
fn apply_menu(cfg: Config) -> Config {
    cfg
}

#[cfg(not(target_os = "macos"))]
fn apply_menu(cfg: Config) -> Config {
    cfg.with_menu(None)
}

/// Load the Discordia app icon from the bundled PNG so the native window
/// (and thus the Windows taskbar / alt-tab) shows our logo instead of the
/// default Dioxus/wry icon. Returns `None` on non-Windows platforms or if
/// decoding fails (the window simply falls back to the default icon).
#[cfg(target_os = "windows")]
fn load_window_icon() -> Option<Icon> {
    let png = include_bytes!("../assets/icon-1024.png");
    let img = image::load_from_memory(png).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    Icon::from_rgba(img.into_raw(), w, h).ok()
}

#[cfg(not(target_os = "windows"))]
fn load_window_icon() -> Option<Icon> {
    None
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
