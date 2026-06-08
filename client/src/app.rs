use dioxus::prelude::*;

use crate::features::{connect::ConnectView, workspace::WorkspaceView};
use crate::state::{SessionMode, SessionParams};

const BASE_CSS: &str = "html,body,#main{height:100%;margin:0}body{background:#313338;color:#e5e7eb;font-family:-apple-system,BlinkMacSystemFont,Segoe UI,Roboto,sans-serif}*{box-sizing:border-box}";

#[component]
pub fn App() -> Element {
    let mut session = use_signal(|| None::<SessionParams>);
    let mut error = use_signal(|| None::<String>);

    rsx! {
        document::Script { src: "https://unpkg.com/@tailwindcss/browser@4" }
        document::Style { {BASE_CSS} }

        div { class: "h-screen w-screen bg-[#313338] text-gray-100 font-sans antialiased overflow-hidden",
            match session() {
                None => rsx! {
                    ConnectView {
                        error: error(),
                        on_connect: move |params: SessionParams| {
                            error.set(None);
                            session.set(Some(params));
                        },
                    }
                },
                Some(params) => rsx! {
                    WorkspaceView {
                        key: "{session_key(&params)}",
                        params: params.clone(),
                        on_disconnect: move |reason: String| {
                            error.set(Some(reason));
                            session.set(None);
                        },
                    }
                },
            }
        }
    }
}

fn session_key(p: &SessionParams) -> String {
    let mode = match &p.mode {
        SessionMode::Remote { server_url } => format!("remote:{server_url}"),
        SessionMode::SelfHost { allow_lan, rendezvous_url, publish_public, .. } => {
            format!(
                "selfhost:{allow_lan}:{}:{publish_public}",
                rendezvous_url.as_deref().unwrap_or("")
            )
        }
        SessionMode::ByCode { rendezvous_url, code } => {
            format!("bycode:{rendezvous_url}:{code}")
        }
    };
    format!("{mode}|{}", p.username)
}
