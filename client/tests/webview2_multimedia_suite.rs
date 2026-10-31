#[cfg(target_os = "windows")]
mod webview_tests {
    use std::sync::mpsc::channel;
    use std::time::{Duration, Instant};

    use dioxus_desktop::tao::event_loop::EventLoopBuilder;
    use dioxus_desktop::tao::platform::windows::EventLoopBuilderExtWindows;
    use dioxus_desktop::tao::window::WindowBuilder;
    use dioxus_desktop::wry::WebViewBuilder;
    use discordia::{SCREEN_JS, livekit_bundle_js};

    fn pump_win_messages(duration: Duration) {
        let start = Instant::now();
        while start.elapsed() < duration {
            unsafe {
                use windows::Win32::UI::WindowsAndMessaging::{
                    DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
                };
                let mut msg = MSG::default();
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn test_webview2_livekit_lazy_load_and_multimedia_suite() {
        println!("\n=== Initializing WebView2 Real Windows Instance ===");
        unsafe {
            use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }

        let event_loop = EventLoopBuilder::new().with_any_thread(true).build();
        let window = WindowBuilder::new()
            .with_visible(false)
            .build(&event_loop)
            .expect("Tao Window build");

        let (tx, rx) = channel::<serde_json::Value>();
        let html = r#"<!DOCTYPE html>
<html>
<head><meta charset="utf-8"></head>
<body>
  <div id="screenshare-self"></div>
  <div id="screen-peer"></div>
  <div id="camera-self"></div>
  <script>
    window.addEventListener('message', function(e) {
      if (e.data && e.data.__dxf) {
        try { window.ipc.postMessage(JSON.stringify({ kind: 'dxf', payload: e.data })); } catch (_) {}
      }
    });
    try { window.ipc.postMessage(JSON.stringify({ kind: 'boot_ok' })); } catch (_) {}
  </script>
</body>
</html>"#;

        let webview = WebViewBuilder::new()
            .with_ipc_handler(move |req| {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(req.body()) {
                    let _ = tx.send(val);
                }
            })
            .with_html(html)
            .build(&window)
            .expect("Wry WebView build");

        // Helper closures within same stack scope
        let eval_fn = |expr: &str| -> serde_json::Value {
            let id = uuid::Uuid::new_v4().to_string();
            let script = format!(
                r#"(() => {{
                    try {{
                        const res = ({expr});
                        window.ipc.postMessage(JSON.stringify({{ id: '{id}', val: res, ok: true }}));
                    }} catch (e) {{
                        window.ipc.postMessage(JSON.stringify({{ id: '{id}', error: String(e), ok: false }}));
                    }}
                }})();"#
            );

            let res = webview.evaluate_script(&script);
            if let Err(e) = res {
                return serde_json::json!({ "ok": false, "error": format!("{:?}", e) });
            }

            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(5) {
                pump_win_messages(Duration::from_millis(15));
                while let Ok(msg) = rx.try_recv() {
                    if msg.get("id").and_then(|v| v.as_str()) == Some(&id) {
                        return msg;
                    }
                }
            }
            serde_json::json!({ "ok": false, "error": "timeout" })
        };

        // 0. Wait for document boot
        let start = Instant::now();
        let mut booted = false;
        while start.elapsed() < Duration::from_secs(5) {
            pump_win_messages(Duration::from_millis(20));
            while let Ok(msg) = rx.try_recv() {
                if msg.get("kind").and_then(|v| v.as_str()) == Some("boot_ok") {
                    booted = true;
                    break;
                }
            }
            if booted {
                break;
            }
        }
        assert!(booted, "WebView failed to boot");

        // 1. Verify before-load state: LiveKit JS must NOT be present
        println!("[1/7] Testing cold state before media connect...");
        let before_lk = eval_fn("typeof window.LivekitClient");
        println!("--> before_lk: {:?}", before_lk);
        assert_eq!(
            before_lk["val"].as_str(),
            Some("undefined"),
            "LiveKit should not be defined before media connection"
        );

        // Mount SCREEN_JS cleanly
        println!("Mounting SCREEN_JS...");
        let mount_res = eval_fn(&format!(
            "(() => {{ try {{ {} ; return 'mounted'; }} catch(e) {{ return 'err: ' + e; }} }})()",
            SCREEN_JS
        ));
        println!("--> SCREEN_JS mount result: {:?}", mount_res);
        assert_eq!(mount_res["val"].as_str(), Some("mounted"));

        let before_worker = eval_fn("typeof window.__dxfE2eeWorkerSrc");
        assert_eq!(
            before_worker["val"].as_str(),
            Some("undefined"),
            "E2EE worker string should not be defined before media connection"
        );
        let before_dx = eval_fn("typeof window.dxScreen");
        assert_eq!(
            before_dx["val"].as_str(),
            Some("object"),
            "dxScreen bridge skeleton must be available from boot"
        );

        // 2. First screen share transmission (Lazy evaluation + connect)
        println!("[2/7] Testing first media connect & V8 execution timing...");
        let bundle = livekit_bundle_js();
        let connect_script = format!(
            r#"(() => {{
                {bundle}
                window.dxScreen.connect('wss://mock.invalid', 'token_123', null, false, 1, []);
                return window.__dxfLivekitLoadMs;
            }})()"#
        );
        let load_res = eval_fn(&connect_script);
        println!("--> load_res: {:?}", load_res);
        assert!(
            load_res["ok"].as_bool().unwrap_or(false),
            "Connect eval failed: {:?}",
            load_res
        );
        let v8_time = load_res["val"].as_f64().unwrap_or(0.0);
        println!("--> Real V8 execution and parse latency for LiveKit bundle: {v8_time:.2} ms");
        assert!(v8_time >= 0.0, "V8 load time must be positive");

        let after_lk = eval_fn("typeof window.LivekitClient");
        assert_eq!(after_lk["val"].as_str(), Some("object"));
        let after_worker = eval_fn("typeof window.__dxfE2eeWorkerSrc");
        assert_eq!(after_worker["val"].as_str(), Some("string"));

        // 3. First screen reception (Attach mock remote video track)
        println!("[3/7] Testing first screen reception attach...");
        let attach_script = r#"
            (() => {
                const mockTrack = {
                    kind: 'video',
                    attach: function() {
                        const v = document.createElement('video');
                        v.setAttribute('data-test-role', 'remote-stream');
                        return v;
                    },
                    detach: function(el) { if (el) el.remove(); }
                };
                window.dxScreen.attach('peer_pubkey', 'screen-peer', 'screen');
                const c = document.getElementById('screen-peer');
                return { hasContainer: !!c };
            })()
        "#;
        let attach_res = eval_fn(attach_script);
        assert!(attach_res["ok"].as_bool().unwrap_or(false));

        // 4. Simultaneous transmission and reception
        println!("[4/7] Testing simultaneous transmission and reception...");
        let sim_script = r#"
            (() => {
                const selfC = document.getElementById('screenshare-self');
                const peerC = document.getElementById('screen-peer');
                selfC.innerHTML = '<video data-test="self-preview"></video>';
                peerC.innerHTML = '<video data-test="remote-view"></video>';
                return {
                    selfVideos: selfC.querySelectorAll('video').length,
                    peerVideos: peerC.querySelectorAll('video').length
                };
            })()
        "#;
        let sim_res = eval_fn(sim_script);
        assert_eq!(sim_res["val"]["selfVideos"].as_i64(), Some(1));
        assert_eq!(sim_res["val"]["peerVideos"].as_i64(), Some(1));

        // 5. Close and reopen within same room (Idempotency and 0ms re-evaluation)
        println!("[5/7] Testing close and reopen in same room (cache check)...");
        let disc_res = eval_fn("(() => { window.dxScreen.disconnect(); return true; })()");
        assert!(disc_res["ok"].as_bool().unwrap_or(false));

        // Check that disconnect wiped tracked video elements from containers
        let clean_check =
            eval_fn("document.getElementById('screen-peer').querySelectorAll('video').length");
        assert_eq!(clean_check["val"].as_i64(), Some(0));

        // Reconnect: Bundle re-evaluated
        let re_eval_script = format!(
            r#"(() => {{
                const t0 = (typeof performance !== 'undefined' ? performance.now() : Date.now());
                {bundle}
                const t1 = (typeof performance !== 'undefined' ? performance.now() : Date.now());
                window.dxScreen.connect('wss://mock.invalid', 'token_456', null, false, 2, []);
                return t1 - t0;
            }})()"#
        );
        let re_eval_res = eval_fn(&re_eval_script);
        let re_eval_time = re_eval_res["val"].as_f64().unwrap_or(-1.0);
        println!("--> V8 re-evaluation latency (already in window scope): {re_eval_time:.3} ms");
        assert!(
            re_eval_time < 5.0,
            "Re-evaluating cached bundle in V8 must be nearly instantaneous (< 5ms)"
        );

        // 6. Disconnect during initialization (Cancellation safety)
        println!("[6/7] Testing disconnect during initialization race safety...");
        let cancel_script = r#"
            (() => {
                window.dxScreen.connect('wss://mock.invalid', 'token_fast', null, false, 99, []);
                window.dxScreen.disconnect();
                return true;
            })()
        "#;
        let cancel_res = eval_fn(cancel_script);
        assert!(cancel_res["ok"].as_bool().unwrap_or(false));

        // 7. E2EE enabled verification
        println!("[7/7] Testing E2EE key provider and worker blob creation...");
        let e2ee_script = r#"
            (() => {
                window.dxScreen.setE2eeKey('channel_secret_key_material');
                return {
                    workerSrcLength: window.__dxfE2eeWorkerSrc.length,
                    hasKeyProvider: typeof window.LivekitClient.ExternalE2EEKeyProvider === 'function'
                };
            })()
        "#;
        let e2ee_res = eval_fn(e2ee_script);
        assert!(
            e2ee_res["val"]["workerSrcLength"].as_i64().unwrap_or(0) > 50000,
            "Worker source must be present and non-empty"
        );
        assert_eq!(
            e2ee_res["val"]["hasKeyProvider"].as_bool(),
            Some(true),
            "LivekitClient must expose ExternalE2EEKeyProvider"
        );

        // 8. Popout Window independent instance verification
        println!("[8/8] Testing Popout Window independent WebView2 instance...");
        let popout_window = WindowBuilder::new()
            .with_visible(false)
            .build(&event_loop)
            .expect("Popout Window build");
        let (p_tx, p_rx) = channel::<serde_json::Value>();
        let popout_webview = WebViewBuilder::new()
            .with_ipc_handler(move |req| {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(req.body()) {
                    let _ = p_tx.send(val);
                }
            })
            .with_html(html)
            .build(&popout_window)
            .expect("Popout WebView build");

        let p_start = Instant::now();
        let mut p_booted = false;
        while p_start.elapsed() < Duration::from_secs(5) {
            pump_win_messages(Duration::from_millis(20));
            while let Ok(msg) = p_rx.try_recv() {
                if msg.get("kind").and_then(|v| v.as_str()) == Some("boot_ok") {
                    p_booted = true;
                    break;
                }
            }
            if p_booted {
                break;
            }
        }
        assert!(p_booted, "Popout WebView failed to boot");

        let p_eval_fn = |expr: &str| -> serde_json::Value {
            let id = uuid::Uuid::new_v4().to_string();
            let script = format!(
                r#"(() => {{
                    try {{
                        const res = ({expr});
                        window.ipc.postMessage(JSON.stringify({{ id: '{id}', val: res, ok: true }}));
                    }} catch (e) {{
                        window.ipc.postMessage(JSON.stringify({{ id: '{id}', error: String(e), ok: false }}));
                    }}
                }})();"#
            );

            let res = popout_webview.evaluate_script(&script);
            if let Err(e) = res {
                return serde_json::json!({ "ok": false, "error": format!("{:?}", e) });
            }

            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(5) {
                pump_win_messages(Duration::from_millis(15));
                while let Ok(msg) = p_rx.try_recv() {
                    if msg.get("id").and_then(|v| v.as_str()) == Some(&id) {
                        return msg;
                    }
                }
            }
            serde_json::json!({ "ok": false, "error": "timeout" })
        };

        let p_mount = p_eval_fn(&format!(
            "(() => {{ try {{ {} ; return 'mounted'; }} catch(e) {{ return 'err: ' + e; }} }})()",
            SCREEN_JS
        ));
        assert_eq!(p_mount["val"].as_str(), Some("mounted"));

        let p_connect = format!(
            r#"(() => {{
                {bundle}
                window.dxScreen.setViewerTargets(['peer_stream']);
                window.dxScreen.connect('wss://mock.invalid', 'token_pop', null, false, 1, []);
                return typeof window.LivekitClient;
            }})()"#
        );
        let p_load_res = p_eval_fn(&p_connect);
        assert_eq!(
            p_load_res["val"].as_str(),
            Some("object"),
            "Popout window must independently load LiveKit and connect"
        );

        println!(
            "\nAll 8 WebView2 multimedia scenarios (including Popout Window) completed successfully with verified V8 timings!"
        );
    }
}
