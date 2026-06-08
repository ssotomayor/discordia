//! Self-host launcher: brings up an embedded `dioxusfun-server` and the
//! bundled `livekit-server` subprocess so the user can run the whole stack on
//! their own machine without external dependencies.
//!
//! The LiveKit binary itself is bundled by `dioxusfun-server`'s build script
//! and re-exported via `dioxusfun_server::livekit_bundle`, so client and
//! server share the same baked-in copy.

use std::net::{IpAddr, SocketAddr};

use dioxusfun_server::ServerHandle;
use dioxusfun_server::livekit::LiveKitConfig;
use dioxusfun_server::livekit_bundle::{
    self, DEFAULT_LIVEKIT_KEY, DEFAULT_LIVEKIT_PORT, DEFAULT_LIVEKIT_SECRET, LivekitSubprocess,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostInfo {
    pub gateway_addr: SocketAddr,
    pub livekit_url: String,
    pub lan_url: String,
    pub local_url: String,
    pub voice_bundled: bool,
    /// Set when self-host registered with a rendezvous; friends can join
    /// with this code instead of a URL.
    pub shortcode: Option<String>,
}

pub struct HostHandle {
    pub info: HostInfo,
    gateway: Option<ServerHandle>,
    livekit: Option<LivekitSubprocess>,
    rendezvous_task: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for HostHandle {
    fn drop(&mut self) {
        if let Some(handle) = self.gateway.take() {
            handle.abort();
        }
        if let Some(task) = self.rendezvous_task.take() {
            task.abort();
        }
        // LivekitSubprocess Drop triggers `kill_on_drop` on the child.
        self.livekit = None;
    }
}

pub async fn start_self_host(
    allow_lan: bool,
    rendezvous_url: Option<String>,
    publish: crate::rendezvous::PublishOptions,
) -> Result<HostHandle, String> {
    let (livekit, voice_bundled) = match livekit_bundle::spawn_livekit().await {
        Ok(child) => {
            eprintln!("[host] livekit ready at ws://127.0.0.1:{DEFAULT_LIVEKIT_PORT}");
            (Some(child), true)
        }
        Err(e) => {
            eprintln!("[host] livekit unavailable: {e}");
            tracing::warn!(error = %e, "self-host voice unavailable");
            (None, false)
        }
    };

    // Register with rendezvous first so we know which LiveKit URL to hand
    // to clients. If the rendezvous operator runs a shared LiveKit, that
    // URL takes precedence over our local subprocess.
    let mut rendezvous_state: Option<(crate::rendezvous::ControlStream, crate::rendezvous::PublishInfo)> = None;
    if let Some(url) = rendezvous_url {
        match crate::rendezvous::register(&url, publish).await {
            Ok((info, control)) => {
                eprintln!(
                    "[host] rendezvous registered: shortcode={} livekit_url={:?}",
                    info.shortcode, info.livekit_url
                );
                rendezvous_state = Some((control, info));
            }
            Err(e) => {
                eprintln!("[host] rendezvous registration failed: {e}");
                tracing::warn!(error = %e, "rendezvous publish failed");
            }
        }
    }

    let livekit_cfg = LiveKitConfig {
        // If rendezvous handed us a shared LiveKit URL, pin it. Otherwise
        // gateway derives from per-connection client Host header.
        explicit_url: rendezvous_state
            .as_ref()
            .and_then(|(_, info)| info.livekit_url.clone()),
        port: DEFAULT_LIVEKIT_PORT,
        api_key: DEFAULT_LIVEKIT_KEY.into(),
        api_secret: DEFAULT_LIVEKIT_SECRET.into(),
    };

    let bind_ip: IpAddr = if allow_lan {
        "0.0.0.0".parse().unwrap()
    } else {
        "127.0.0.1".parse().unwrap()
    };
    let preferred = SocketAddr::new(bind_ip, 9000);

    let gateway = dioxusfun_server::spawn(preferred, 20, livekit_cfg)
        .await
        .map_err(|e| format!("embedded server: {e}"))?;
    let gateway_addr = gateway.addr;
    let local_url = format!("ws://127.0.0.1:{}", gateway_addr.port());
    let lan_url = lan_url_for(gateway_addr.port()).unwrap_or_else(|| local_url.clone());

    let (shortcode, rendezvous_task) = match rendezvous_state {
        Some((control, info)) => {
            let task = crate::rendezvous::run_adapter(
                control,
                info.rendezvous_base.clone(),
                SocketAddr::new("127.0.0.1".parse().unwrap(), gateway_addr.port()),
            );
            (Some(info.shortcode), Some(task))
        }
        None => (None, None),
    };

    let livekit_display = if voice_bundled {
        format!("ws://127.0.0.1:{DEFAULT_LIVEKIT_PORT}")
    } else {
        String::new()
    };

    Ok(HostHandle {
        info: HostInfo {
            gateway_addr,
            livekit_url: livekit_display,
            lan_url,
            local_url,
            voice_bundled,
            shortcode,
        },
        gateway: Some(gateway),
        livekit,
        rendezvous_task,
    })
}

fn lan_url_for(port: u16) -> Option<String> {
    let ip = local_ip_address::local_ip().ok()?;
    if matches!(ip, IpAddr::V4(v4) if v4.is_loopback() || v4.is_unspecified()) {
        return None;
    }
    Some(format!("ws://{ip}:{port}"))
}
