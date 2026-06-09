mod app;
mod features;
mod host;
mod identity;
mod net;
mod protocol;
mod rendezvous;
mod session;
mod state;

fn main() {
    dioxus::launch(app::App);
}
