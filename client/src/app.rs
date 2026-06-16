use dioxus::prelude::*;

use crate::features::{
    connect::ConnectView, identity_setup::IdentitySetupView, workspace::WorkspaceView,
};
use crate::identity::Identity;
use crate::session::{self, SavedSession};
use crate::state::{SessionMode, SessionParams};

/// App brand mark. Yellow + dark gradient stylized D-shape; renders
/// cleanly at any size. Used in two places: the connect screen header
/// and the workspace top bar (next to the wallet).
pub const DISCORDIA_LOGO: Asset = asset!("/assets/discordia-logo.svg");

const BASE_CSS: &str = "
:root {
  --bg: #0a0908;
  --panel: #0a0908;
  --border: rgba(238, 202, 178, 0.18);
  --border-strong: rgba(255, 209, 179, 0.35);
  --text: #d6d6d6;
  --text-muted: #888888;
  --text-dim: #5a5a5a;
  --accent: #e0a06a;
  --accent-soft: rgba(224, 160, 106, 0.10);
  --accent-strong: #ec8f3f;
  --success: #8fa872;
  --warn: #d4a04f;
  --danger: #c67878;
  --ease: cubic-bezier(0.4, 0.0, 0.2, 1);
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
::-webkit-scrollbar-thumb { background: rgba(190, 130, 90, 0.18); border-radius: 4px; transition: background 0.2s var(--ease); }
::-webkit-scrollbar-thumb:hover { background: rgba(190, 130, 90, 0.35); }
button { cursor: pointer; }
button:disabled { cursor: not-allowed; }
input::placeholder { color: var(--text-dim); }

/* Smooth color/border transitions on every interactive surface. Excluded
   from `transform` so drag-in-progress (which is driven by transform via
   document::eval) doesn't get interpolated. */
button, a, input, textarea, select, summary, [role='button'] {
  transition: color 0.15s var(--ease),
              background-color 0.15s var(--ease),
              border-color 0.18s var(--ease),
              opacity 0.15s var(--ease);
}
button:active:not(:disabled) { transform: scale(0.985); }

/* Apply to any bordered panel/widget for a subtle hover brightening. */
.panel-hover {
  transition: border-color 0.2s var(--ease), background-color 0.2s var(--ease);
}
.panel-hover:hover {
  border-color: var(--border-strong);
}

/* Fade-in animation used on tab content / step content so switches feel
   intentional instead of jarring snaps. */
@keyframes dxf-fade-in {
  from { opacity: 0; transform: translateY(4px); }
  to   { opacity: 1; transform: translateY(0); }
}
.fade-in { animation: dxf-fade-in 0.18s var(--ease) both; }

/* Window drag regions. With macOS fullsize content view + transparent
   titlebar, there's no OS titlebar strip — so the user needs SOME region
   they can grab to move the window. Anything tagged .dxf-drag-region is
   draggable; buttons / inputs inside such a region must opt out with
   .dxf-no-drag so they remain clickable. The traffic lights stay at
   the top-left and float over our content. */
.dxf-drag-region { -webkit-app-region: drag; user-select: none; }
.dxf-no-drag { -webkit-app-region: no-drag; }

/* Discordia brand mark — gentle wiggle + scale + warm glow on hover.
   Two states: a slow idle drift (so it feels alive without demanding
   attention) and a stronger hover response. */
@keyframes dxf-logo-idle {
  0%, 100% { transform: rotate(0deg); }
  50%      { transform: rotate(-2deg); }
}
.dxf-logo {
  transition: transform 0.35s var(--ease), filter 0.35s var(--ease);
  transform-origin: center;
  animation: dxf-logo-idle 6s var(--ease) infinite;
  will-change: transform, filter;
}
.dxf-logo:hover {
  transform: rotate(-12deg) scale(1.12);
  filter: drop-shadow(0 0 10px rgba(255, 210, 26, 0.55));
  animation-play-state: paused;
}
";

#[component]
pub fn App() -> Element {
    let mut identity = use_signal(|| Identity::load().ok().flatten());
    let mut session = use_signal(|| None::<SessionParams>);
    let mut error = use_signal(|| None::<String>);
    let last_session = use_signal(|| session::load().ok().flatten());

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
                        last_session: last_session.read().clone(),
                        on_connect: move |params: SessionParams| {
                            error.set(None);
                            // Persist for next launch's Reconnect button.
                            let saved = SavedSession {
                                mode: params.mode.clone(),
                                username: params.username.clone(),
                            };
                            let _ = session::save(&saved);
                            session.set(Some(params));
                        },
                        on_rename: move |new_name: String| {
                            // Mutate the live identity + persist to disk. The
                            // new name takes effect on the next Connect (we
                            // don't surgery the in-flight gateway session).
                            let mut current = identity.write();
                            if let Some(id) = current.as_mut() {
                                let _ = id.set_display_name(new_name);
                            }
                        },
                        on_sign_out: move |_| {
                            let _ = Identity::delete_file();
                            let _ = session::clear();
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
