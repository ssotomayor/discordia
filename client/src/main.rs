mod app;
mod features;
mod host;
mod net;
mod protocol;
mod rendezvous;
mod state;

fn main() {
    dioxus::launch(app::App);
}
