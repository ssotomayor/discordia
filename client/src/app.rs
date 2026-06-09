use dioxus::prelude::*;

use crate::features::{
    connect::ConnectView, identity_setup::IdentitySetupView, workspace::WorkspaceView,
};
use crate::identity::Identity;
use crate::state::{SessionMode, SessionParams};

const BASE_CSS: &str = "
:root {
  --bg: #0a0908;
  --panel: #0a0908;
  --border: rgba(190, 130, 90, 0.18);
  --border-strong: rgba(190, 130, 90, 0.35);
  --text: #d6d6d6;
  --text-muted: #888888;
  --text-dim: #5a5a5a;
  --accent: #e0a06a;
  --accent-soft: rgba(224, 160, 106, 0.10);
  --accent-strong: #ec8f3f;
  --success: #8fa872;
  --warn: #d4a04f;
  --danger: #c67878;
}
html, body, #main { height: 100%; margin: 0; }
body {
  background: var(--bg);
  color: var(--text);
  font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
}
* { box-sizing: border-box; }
::-webkit-scrollbar { width: 8px; height: 8px; }
::-webkit-scrollbar-track { background: transparent; }
::-webkit-scrollbar-thumb { background: rgba(190, 130, 90, 0.18); border-radius: 4px; }
::-webkit-scrollbar-thumb:hover { background: rgba(190, 130, 90, 0.3); }
button { cursor: pointer; }
button:disabled { cursor: not-allowed; }
input::placeholder { color: var(--text-dim); }
";

#[component]
pub fn App() -> Element {
    // Load identity from disk on mount. None if no identity yet — first-launch.
    let mut identity = use_signal(|| Identity::load().ok().flatten());
    let mut session = use_signal(|| None::<SessionParams>);
    let mut error = use_signal(|| None::<String>);

    rsx! {
        document::Script { src: "https://unpkg.com/@tailwindcss/browser@4" }
        document::Style { {BASE_CSS} }

        div { class: "h-screen w-screen bg-[var(--bg)] text-[var(--text)] antialiased overflow-hidden",
            match (identity.read().clone(), session.read().clone()) {
                (None, _) => rsx! {
                    IdentitySetupView {
                        on_done: move |new_id: Identity| identity.set(Some(new_id)),
                    }
                },
                (Some(id), None) => rsx! {
                    ConnectView {
                        identity: id,
                        error: error(),
                        on_connect: move |params: SessionParams| {
                            error.set(None);
                            session.set(Some(params));
                        },
                        on_sign_out: move |_| {
                            let _ = Identity::delete_file();
                            session.set(None);
                            identity.set(None);
                        },
                    }
                },
                (Some(_), Some(params)) => rsx! {
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
    format!("{mode}|{}|{}", p.username, p.identity.pubkey)
}
