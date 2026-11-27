//! Native WebView receiver check; stdin is supplied by the isolated music integration test.
use dioxus::prelude::*;
use serde_json::Value;
use std::{io::Read, sync::OnceLock};

static OPTIONS: OnceLock<String> = OnceLock::new();

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut input = String::new();
    std::io::stdin()
        .take(32 * 1024)
        .read_to_string(&mut input)?;
    let value: Value = serde_json::from_str(&input)?;
    let url = url::Url::parse(value["url"].as_str().ok_or("missing URL")?)?;
    if url.scheme() != "ws" || url.host_str() != Some("127.0.0.1") {
        return Err("probe only accepts an isolated loopback SFU".into());
    }
    OPTIONS
        .set(input)
        .map_err(|_| "probe already initialized")?;
    let config = dioxus::desktop::Config::new().with_window(
        dioxus::desktop::tao::window::WindowBuilder::new()
            .with_title("Music Lab · isolated WebView audio test")
            .with_inner_size(dioxus::desktop::tao::dpi::LogicalSize::new(600.0, 280.0)),
    );
    #[cfg(target_os="windows")]
    let config=config.with_windows_browser_args("--unsafely-treat-insecure-origin-as-secure=http://dioxus.index.html,http://dioxus.index.html/");
    LaunchBuilder::desktop().with_cfg(config).launch(App);
    Ok(())
}

#[component]
fn App() -> Element {
    let status = use_signal(|| "Ready: synthetic tones, muted output".to_string());
    use_effect(move || begin(status));
    rsx! {
        main { style: "font:18px sans-serif;padding:28px;background:#14191e;color:white;height:100vh",
            h2 { "Music Lab — native receiver check" }
            p { "{status}" }

        }
    }
}

fn begin(mut status: Signal<String>) {
    spawn(async move {
        let Some(options) = OPTIONS.get() else { return };
        let source = format!(
            "{}\n{}\n{}\nrunProbe({options});",
            discordia::livekit_bundle_js(),
            include_str!("../assets/activity-audio.js"),
            include_str!("activity_audio_probe.js")
        );
        let mut eval = document::eval(&source);
        while let Ok(value) = eval.recv::<Value>().await {
            if let Some(message) = value["status"].as_str() {
                println!("MUSIC_WEBVIEW_STATUS {message}");
                status.set(message.to_string());
            }
            if value["finished"] == true {
                if value["verified"] == true {
                    println!("MUSIC_WEBVIEW_PCM_VERIFIED");
                } else {
                    println!("MUSIC_WEBVIEW_PCM_FAILED {}", value["metrics"]);
                }
                status.set(
                    if value["verified"] == true {
                        "Verified decrypted PCM and receiver teardown"
                    } else {
                        "PCM verification failed"
                    }
                    .into(),
                );
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                dioxus::desktop::window().close();
                break;
            }
        }
    });
}
