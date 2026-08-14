mod app;
mod blossom;
mod denoise;
mod e2ee;
#[macro_use]
mod devlog;
mod emoji;
mod features;
mod host;
mod identity;
mod mediakey;
mod net;
mod portmap;
mod profile;
mod protocol;
mod quic;
mod rawmic;
mod rendezvous;
mod session;
mod settings;
mod state;
mod sysaudio;
mod sysvideo;
#[cfg(target_os = "windows")]
mod webview2;

use dioxus::LaunchBuilder;
use dioxus::desktop::{Config, WindowBuilder, tao::dpi::LogicalSize, tao::window::Icon};

/// The name the compiler gave this crate, which a log filter has to match.
///
/// Taken from `module_path!()` rather than written down, because the two differ:
/// the bin target is `Discordia`, so events from this crate carry targets like
/// `Discordia::net` while the package is called `dioxusfun`. Hard-coding the
/// package name would produce a filter that matches nothing and a subscriber
/// that prints nothing — the exact silence this is meant to fix.
fn log_crate_root() -> &'static str {
    module_path!().split("::").next().unwrap_or("Discordia")
}

/// Turn on log output, at a level that is useful without being a wall.
///
/// This crate has always depended on `tracing` and never installed a
/// subscriber, so every `tracing::warn!` in it — sixteen of them, most along the
/// networking paths explaining *why* a direct connection lost to the relay —
/// was formatted and dropped. The failures that most need explaining are the
/// quiet ones: a port mapping the router renumbered, a QUIC path that timed out,
/// a session closed because it slid back onto a relay it was told not to use.
/// None of those reach the UI, and until now none of them reached anywhere else
/// either.
///
/// Our crates at `info`, everything else at `warn`: iroh, livekit and hyper are
/// all chatty enough at `info` to bury exactly what this is for. `RUST_LOG`
/// overrides the lot when more is wanted.
fn init_logging() {
    // `try_init` rather than `init`: a second call must not take the process
    // down over logging.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(default_filter())
        .with_target(false)
        .try_init();
}

/// The filter `init_logging` installs, split out so a test can drive it.
fn default_filter() -> tracing_subscriber::EnvFilter {
    use tracing_subscriber::EnvFilter;
    EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new(format!(
            "warn,{}=info,dioxusfun_server=info",
            log_crate_root()
        ))
    })
}

fn main() {
    init_logging();

    // Settle the WebView2 runtime before anything can reach for it: wry paints
    // every pixel with it, so without one the window would open and never
    // render. This either returns with a runtime available or exits — and it is
    // deliberately the first statement, ahead of every Dioxus call below.
    //
    // It also serves the installer, which runs this same binary with
    // `--ensure-webview2` and never gets past this line. See `webview2::gate`.
    #[cfg(target_os = "windows")]
    webview2::gate();

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

#[cfg(test)]
mod logging_tests {
    /// The log filter names this crate, and the name is not the one you would
    /// guess: the bin target is `Discordia`, so `module_path!()` rooted here is
    /// `Discordia::…`, not `dioxusfun::…`. A filter written against the package
    /// name would silently match nothing — which is the failure mode the
    /// subscriber exists to end, so it is pinned rather than assumed.
    /// The end of the chain, not the middle: emit a real event from this crate
    /// under the real filter and assert the text comes out. A filter that names
    /// the wrong crate compiles, installs, and prints nothing — so anything
    /// short of capturing output would pass while the feature stayed broken.
    #[test]
    fn an_event_from_this_crate_reaches_the_writer() {
        use std::sync::{Arc, Mutex};
        #[derive(Clone, Default)]
        struct Capture(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Capture {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let capture = Capture::default();
        let sink = capture.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(super::default_filter())
            .with_target(false)
            .with_ansi(false)
            .with_writer(move || sink.clone())
            .finish();

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(marker = "reachability", "direct path lost to the relay");
            // Below the bar for third-party crates, and this crate is not one —
            // but the level still applies, so a debug line must not appear.
            tracing::debug!("noise that should stay filtered out");
        });

        let printed = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
        assert!(
            printed.contains("direct path lost to the relay"),
            "nothing was printed — the filter does not match this crate: {printed:?}"
        );
        assert!(!printed.contains("noise that should stay"), "{printed}");
    }

    #[test]
    fn the_filter_names_the_crate_the_compiler_actually_used() {
        // Pinned to the literal, not compared against another `module_path!()`
        // — two derivations of the same thing would agree even if both were
        // wrong, and the value is the whole point.
        assert_eq!(super::log_crate_root(), "Discordia");
    }
}
