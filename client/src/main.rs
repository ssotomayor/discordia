// Release builds are windowed. Without this Windows gives the process a console
// of its own, so double-clicking the app puts a black box beside it for as long
// as it runs, and whatever went wrong scrolls past in a window nobody reads.
// `init_logging` writes to a file there instead. Debug keeps the console, which
// is where `dx serve` and a terminal run want it.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

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
mod nostr;
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
mod update;
mod version;
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
    let sink = Sink::open();
    let ansi = matches!(sink, Sink::Stdout);
    // `try_init` rather than `init`: a second call must not take the process
    // down over logging.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(default_filter())
        .with_target(false)
        .with_ansi(ansi)
        .with_writer(move || sink.clone())
        .try_init();
    redirect_std_handles();
    install_panic_hook();
    tracing::info!(version = version::VERSION, "Discordia starting");
}

/// Point the process's own stdout/stderr at the log file.
///
/// This crate carries ~140 `eprintln!` diagnostics that predate the subscriber
/// — voice, screen share, host, rendezvous — and a windowed build has no stderr
/// for them to reach: the write fails and the text is gone. Handing the process
/// a real handle again keeps all of them, at the cost of nothing, where
/// converting each call site would mean choosing a level for each and a diff
/// across seventeen files. Both are pointed at the same file the subscriber
/// writes, which is what the console did for them before.
///
/// Debug leaves them alone — there is a console there, and it is being watched.
/// That is a runtime `cfg!` rather than a `#[cfg]` so the release-only body is
/// still compiled (and so still checked) by an ordinary `cargo check`.
#[cfg(target_os = "windows")]
fn redirect_std_handles() {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Console::{STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle};

    if cfg!(debug_assertions) {
        return;
    }
    let Some(file) = LOG_FILE.get() else { return };
    let raw = file
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_raw_handle();
    // SAFETY: `raw` belongs to the file in `LOG_FILE`, a `OnceLock` that is
    // never taken from or cleared — so the handle outlives every write made
    // through it, including one from a panic on the way down.
    unsafe {
        let _ = SetStdHandle(STD_OUTPUT_HANDLE, HANDLE(raw as _));
        let _ = SetStdHandle(STD_ERROR_HANDLE, HANDLE(raw as _));
    }
}

#[cfg(not(target_os = "windows"))]
fn redirect_std_handles() {}

/// One previous log is kept; past this the current one becomes it.
const LOG_MAX_BYTES: u64 = 5 * 1024 * 1024;

/// The open log file, held for the life of the process.
///
/// `redirect_std_handles` hands its raw handle to the OS, which keeps no
/// reference of its own — so the file has to be kept alive here rather than
/// only inside the subscriber's writer.
static LOG_FILE: std::sync::OnceLock<std::sync::Arc<std::sync::Mutex<std::fs::File>>> =
    std::sync::OnceLock::new();

/// Where a release build's log lands. Under `config_dir`, so
/// `DIOXUSFUN_CONFIG_DIR` moves it along with everything else.
fn log_path() -> std::path::PathBuf {
    identity::config_dir().join("logs").join("discordia.log")
}

/// Where log output goes: the console in a debug build, a file in a release one.
///
/// A release build has no console (see the attribute at the top of this file),
/// so a subscriber writing to stdout there produces exactly the silence this
/// subscriber was added to end — and a worse kind, because the code still looks
/// like it logs.
#[derive(Clone)]
enum Sink {
    Stdout,
    File(std::sync::Arc<std::sync::Mutex<std::fs::File>>),
}

impl Sink {
    fn open() -> Self {
        if cfg!(debug_assertions) {
            return Self::Stdout;
        }
        // Falling back rather than reporting: the only channel a failure here
        // could be reported on is the one that just failed to open.
        Self::open_file().unwrap_or(Self::Stdout)
    }

    fn open_file() -> Option<Self> {
        let path = log_path();
        std::fs::create_dir_all(path.parent()?).ok()?;
        rotate(&path, LOG_MAX_BYTES);
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok()?;
        let file = LOG_FILE.get_or_init(|| std::sync::Arc::new(std::sync::Mutex::new(file)));
        Some(Self::File(file.clone()))
    }
}

impl std::io::Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            // Poisoning is recovered from, not unwrapped: the panic hook logs
            // through this same lock, and a panic *while holding* it would
            // otherwise turn a recoverable crash into an abort inside the hook.
            Self::File(f) => f.lock().unwrap_or_else(|e| e.into_inner()).write(buf),
            Self::Stdout => std::io::stdout().write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::File(f) => f.lock().unwrap_or_else(|e| e.into_inner()).flush(),
            Self::Stdout => std::io::stdout().flush(),
        }
    }
}

/// Roll the log over once it passes `max_bytes`, keeping one generation.
///
/// Limits are arguments rather than read from `log_path()` so a test can drive
/// this against a temp dir: `DIOXUSFUN_CONFIG_DIR` is process-wide, and tests
/// share a process.
fn rotate(path: &std::path::Path, max_bytes: u64) {
    let full = std::fs::metadata(path).is_ok_and(|m| m.len() >= max_bytes);
    if full {
        let _ = std::fs::rename(path, path.with_extension("log.1"));
    }
}

/// Send panics to the log as well as to the default handler.
///
/// The default hook writes to stderr, which a release build does not have, so
/// until now the one event most worth having a record of was the one that left
/// none. Chained rather than replaced, so a debug run still prints as usual.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("{info}\n{}", std::backtrace::Backtrace::force_capture());
        previous(info);
    }));
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

    // The executable a previous portable update parked. Returns at once and
    // keeps trying on a thread — the build that parked it is usually still
    // exiting when this runs, and holding the file.
    update::sweep_outgoing();

    // Must run before any Dioxus/wry calls: wry paints with WebView2, so
    // without it the window opens blank.
    // Also serves the installer path (`--ensure-webview2`), which exits here.
    #[cfg(target_os = "windows")]
    webview2::gate();

    // WebView2 hides `navigator.mediaDevices` in insecure contexts, breaking
    // screen sharing.
    // Both origin spellings are listed because Chromium's origin parser may
    // not match the trailing slash.
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

    /// A release build appends to one file for the life of the install, so the
    /// only thing standing between a user and an unbounded log is this.
    #[test]
    fn a_full_log_rolls_over_and_keeps_one_generation() {
        let dir = std::env::temp_dir().join("dxf-log-rotate-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("discordia.log");
        let rolled = dir.join("discordia.log.1");

        std::fs::write(&path, b"0123456789").unwrap();
        super::rotate(&path, 100);
        assert!(path.exists(), "rotated a log that was nowhere near the limit");
        assert!(!rolled.exists());

        super::rotate(&path, 10);
        assert!(!path.exists(), "the full log was left in place");
        assert_eq!(std::fs::read(&rolled).unwrap(), b"0123456789");

        // The second roll overwrites the first: one generation, not a pile.
        std::fs::write(&path, b"newer").unwrap();
        super::rotate(&path, 1);
        assert_eq!(std::fs::read(&rolled).unwrap(), b"newer");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_filter_names_the_crate_the_compiler_actually_used() {
        // Pinned to the literal, not compared against another `module_path!()`
        // — two derivations of the same thing would agree even if both were
        // wrong, and the value is the whole point.
        assert_eq!(super::log_crate_root(), "Discordia");
    }
}
