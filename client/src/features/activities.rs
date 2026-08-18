//! Tier 3 — **client-sandboxed activities**.
//!
//! An activity is a mini-app rendered in a `<iframe sandbox="allow-scripts">`.
//! Because it's sandboxed *without* `allow-same-origin`, it runs in a unique
//! opaque origin: it can't touch the app's DOM, storage, or network. Its only
//! channel to the host is `postMessage`, bridged to Rust through a constrained
//! RPC surface. Each activity declares the **capabilities** it needs; the user
//! approves them at launch, and the bridge enforces that grant on every call.
//!
//! This mirrors Discord's Embedded App SDK model: untrusted UI runs sandboxed
//! in the user's own client, never on a server, and reaches the host only
//! through a capability-checked RPC bridge.
//!
//! The bridge reuses the same `document::eval` JS pattern as `screenshare.rs`.

use dioxus::prelude::*;
use serde_json::{Value, json};

use crate::protocol::{ClientMessage, Id};
use crate::state::{AppState, GatewayTx, use_app_state, use_gateway};

/// A capability an activity may request. The user sees these at launch and the
/// bridge checks them on every RPC call.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    /// Read the current user's pubkey + display name.
    UserRead,
    /// Read the currently-selected channel's id + name.
    ChannelRead,
    /// Post a message to the current channel **as the user** (clearly attributed).
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

/// A bundled activity. For this pass activities are local + allowlisted (no
/// arbitrary remote URLs): the HTML is embedded and loaded via `srcdoc`, so the
/// iframe gets an opaque origin and the strongest sandbox.
pub struct ActivityDef {
    pub id: &'static str,
    pub name: &'static str,
    pub icon: &'static str,
    pub caps: &'static [Capability],
    pub html: &'static str,
}

/// The allowlist of bundled activities.
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

/// Host surface: a floating launcher, a consent step, the sandboxed window, and
/// the always-on RPC bridge. Mount once in the workspace.
#[component]
pub fn ActivityHost() -> Element {
    let state = use_app_state();
    let gateway = use_gateway();

    let mut picker_open = use_signal(|| false);
    let mut consenting = use_signal(|| None::<usize>);
    // The launched activity *and the channel it was launched from*. Bound at
    // launch rather than read per call: an activity that posts "where the user
    // is now" misdirects every share the moment they change channel, and the
    // person who opened it has no reason to expect that.
    let mut launched = use_signal(|| None::<Launched>);

    use_future(move || {
        let gateway = gateway.clone();
        async move {
            let mut eval = document::eval(BRIDGE_JS);
            loop {
                let Ok(msg) = eval.recv::<Value>().await else {
                    break;
                };
                let open = *launched.peek();
                let def = open.as_ref().and_then(|l| ACTIVITIES.get(l.idx));
                let req_id = msg.get("reqId").cloned().unwrap_or(Value::Null);
                let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
                let params = msg.get("params").cloned().unwrap_or(Value::Null);

                let (ok, payload) = match def {
                    Some(def) => handle_rpc(
                        method,
                        &params,
                        def,
                        open.as_ref().and_then(|l| l.channel),
                        &state,
                        &gateway,
                    ),
                    None => (false, json!("no activity is open")),
                };
                let reply = if ok {
                    json!({ "__dxf_reply": req_id, "ok": true, "data": payload })
                } else {
                    json!({ "__dxf_reply": req_id, "ok": false, "error": payload })
                };
                let _ = document::eval(&format!(
                    "var f=document.getElementById('dxf-activity-frame');\
                 if(f&&f.contentWindow){{f.contentWindow.postMessage({}, '*');}}",
                    reply
                ));
            }
        }
    });

    rsx! {
        button {
            class: "fixed bottom-3 left-3 z-40 border border-[var(--border)] rounded px-3 py-1 text-[10px] uppercase tracking-wider bg-[var(--panel)] hover:border-[var(--accent)] text-[var(--text-muted)] hover:text-[var(--accent)] transition-colors",
            onclick: move |_| picker_open.set(!picker_open()),
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
                                    let channel = state.read().selected_channel;
                                    launched.set(Some(Launched { idx: i, channel }));
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

        if let Some(idx) = launched().map(|l| l.idx) {
            if let Some(def) = ACTIVITIES.get(idx) {
                ActivityWindow { def_idx: idx, name: def.name, icon: def.icon, html: def.html,
                    on_close: move |_| launched.set(None),
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
                "Runs sandboxed in your client. It can't read your other channels, files, or keys."
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
enum Drag {
    Move { dx: f64, dy: f64 },
    Resize { px: f64, py: f64, w0: f64, h0: f64 },
}

/// Draggable / resizable window hosting the sandboxed iframe. Same interaction
/// model as the screen-share viewer.
#[component]
fn ActivityWindow(
    def_idx: usize,
    name: &'static str,
    icon: &'static str,
    html: &'static str,
    on_close: EventHandler<()>,
) -> Element {
    let _ = def_idx;
    let mut x = use_signal(|| 220.0_f64);
    let mut y = use_signal(|| 120.0_f64);
    let mut w = use_signal(|| 420.0_f64);
    let mut h = use_signal(|| 460.0_f64);
    let mut drag = use_signal(|| None::<Drag>);

    rsx! {
        if drag().is_some() {
            div {
                class: "fixed inset-0 z-50",
                onmousemove: move |e| {
                    let c = e.client_coordinates();
                    match drag() {
                        Some(Drag::Move { dx, dy }) => { x.set(c.x - dx); y.set(c.y - dy); }
                        Some(Drag::Resize { px, py, w0, h0 }) => {
                            w.set((w0 + (c.x - px)).max(300.0));
                            h.set((h0 + (c.y - py)).max(260.0));
                        }
                        None => {}
                    }
                },
                onmouseup: move |_| drag.set(None),
            }
        }
        div {
            class: "fixed z-40 flex flex-col bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg shadow-2xl overflow-hidden dxf-modal-in",
            style: "left: {x}px; top: {y}px; width: {w}px; height: {h}px;",
            div {
                class: "h-9 px-3 flex items-center gap-2 border-b border-[var(--border)] shrink-0 cursor-move select-none",
                onmousedown: move |e| {
                    let c = e.client_coordinates();
                    drag.set(Some(Drag::Move { dx: c.x - x(), dy: c.y - y() }));
                },
                span { class: "text-sm", "{icon}" }
                span { class: "text-sm text-[var(--text)] font-medium truncate flex-1", "{name}" }
                span { class: "text-[9px] uppercase tracking-wider text-[var(--text-dim)]", "Sandboxed" }
                button {
                    class: "text-[var(--text-dim)] hover:text-[var(--text)] text-lg leading-none ml-1",
                    onmousedown: move |e| e.stop_propagation(),
                    onclick: move |_| on_close.call(()),
                    "✕"
                }
            }
            // Omit allow-same-origin to force an opaque origin, isolating the
            // sandbox from the host except via postMessage.
            iframe {
                id: "dxf-activity-frame",
                class: "flex-1 min-h-0 w-full bg-white border-0",
                "sandbox": "allow-scripts",
                "srcdoc": "{html}",
            }
            div {
                class: "absolute bottom-0 right-0 w-4 h-4 cursor-nwse-resize",
                style: "background: linear-gradient(135deg, transparent 0 50%, var(--border-strong) 50% 100%);",
                onmousedown: move |e| {
                    e.stop_propagation();
                    let c = e.client_coordinates();
                    drag.set(Some(Drag::Resize { px: c.x, py: c.y, w0: w(), h0: h() }));
                },
            }
        }
    }
}

/// A launched activity: which one, and the channel it was opened from.
///
/// The channel is captured at launch because that is what the person opening it
/// was looking at. Resolving it per call instead means an activity that posts a
/// result lands wherever the user happens to have navigated by the time they
/// click — a misdirection nobody is told about.
#[derive(Clone, Copy, PartialEq)]
struct Launched {
    idx: usize,
    channel: Option<Id>,
}

/// Capability-checked RPC dispatch. Returns `(ok, payload)`.
fn handle_rpc(
    method: &str,
    params: &Value,
    def: &ActivityDef,
    // Bound to the launch channel, not the user's current selection.
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
                    gateway.send(ClientMessage::SendMessage {
                        channel_id,
                        content,
                        image: None,
                        // Activities post on their own behalf; they have no
                        // concept of replying to a message.
                        reply_to: None,
                    });
                    (true, json!({ "sent": true }))
                }
                None => (false, json!("no channel is open")),
            }
        }
        _ => (false, json!("permission denied or unknown method")),
    }
}

/// Installed once: forwards every message from the activity iframe to Rust.
///
/// The listener is registered once per webview; the *sink* it calls is
/// reassigned on every eval. Only the second half survives a remount — a
/// listener closes over the `dioxus.send` of the eval that made it, and this
/// component is rebuilt whenever the session is, so guarding registration alone
/// leaves the live channel unregistered and the registered one dead. Same shape
/// as `features::chat`'s drop handler, and the same bug the camera pump had.
const BRIDGE_JS: &str = r#"
window.__dxfActivitySink = function (m) { try { dioxus.send(m); } catch (err) {} };
if (!window.__dxfActivityWired) {
  window.__dxfActivityWired = true;
  window.addEventListener('message', function (e) {
    var f = document.getElementById('dxf-activity-frame');
    if (!f || e.source !== f.contentWindow) return;
    var d = e.data;
    if (!d || d.__dxf !== true) return;
    if (window.__dxfActivitySink) window.__dxfActivitySink(d);
  });
}
"#;

/// The bundled "Dice Roller" activity. A self-contained page with an inline SDK
/// shim that talks to the host over postMessage. Demonstrates a capability read
/// (`user.get`) and a gated action (`message.send`).
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
    const dxf = (function () {
      let n = 0; const pending = {};
      window.addEventListener('message', function (e) {
        const d = e.data;
        if (!d || d.__dxf_reply === undefined) return;
        const p = pending[d.__dxf_reply]; if (!p) return;
        delete pending[d.__dxf_reply];
        d.ok ? p.resolve(d.data) : p.reject(d.error);
      });
      function call(method, params) {
        return new Promise(function (resolve, reject) {
          const reqId = ++n; pending[reqId] = { resolve: resolve, reject: reject };
          parent.postMessage({ __dxf: true, reqId: reqId, method: method, params: params || {} }, '*');
        });
      }
      return {
        getUser: function () { return call('user.get'); },
        getChannel: function () { return call('channel.get'); },
        sendMessage: function (content) { return call('message.send', { content: content }); }
      };
    })();

    const dieEl = document.getElementById('die');
    const rollBtn = document.getElementById('roll');
    const shareBtn = document.getElementById('share');
    const whoEl = document.getElementById('who');
    const whereEl = document.getElementById('where');
    const faces = ['⚀','⚁','⚂','⚃','⚄','⚅'];
    let last = 0;

    dxf.getUser().then(function (u) { whoEl.textContent = 'Hi, ' + u.username; })
       .catch(function () { whoEl.textContent = ''; });

    // Read once is sufficient because the activity is bound to its launch
    // channel, preventing stale state.
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
