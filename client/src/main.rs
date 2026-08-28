#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod agc;
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

fn log_crate_root() -> &'static str {
    module_path!().split("::").next().unwrap_or("Discordia")
}

fn init_logging() {
    let sink = Sink::open();
    let ansi = matches!(sink, Sink::Stdout);
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
    unsafe {
        let _ = SetStdHandle(STD_OUTPUT_HANDLE, HANDLE(raw as _));
        let _ = SetStdHandle(STD_ERROR_HANDLE, HANDLE(raw as _));
    }
}

#[cfg(not(target_os = "windows"))]
fn redirect_std_handles() {}

const LOG_MAX_BYTES: u64 = 5 * 1024 * 1024;

static LOG_FILE: std::sync::OnceLock<std::sync::Arc<std::sync::Mutex<std::fs::File>>> =
    std::sync::OnceLock::new();

fn log_path() -> std::path::PathBuf {
    identity::config_dir().join("logs").join("discordia.log")
}

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

fn rotate(path: &std::path::Path, max_bytes: u64) {
    let full = std::fs::metadata(path).is_ok_and(|m| m.len() >= max_bytes);
    if full {
        let _ = std::fs::rename(path, path.with_extension("log.1"));
    }
}

fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("{info}\n{}", std::backtrace::Backtrace::force_capture());
        previous(info);
    }));
}

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

    update::sweep_outgoing();

    #[cfg(target_os = "windows")]
    webview2::gate();

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
        .with_inner_size(LogicalSize::new(1440.0, 900.0))
        // The binding case is the social drawer open: it floors at 600 and the
        // connect form stops being readable under ~420. Anything narrower and
        // the form starts wrapping its own controls.
        .with_min_inner_size(LogicalSize::new(1024.0, 600.0))
        .with_always_on_top(false)
        .with_window_icon(load_window_icon());
    let window = mac_window(window);
    LaunchBuilder::new()
        .with_cfg(apply_menu(Config::new().with_window(window)))
        .launch(app::App);
}

#[cfg(target_os = "macos")]
fn apply_menu(cfg: Config) -> Config {
    cfg
}

#[cfg(not(target_os = "macos"))]
fn apply_menu(cfg: Config) -> Config {
    cfg.with_menu(None)
}

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
    fn a_full_log_rolls_over_and_keeps_one_generation() {
        let dir = std::env::temp_dir().join("dxf-log-rotate-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("discordia.log");
        let rolled = dir.join("discordia.log.1");

        std::fs::write(&path, b"0123456789").unwrap();
        super::rotate(&path, 100);
        assert!(
            path.exists(),
            "rotated a log that was nowhere near the limit"
        );
        assert!(!rolled.exists());

        super::rotate(&path, 10);
        assert!(!path.exists(), "the full log was left in place");
        assert_eq!(std::fs::read(&rolled).unwrap(), b"0123456789");

        std::fs::write(&path, b"newer").unwrap();
        super::rotate(&path, 1);
        assert_eq!(std::fs::read(&rolled).unwrap(), b"newer");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_filter_names_the_crate_the_compiler_actually_used() {
        assert_eq!(super::log_crate_root(), "Discordia");
    }
}
