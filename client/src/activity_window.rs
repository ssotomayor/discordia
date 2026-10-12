//! Third-party activity code never runs in the main webview: a frame there can
//! post Dioxus IPC directly (#304). Each activity gets a bare wry window here.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use dioxus::desktop::LogicalSize;
use dioxus::desktop::tao::event::{Event, WindowEvent};
use dioxus::desktop::tao::event_loop::EventLoopWindowTarget;
use dioxus::desktop::tao::window::{Window, WindowBuilder};
use dioxus::desktop::wry::http::{Request, Response};
use dioxus::desktop::wry::{self, NewWindowResponse, WebContext, WebView, WebViewBuilder};
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

const SCHEME: &str = "dxapp";
const MAX_CALL_BYTES: usize = 64 * 1024;

pub struct BundleFile {
    pub mime: String,
    pub bytes: Arc<[u8]>,
}

pub struct Bundle {
    pub files: HashMap<String, BundleFile>,
}

impl Bundle {
    pub fn single_page(html: &str) -> Self {
        let mut files = HashMap::new();
        files.insert(
            "index.html".to_string(),
            BundleFile {
                mime: "text/html; charset=utf-8".into(),
                bytes: Arc::from(html.as_bytes()),
            },
        );
        Bundle { files }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum FromActivity {
    Call {
        id: u64,
        method: String,
        params: Value,
    },
    Closed,
    Failed(String),
}

pub struct OpenSpec {
    pub key: u64,
    pub title: String,
    /// Lowercase hex, the bundle's hash. It is the page's origin, so two
    /// activities never share storage.
    pub host: String,
    pub bundle: Arc<Bundle>,
    pub tx: UnboundedSender<FromActivity>,
}

struct Open {
    // Field order is drop order: the webview goes before its context and window.
    webview: WebView,
    _context: WebContext,
    window: Window,
    tx: UnboundedSender<FromActivity>,
}

thread_local! {
    static PENDING: RefCell<Vec<OpenSpec>> = const { RefCell::new(Vec::new()) };
    static OPEN: RefCell<HashMap<u64, Open>> = RefCell::new(HashMap::new());
}

/// Opening needs the event loop's window target, which only `on_event` holds,
/// so the request waits for the next event — the same loop turn as the click.
pub fn open(spec: OpenSpec) {
    close(spec.key);
    PENDING.with(|p| p.borrow_mut().push(spec));
}

pub fn close(key: u64) {
    PENDING.with(|p| p.borrow_mut().retain(|s| s.key != key));
    let gone = OPEN.with(|o| o.borrow_mut().remove(&key));
    drop(gone);
}

pub fn is_open(key: u64) -> bool {
    OPEN.with(|o| o.borrow().contains_key(&key))
}

pub fn focus(key: u64) {
    OPEN.with(|o| {
        if let Some(open) = o.borrow().get(&key) {
            open.window.set_focus();
        }
    });
}

pub fn reply(key: u64, id: u64, result: Result<Value, String>) {
    let (ok, value) = match result {
        Ok(v) => (true, v),
        Err(e) => (false, Value::String(e)),
    };
    evaluate(key, &format!("window.__dxfReply({id},{ok},{value})"));
}

pub fn emit(key: u64, event: &str, data: &Value) {
    let name = Value::String(event.to_string());
    evaluate(key, &format!("window.__dxfEvent({name},{data})"));
}

fn evaluate(key: u64, js: &str) {
    OPEN.with(|o| {
        if let Some(open) = o.borrow().get(&key)
            && let Err(e) = open.webview.evaluate_script(js)
        {
            tracing::warn!(error = %e, "activity window script failed");
        }
    });
}

/// Installed as the app's custom event handler: the scoped wry handlers only
/// see Dioxus's own windows, never these.
pub fn on_event<T>(event: &Event<T>, target: &EventLoopWindowTarget<T>) {
    let pending = PENDING.with(|p| std::mem::take(&mut *p.borrow_mut()));
    for spec in pending {
        let (key, tx) = (spec.key, spec.tx.clone());
        match build(spec, target) {
            Ok(open) => {
                OPEN.with(|o| o.borrow_mut().insert(key, open));
            }
            Err(e) => {
                tracing::warn!(error = %e, "activity window failed to open");
                let _ = tx.send(FromActivity::Failed(e));
            }
        }
    }

    if let Event::WindowEvent {
        window_id, event, ..
    } = event
    {
        match event {
            WindowEvent::CloseRequested | WindowEvent::Destroyed => {
                let gone = OPEN.with(|o| {
                    let mut o = o.borrow_mut();
                    let key = o
                        .iter()
                        .find(|(_, w)| w.window.id() == *window_id)
                        .map(|(k, _)| *k)?;
                    o.remove(&key)
                });
                if let Some(open) = gone {
                    let _ = open.tx.send(FromActivity::Closed);
                }
            }
            WindowEvent::Resized(size) => OPEN.with(|o| {
                if let Some(open) = o.borrow().values().find(|w| w.window.id() == *window_id) {
                    let _ = open.webview.set_bounds(wry::Rect {
                        position: wry::dpi::LogicalPosition::new(0.0, 0.0).into(),
                        size: wry::dpi::PhysicalSize::new(size.width, size.height).into(),
                    });
                }
            }),
            _ => {}
        }
    }
}

pub fn base_url(host: &str) -> String {
    if cfg!(windows) {
        format!("http://{SCHEME}.{host}/")
    } else {
        format!("{SCHEME}://{host}/")
    }
}

pub fn valid_host(host: &str) -> bool {
    (16..=64).contains(&host.len())
        && host
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The policy is the network wall: nothing but the activity's own origin, and
/// a frame it creates inherits it.
pub fn content_policy(host: &str) -> String {
    let origin = base_url(host);
    let origin = origin.trim_end_matches('/');
    let own = format!("'self' {origin}");
    format!(
        "default-src 'none'; \
         script-src {own} 'unsafe-inline' 'unsafe-eval' blob:; \
         style-src {own} 'unsafe-inline'; \
         img-src {own} data: blob:; \
         font-src {own} data:; \
         media-src {own} data: blob:; \
         worker-src {own} blob:; \
         connect-src {own}; \
         frame-src 'none'; child-src 'none'; object-src 'none'; \
         form-action 'none'; base-uri 'none'"
    )
}

/// Only the activity's own page may call: a frame it made, or anything that
/// navigated away, is refused even though the handler is visible to it.
pub fn call_allowed(uri: &str, host: &str) -> bool {
    uri.starts_with(&base_url(host))
}

pub fn parse_call(body: &str) -> Option<FromActivity> {
    if body.len() > MAX_CALL_BYTES {
        return None;
    }
    let v: Value = serde_json::from_str(body).ok()?;
    Some(FromActivity::Call {
        id: v.get("id")?.as_u64()?,
        method: v.get("method")?.as_str()?.to_string(),
        params: v.get("params").cloned().unwrap_or(Value::Null),
    })
}

pub fn serve(bundle: &Bundle, host: &str, path: &str) -> Response<Cow<'static, [u8]>> {
    let path = path.trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    let Some(file) = bundle.files.get(path) else {
        return Response::builder()
            .status(404)
            .header("Content-Security-Policy", content_policy(host))
            .body(Cow::Borrowed(&b""[..]))
            .expect("static response");
    };
    Response::builder()
        .status(200)
        .header("Content-Type", file.mime.as_str())
        .header("Content-Security-Policy", content_policy(host))
        .header("X-Content-Type-Options", "nosniff")
        .header("Cache-Control", "no-store")
        .body(Cow::Owned(file.bytes.to_vec()))
        .expect("static response")
}

fn build<T>(spec: OpenSpec, target: &EventLoopWindowTarget<T>) -> Result<Open, String> {
    if !valid_host(&spec.host) {
        return Err("activity has no valid bundle hash".into());
    }
    let window = WindowBuilder::new()
        .with_title(&spec.title)
        .with_inner_size(LogicalSize::new(480.0, 560.0))
        .with_min_inner_size(LogicalSize::new(300.0, 260.0))
        .build(target)
        .map_err(|e| format!("window: {e}"))?;

    let data_dir = crate::identity::config_dir()
        .join("activities")
        .join(&spec.host);
    let mut context = WebContext::new(Some(data_dir));
    let base = base_url(&spec.host);

    let bundle = spec.bundle.clone();
    let host = spec.host.clone();
    let serve_bundle =
        move |_id: wry::WebViewId, req: Request<Vec<u8>>| serve(&bundle, &host, req.uri().path());
    let host = spec.host.clone();
    let tx = spec.tx.clone();
    let ipc = move |req: Request<String>| {
        if !call_allowed(&req.uri().to_string(), &host) {
            tracing::warn!(uri = %req.uri(), "activity call from a foreign frame refused");
            return;
        }
        if let Some(call) = parse_call(req.body()) {
            let _ = tx.send(call);
        }
    };
    let nav_base = base.clone();

    let builder = WebViewBuilder::new_with_web_context(&mut context)
        .with_url(format!("{base}index.html"))
        .with_custom_protocol(SCHEME.to_string(), serve_bundle)
        .with_initialization_script(HARDEN_JS)
        .with_initialization_script_for_main_only(SDK_JS, true)
        .with_ipc_handler(ipc)
        .with_navigation_handler(move |url| url.starts_with(&nav_base))
        .with_new_window_req_handler(|_, _| NewWindowResponse::Deny)
        .with_download_started_handler(|_, _| false)
        .with_clipboard(false)
        .with_back_forward_navigation_gestures(false)
        .with_devtools(cfg!(debug_assertions));

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let webview = builder.build(&window);
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let webview = {
        use dioxus::desktop::tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        let vbox = window.default_vbox().ok_or("window has no GTK box")?;
        builder.build_gtk(vbox)
    };
    let webview = webview.map_err(|e| format!("webview: {e}"))?;

    Ok(Open {
        webview,
        _context: context,
        window,
        tx: spec.tx,
    })
}

/// Runs in every frame before the page. CSP does not cover WebRTC, and the
/// main webview needs it for LiveKit, so it is removed here instead.
const HARDEN_JS: &str = r#"(function () {
  var gone = ['RTCPeerConnection', 'webkitRTCPeerConnection', 'RTCDataChannel',
    'RTCIceCandidate', 'RTCSessionDescription', 'RTCRtpSender', 'RTCRtpReceiver',
    'RTCRtpTransceiver', 'RTCDtlsTransport', 'RTCIceTransport', 'RTCSctpTransport',
    'RTCCertificate', 'WebTransport', 'Notification'];
  gone.forEach(function (k) {
    try { delete window[k]; } catch (e) {}
    try { Object.defineProperty(window, k, { value: undefined, configurable: false, writable: false }); } catch (e) {}
  });
  ['mediaDevices', 'geolocation', 'credentials', 'serviceWorker'].forEach(function (k) {
    try { Object.defineProperty(navigator, k, { value: undefined, configurable: false }); } catch (e) {}
  });
})();"#;

const SDK_JS: &str = r#"(function () {
  if (window.discordia) return;
  var pending = {}, next = 0, listeners = {};
  function call(method, params) {
    return new Promise(function (resolve, reject) {
      var id = ++next;
      pending[id] = [resolve, reject];
      window.ipc.postMessage(JSON.stringify({ id: id, method: String(method), params: params || {} }));
    });
  }
  function on(name, f) {
    (listeners[name] = listeners[name] || []).push(f);
    return function () { listeners[name] = (listeners[name] || []).filter(function (g) { return g !== f; }); };
  }
  Object.defineProperty(window, '__dxfReply', { value: function (id, ok, value) {
    var p = pending[id]; if (!p) return; delete pending[id]; (ok ? p[0] : p[1])(value);
  } });
  Object.defineProperty(window, '__dxfEvent', { value: function (name, data) {
    (listeners[name] || []).slice().forEach(function (f) { try { f(data); } catch (e) {} });
  } });
  Object.defineProperty(window, 'discordia', { value: Object.freeze({ call: call, on: on }) });
})();"#;

#[cfg(test)]
mod tests {
    use super::*;

    const HOST: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn only_the_activitys_own_page_may_call() {
        let own = format!("{}index.html", base_url(HOST));
        assert!(call_allowed(&own, HOST));
        assert!(!call_allowed("about:srcdoc", HOST));
        assert!(!call_allowed("about:blank", HOST));
        assert!(!call_allowed("dioxus://index.html/", HOST));
        assert!(!call_allowed(
            &base_url("ffffffffffffffffffffffffffffffff"),
            HOST
        ));
        assert!(!call_allowed(
            &format!("{}.evil/", base_url(HOST).trim_end_matches('/')),
            HOST
        ));
    }

    #[test]
    fn a_host_is_a_hash_and_nothing_else() {
        assert!(valid_host(HOST));
        assert!(!valid_host("ABCDEF0123456789ABCDEF0123456789"));
        assert!(!valid_host("evil.com"));
        assert!(!valid_host("0123"));
        assert!(!valid_host("0123456789abcdef0123456789abcdef/../x"));
    }

    #[test]
    fn the_policy_allows_no_network_but_the_activity_itself() {
        let csp = content_policy(HOST);
        assert!(csp.starts_with("default-src 'none';"));
        let connect = csp
            .split(';')
            .map(str::trim)
            .find(|d| d.starts_with("connect-src"))
            .unwrap();
        assert_eq!(
            connect,
            format!(
                "connect-src 'self' {}",
                base_url(HOST).trim_end_matches('/')
            )
        );
        for d in [
            "frame-src 'none'",
            "form-action 'none'",
            "object-src 'none'",
        ] {
            assert!(csp.contains(d), "{d}");
        }
        for token in csp.split([' ', ';']) {
            assert!(
                !matches!(token, "http:" | "https:" | "ws:" | "wss:" | "*"),
                "{token}"
            );
        }
    }

    #[test]
    fn every_response_carries_the_policy() {
        let bundle = Bundle::single_page("<p>hi</p>");
        for path in ["/", "/index.html", "/missing.js"] {
            let r = serve(&bundle, HOST, path);
            assert_eq!(
                r.headers()["Content-Security-Policy"].to_str().unwrap(),
                content_policy(HOST)
            );
        }
        assert_eq!(serve(&bundle, HOST, "/").body().as_ref(), b"<p>hi</p>");
        assert_eq!(serve(&bundle, HOST, "/missing.js").status(), 404);
    }

    #[test]
    fn calls_are_parsed_strictly() {
        assert_eq!(
            parse_call(r#"{"id":3,"method":"user.get","params":{}}"#),
            Some(FromActivity::Call {
                id: 3,
                method: "user.get".into(),
                params: serde_json::json!({})
            })
        );
        assert_eq!(parse_call(r#"{"method":"user.get"}"#), None);
        assert_eq!(parse_call(r#"{"id":-1,"method":"x"}"#), None);
        assert_eq!(parse_call("not json"), None);
        let huge = format!(
            r#"{{"id":1,"method":"x","params":"{}"}}"#,
            "a".repeat(MAX_CALL_BYTES)
        );
        assert_eq!(parse_call(&huge), None);
    }
}
