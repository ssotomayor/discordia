mod app;
mod features;
mod host;
mod identity;
mod net;
mod protocol;
mod rendezvous;
mod session;
mod state;
mod wallet;

use dioxus::LaunchBuilder;
use dioxus::desktop::{Config, WindowBuilder, tao::dpi::LogicalSize};

fn main() {
    let window = WindowBuilder::new()
        .with_title("dioxusfun")
        .with_inner_size(LogicalSize::new(1280.0, 800.0))
        // Dioxus desktop dev builds default this to true. Turn it off so the
        // window behaves like any other macOS window — focusable, sendable
        // behind other apps, etc.
        .with_always_on_top(false);
    LaunchBuilder::new()
        .with_cfg(Config::new().with_window(window))
        .launch(app::App);
}
