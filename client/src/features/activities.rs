use dioxus::prelude::*;
use serde_json::{Value, json};

use crate::activity_window::{self, FromActivity};
use crate::protocol::{ClientMessage, Id};
use crate::state::{AppState, GatewayTx, use_app_state, use_gateway};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    UserRead,
    ChannelRead,
    MessageSend,
}

impl Capability {
    fn label(self) -> &'static str {
        match self {
            Capability::UserRead => "See your name and public key",
            Capability::ChannelRead => "See the current channel",
            Capability::MessageSend => "Post messages as you",
        }
    }
}

pub struct ActivityDef {
    pub id: &'static str,
    pub name: &'static str,
    pub icon: &'static str,
    pub caps: &'static [Capability],
    pub html: &'static str,
}

pub const ACTIVITIES: &[ActivityDef] = &[ActivityDef {
    id: "dice",
    name: "Dice Roller",
    icon: "🎲",
    caps: &[
        Capability::UserRead,
        Capability::ChannelRead,
        Capability::MessageSend,
    ],
    html: DICE_HTML,
}];

/// One activity window per client; a second launch replaces the first.
const WINDOW_KEY: u64 = 1;

pub fn bundle_host(html: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(html.as_bytes()))[..32].to_string()
}

#[component]
pub fn ActivityHost() -> Element {
    let state = use_app_state();
    let gateway = use_gateway();

    let mut picker_open = use_signal(|| false);
    let mut consenting = use_signal(|| None::<usize>);
    let mut launched = use_signal(|| None::<Launched>);

    use_drop(|| activity_window::close(WINDOW_KEY));

    use_effect(move || {
        let Some(channel) = launched.read().as_ref().and_then(|l| l.channel) else {
            return;
        };
        if !state.read().channels.iter().any(|c| c.id == channel) {
            activity_window::close(WINDOW_KEY);
            launched.set(None);
        }
    });

    let mut launch = move |idx: usize| {
        let Some(def) = ACTIVITIES.get(idx) else {
            return;
        };
        let channel = state.read().selected_channel;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        activity_window::open(activity_window::OpenSpec {
            key: WINDOW_KEY,
            title: format!("{} {} — activity", def.icon, def.name),
            host: bundle_host(def.html),
            bundle: std::sync::Arc::new(activity_window::Bundle::single_page(def.html)),
            tx,
        });
        launched.set(Some(Launched { idx, channel }));
        let gateway = gateway.clone();
        spawn(async move {
            while let Some(msg) = rx.recv().await {
                match msg {
                    FromActivity::Call { id, method, params } => {
                        let (ok, payload) =
                            handle_rpc(&method, &params, def, channel, &state, &gateway);
                        let result = if ok {
                            Ok(payload)
                        } else {
                            Err(payload.as_str().unwrap_or("refused").to_string())
                        };
                        activity_window::reply(WINDOW_KEY, id, result);
                    }
                    FromActivity::Closed | FromActivity::Failed(_) => {
                        if launched.peek().as_ref().map(|l| l.idx) == Some(idx) {
                            launched.set(None);
                        }
                        break;
                    }
                }
            }
        });
    };

    rsx! {
        button {
            class: "fixed bottom-3 left-3 z-40 border border-[var(--border)] rounded px-3 py-1 text-[10px] uppercase tracking-wider bg-[var(--panel)] hover:border-[var(--accent)] text-[var(--text-muted)] hover:text-[var(--accent)] transition-colors",
            onclick: move |_| {
                if launched.peek().is_some() && activity_window::is_open(WINDOW_KEY) {
                    activity_window::focus(WINDOW_KEY);
                }
                picker_open.set(!picker_open());
            },
            "Activities"
        }

        if picker_open() {
            div {
                class: "dxf-backdrop-in fixed inset-0 z-50 flex items-center justify-center bg-black/50",
                onclick: move |_| { picker_open.set(false); consenting.set(None); },
                div {
                    class: "dxf-modal-in w-80 bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg shadow-xl overflow-hidden",
                    onclick: move |e| e.stop_propagation(),
                    div { class: "px-4 py-3 border-b border-[var(--border)] flex items-center",
                        h3 { class: "text-sm font-medium text-[var(--accent)] flex-1", "Activities" }
                        button {
                            class: "text-[var(--text-dim)] hover:text-[var(--text)] text-lg leading-none",
                            onclick: move |_| { picker_open.set(false); consenting.set(None); },
                            "✕"
                        }
                    }
                    div { class: "p-2",
                        if let Some(idx) = consenting() {
                            ConsentPanel {
                                idx,
                                on_cancel: move |_| consenting.set(None),
                                on_approve: move |i: usize| {
                                    launch(i);
                                    consenting.set(None);
                                    picker_open.set(false);
                                },
                            }
                        } else {
                            for (i, def) in ACTIVITIES.iter().enumerate() {
                                button {
                                    key: "{def.id}",
                                    class: "w-full flex items-center gap-3 px-2 py-2 rounded hover:bg-white/[0.03] text-left transition-colors",
                                    onclick: move |_| consenting.set(Some(i)),
                                    span { class: "w-9 h-9 rounded-md border border-[var(--border)] flex items-center justify-center text-lg shrink-0",
                                        "{def.icon}"
                                    }
                                    div { class: "flex-1 min-w-0",
                                        div { class: "text-sm text-[var(--text)] truncate", "{def.name}" }
                                        div { class: "text-[10px] text-[var(--text-dim)]",
                                            "{def.caps.len()} permission(s)"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn ConsentPanel(
    idx: usize,
    on_cancel: EventHandler<()>,
    on_approve: EventHandler<usize>,
) -> Element {
    let Some(def) = ACTIVITIES.get(idx) else {
        return rsx! { Fragment {} };
    };
    rsx! {
        div { class: "px-1 py-1",
            div { class: "flex items-center gap-2 mb-2",
                span { class: "text-lg", "{def.icon}" }
                span { class: "text-sm text-[var(--text)] font-medium", "{def.name}" }
            }
            div { class: "text-xs text-[var(--text-muted)] mb-1.5", "This activity will be able to:" }
            ul { class: "space-y-1 mb-3",
                for cap in def.caps.iter().copied() {
                    li { class: "flex items-start gap-2 text-xs text-[var(--text)]",
                        span { class: "text-[var(--accent)]", "•" }
                        span { "{cap.label()}" }
                    }
                }
            }
            div { class: "text-[10px] text-[var(--text-dim)] mb-2",
                "Opens in its own window with no internet access. It can't read your other channels, files, or keys."
            }
            div { class: "flex gap-2",
                button {
                    class: "flex-1 rounded px-2 py-1.5 text-[11px] uppercase tracking-wider text-[var(--accent)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors",
                    onclick: move |_| on_approve.call(idx),
                    "Launch"
                }
                button {
                    class: "rounded px-3 py-1.5 text-[11px] uppercase tracking-wider text-[var(--text-dim)] hover:text-[var(--text-muted)] transition-colors",
                    onclick: move |_| on_cancel.call(()),
                    "Cancel"
                }
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
struct Launched {
    idx: usize,
    channel: Option<Id>,
}

fn handle_rpc(
    method: &str,
    params: &Value,
    def: &ActivityDef,
    bound_channel: Option<Id>,
    state: &Signal<AppState>,
    gateway: &GatewayTx,
) -> (bool, Value) {
    let has = |c: Capability| def.caps.contains(&c);
    match method {
        "user.get" if has(Capability::UserRead) => {
            let s = state.read();
            match &s.self_user {
                Some(u) => (true, json!({ "pubkey": u.pubkey, "username": u.username })),
                None => (false, json!("not connected")),
            }
        }
        "channel.get" if has(Capability::ChannelRead) => {
            let s = state.read();
            let cid = bound_channel;
            let name = cid.and_then(|id| {
                s.channels
                    .iter()
                    .find(|c| c.id == id)
                    .map(|c| c.name.clone())
            });
            (
                true,
                json!({ "id": cid.map(|i| i.to_string()), "name": name }),
            )
        }
        "message.send" if has(Capability::MessageSend) => {
            let content = params
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if content.is_empty() {
                return (false, json!("content is empty"));
            }
            let cid = bound_channel;
            match cid {
                Some(channel_id) => {
                    if gateway
                        .0
                        .send(ClientMessage::SendMessage {
                            image_name: None,
                            channel_id,
                            content,
                            image: None,
                            reply_to: None,
                        })
                        .is_err()
                    {
                        return (false, json!("The connection is closed"));
                    }
                    (true, json!({ "sent": true }))
                }
                None => (false, json!("no channel is open")),
            }
        }
        _ => (false, json!("permission denied or unknown method")),
    }
}

const DICE_HTML: &str = r##"<!doctype html><html><head><meta charset="utf-8"><style>
  body { margin:0; font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,sans-serif;
    background:#15110d; color:#e8e2da; height:100vh; display:flex; flex-direction:column;
    align-items:center; justify-content:center; gap:18px; }
  .die { font-size:84px; line-height:1; user-select:none; transition:transform .15s ease; }
  .die.rolling { transform:rotate(20deg) scale(1.1); }
  button { font:inherit; padding:8px 16px; border-radius:8px; border:1px solid #5a4634;
    background:transparent; color:#e0a06a; cursor:pointer; transition:all .15s ease; }
  button:hover { border-color:#e0a06a; }
  button:disabled { opacity:.4; cursor:default; }
  .who { font-size:13px; color:#9a8c7c; min-height:18px; }
  .row { display:flex; gap:10px; }
</style></head><body>
  <div class="who" id="who">…</div>
  <div class="die" id="die">🎲</div>
  <div class="row">
    <button id="roll">Roll</button>
    <button id="share" disabled>Share to chat</button>
  </div>
  <div class="who" id="where"></div>
  <script>
    const dxf = {
      getUser: function () { return discordia.call('user.get'); },
      getChannel: function () { return discordia.call('channel.get'); },
      sendMessage: function (content) { return discordia.call('message.send', { content: content }); }
    };

    const dieEl = document.getElementById('die');
    const rollBtn = document.getElementById('roll');
    const shareBtn = document.getElementById('share');
    const whoEl = document.getElementById('who');
    const whereEl = document.getElementById('where');
    const faces = ['⚀','⚁','⚂','⚃','⚄','⚅'];
    let last = 0;

    dxf.getUser().then(function (u) { whoEl.textContent = 'Hi, ' + u.username; })
       .catch(function () { whoEl.textContent = ''; });

    function showWhere() {
      dxf.getChannel()
         .then(function (c) { whereEl.textContent = c && c.name ? 'shares go to #' + c.name : ''; })
         .catch(function () { whereEl.textContent = ''; });
    }
    showWhere();

    rollBtn.addEventListener('click', function () {
      dieEl.classList.add('rolling');
      last = 1 + Math.floor(Math.random() * 6);
      setTimeout(function () {
        dieEl.textContent = faces[last - 1];
        dieEl.classList.remove('rolling');
        shareBtn.disabled = false;
      }, 150);
    });

    shareBtn.addEventListener('click', function () {
      if (!last) return;
      shareBtn.disabled = true;
      dxf.sendMessage('🎲 rolled a ' + last + '!')
         .then(function () { showWhere(); shareBtn.textContent = 'Shared!'; setTimeout(function(){ shareBtn.textContent='Share to chat'; shareBtn.disabled=false; }, 1200); })
         .catch(function (err) { shareBtn.textContent = 'Denied'; });
    });
  </script>
</body></html>"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn integration_requires_permission_and_shares_only_to_its_launch_channel() {
        fn harness() -> Element {
            let state = use_signal(AppState::empty);
            use_hook(move || {
                let channel = Id::new_v4();
                let mut state = state;
                state.write().selected_channel = Some(Id::new_v4());
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
                let gateway = GatewayTx(tx);
                let def = &ACTIVITIES[0];
                let denied = ActivityDef {
                    id: "restricted",
                    name: "Restricted",
                    icon: "",
                    caps: &[],
                    html: "",
                };
                let params = json!({ "content": "🎲 rolled a 6!" });
                assert!(
                    !handle_rpc(
                        "message.send",
                        &params,
                        &denied,
                        Some(channel),
                        &state,
                        &gateway
                    )
                    .0
                );
                assert!(rx.try_recv().is_err());
                assert!(
                    handle_rpc(
                        "message.send",
                        &params,
                        def,
                        Some(channel),
                        &state,
                        &gateway
                    )
                    .0
                );
                assert!(
                    matches!(rx.try_recv().unwrap(), ClientMessage::SendMessage { channel_id, content, .. } if channel_id == channel && content == "🎲 rolled a 6!")
                );
                assert!(!handle_rpc("message.send", &params, def, None, &state, &gateway).0);
                assert!(
                    !handle_rpc(
                        "message.send",
                        &json!({"content":" "}),
                        def,
                        Some(channel),
                        &state,
                        &gateway
                    )
                    .0
                );
                drop(rx);
                assert!(
                    !handle_rpc(
                        "message.send",
                        &params,
                        def,
                        Some(channel),
                        &state,
                        &gateway
                    )
                    .0
                );
            });
            rsx! {}
        }
        VirtualDom::new(harness).rebuild_in_place();
    }
}
