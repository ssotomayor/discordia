//! Self-host launcher: brings up an embedded `dioxusfun-server` and the
//! bundled `livekit-server` subprocess so the user can run the whole stack on
//! their own machine without external dependencies.
//!
//! The LiveKit binary itself is bundled by `dioxusfun-server`'s build script
//! and re-exported via `dioxusfun_server::livekit_bundle`, so client and
//! server share the same baked-in copy.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use dioxusfun_server::ServerHandle;
use dioxusfun_server::livekit::LiveKitConfig;
use dioxusfun_server::livekit_bundle::{
    self, DEFAULT_LIVEKIT_KEY, DEFAULT_LIVEKIT_PORT, DEFAULT_LIVEKIT_SECRET,
    DEFAULT_LIVEKIT_TCP_PORT, DEFAULT_LIVEKIT_UDP_PORT, LivekitSubprocess,
};

use crate::portmap;

/// How far this host can be reached, and — when the answer is "not far" — why.
///
/// The three tiers in `docs/NETWORKING.md` collapse to this for the UI's
/// purposes: the point is that a host is never left to infer its own
/// reachability from a friend failing to connect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reachability {
    /// The gateway is bound to loopback, so nobody else can reach it at all —
    /// direct connections were not allowed when hosting started.
    LoopbackOnly,
    /// Reachable on this network, and no further. Carries the reason no public
    /// address was obtained.
    LanOnly { reason: String },
    /// A public address the internet can dial.
    Direct {
        endpoint: String,
        /// Which protocol got the mapping ("UPnP-IGD" / "NAT-PMP").
        method: &'static str,
        /// Whether voice is reachable there too, or only chat.
        media: bool,
    },
}

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
    /// Why rendezvous publishing failed, when it did. Surfaced in the host
    /// banner — a silent failure looks identical to "published" and leaves
    /// friends unable to find you.
    pub publish_error: Option<String>,
    /// True when the host asked to be listed in the public directory.
    pub listed_public: bool,
    /// What friends can reach, and why they can't reach more.
    pub reachability: Reachability,
}

pub struct HostHandle {
    pub info: HostInfo,
    gateway: Option<ServerHandle>,
    livekit: Option<LivekitSubprocess>,
    rendezvous_task: Option<tokio::task::JoinHandle<()>>,
    /// Holds the port mappings open; dropping it hands them back.
    _port_mapping: Option<portmap::MappingGuard>,
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
    // The host's own identity: its pubkey becomes the operator of the seeded
    // Lobby (so the person running the server can moderate it — it's their
    // machine), and it signs the rendezvous name-ownership proof.
    identity: crate::identity::Identity,
) -> Result<HostHandle, String> {
    let operator_pubkey = identity.pubkey.clone();

    // Bind before anything else, because the port is an input to everything
    // that follows: it is what we ask the router to forward, and what we
    // advertise to the rendezvous. `bind_with_fallback` may not give us the
    // port we asked for, so guessing it here would mean mapping the wrong one.
    // Serving starts further down, on this same listener.
    let bind_ip: IpAddr = if allow_lan {
        "0.0.0.0".parse().unwrap()
    } else {
        "127.0.0.1".parse().unwrap()
    };
    let listener = dioxusfun_server::bind_with_fallback(SocketAddr::new(bind_ip, 9000), 20)
        .await
        .map_err(|e| format!("embedded server: {e}"))?;
    let gateway_addr = listener
        .local_addr()
        .map_err(|e| format!("embedded server: {e}"))?;

    // Tier 1: ask the router for a way in. Attempted whether or not a
    // rendezvous is involved — it is the one path that needs nobody else — but
    // only when the gateway is actually listening off-loopback, since a forward
    // to a port bound to 127.0.0.1 lands on nothing.
    let (mapped, port_mapping, reachability) = if allow_lan {
        match local_ipv4() {
            Some(local_ip) => {
                let ports = portmap::Ports {
                    gateway_tcp: gateway_addr.port(),
                    media_tcp: DEFAULT_LIVEKIT_PORT,
                    media_tcp_ice: DEFAULT_LIVEKIT_TCP_PORT,
                    media_udp: DEFAULT_LIVEKIT_UDP_PORT,
                };
                match portmap::request(local_ip, ports).await {
                    Ok((mapped, guard)) => {
                        eprintln!(
                            "[host] {} mapped {} (media: {}, hairpin: {})",
                            mapped.method,
                            mapped.endpoint(),
                            mapped.media,
                            mapped.hairpin
                        );
                        let reach = Reachability::Direct {
                            endpoint: mapped.endpoint(),
                            method: mapped.method,
                            // Voice only counts as reachable when the media
                            // ports kept their numbers *and* we can advertise
                            // them, which is what the hairpin check gates.
                            media: mapped.media && mapped.hairpin,
                        };
                        (Some(mapped), Some(guard), reach)
                    }
                    Err(reason) => {
                        eprintln!("[host] no port mapping: {reason}");
                        (None, None, Reachability::LanOnly { reason })
                    }
                }
            }
            None => (
                None,
                None,
                Reachability::LanOnly {
                    reason: "this machine has no IPv4 address on a local network".into(),
                },
            ),
        }
    } else {
        (None, None, Reachability::LoopbackOnly)
    };

    // Advertising the external address to LiveKit *replaces* its LAN ICE
    // candidate rather than adding to it, so it is only safe once we know this
    // machine can reach its own public address — otherwise the host and its LAN
    // friends lose the voice path the remote friend gains. Both conditions, or
    // neither: see `livekit_bundle::spawn_livekit`.
    let advertise_ip = mapped
        .as_ref()
        .filter(|m| m.media && m.hairpin)
        .map(|m| m.public_ip);

    // Register with rendezvous first so we know which LiveKit URL to hand
    // to clients. If the rendezvous operator runs a shared LiveKit, that
    // URL takes precedence over our local subprocess.
    let mut rendezvous_state: Option<(
        crate::rendezvous::ControlStream,
        crate::rendezvous::PublishInfo,
    )> = None;
    let mut publish_error: Option<String> = None;
    let listed_public = publish.publish_public;
    if let Some(url) = rendezvous_url {
        let endpoint = mapped.as_ref().map(|m| m.endpoint());
        match crate::rendezvous::register(&url, publish, endpoint, &identity).await {
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
                publish_error = Some(e);
            }
        }
    }

    // If rendezvous handed us a shared LiveKit URL, pin it. Otherwise the
    // gateway derives one per connection from the client's Host header.
    let explicit_url = rendezvous_state
        .as_ref()
        .and_then(|(_, info)| info.livekit_url.clone());

    // Only now is it known whether a local SFU is wanted at all: a rendezvous
    // that runs its own wins for every client, and the bundled one would spend
    // the session holding port 7880 and serving nobody. It used to be started
    // unconditionally, before this was knowable — which is why the spawn moved
    // down here rather than the question moving up.
    let shared_sfu_url = explicit_url.clone();
    let (livekit, voice_bundled) = if explicit_url.is_some() {
        eprintln!("[host] rendezvous supplies the SFU — not starting the bundled one");
        (None, false)
    } else {
        match livekit_bundle::spawn_livekit(advertise_ip).await {
            Ok(child) => {
                eprintln!("[host] livekit ready at ws://127.0.0.1:{DEFAULT_LIVEKIT_PORT}");
                (Some(child), true)
            }
            Err(e) => {
                eprintln!("[host] livekit unavailable: {e}");
                tracing::warn!(error = %e, "self-host voice unavailable");
                (None, false)
            }
        }
    };

    let livekit_cfg = LiveKitConfig {
        explicit_url,
        port: DEFAULT_LIVEKIT_PORT,
        // Local credentials only ever sign for our own bundled subprocess.
        api_key: DEFAULT_LIVEKIT_KEY.into(),
        api_secret: DEFAULT_LIVEKIT_SECRET.into(),
        // When the rendezvous runs a shared SFU, it mints tokens for us — we
        // hold a session grant, never its signing secret (a public relay can't
        // hand that out: any host could then mint into any other host's rooms).
        minter: rendezvous_state.as_ref().and_then(|(_, info)| {
            match (&info.livekit_url, &info.voice_token_grant) {
                (Some(_), Some(grant)) => Some(std::sync::Arc::new(
                    crate::rendezvous::RendezvousMinter::new(&info.rendezvous_base, grant.clone()),
                )
                    as std::sync::Arc<dyn dioxusfun_server::livekit::VoiceTokenMinter>),
                _ => None,
            }
        }),
        // Friends proxied in by the rendezvous hit our gateway on loopback;
        // hand them our LAN address for LiveKit instead of their own machine.
        lan_host: local_ip_address::local_ip().ok().map(|ip| ip.to_string()),
        // …unless we have a public one, which is the only address that also
        // works for a proxied friend who is not on this network.
        public_host: advertise_ip.map(|ip| ip.to_string()),
    };

    let operators = std::collections::HashSet::from([operator_pubkey]);
    // Durable self-host data lives next to the identity/settings files, so a
    // relaunched host keeps its guilds, members, and message history.
    let cfg = dioxusfun_server::ServerConfig {
        livekit: livekit_cfg,
        operators,
        data_dir: crate::identity::config_dir().join("host-data"),
    };
    let gateway = dioxusfun_server::spawn_on(listener, cfg)
        .await
        .map_err(|e| format!("embedded server: {e}"))?;
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

    // Which SFU clients will actually be sent to. Empty means there is none —
    // which since the spawn was deferred is no longer the same question as
    // "did the bundled one start": a rendezvous that supplies an SFU means we
    // deliberately started nothing, and voice works fine.
    let livekit_display = match (&shared_sfu_url, voice_bundled) {
        (Some(url), _) => url.clone(),
        (None, true) => format!("ws://127.0.0.1:{DEFAULT_LIVEKIT_PORT}"),
        (None, false) => String::new(),
    };

    Ok(HostHandle {
        info: HostInfo {
            gateway_addr,
            livekit_url: livekit_display,
            lan_url,
            local_url,
            voice_bundled,
            shortcode,
            publish_error,
            listed_public,
            reachability,
        },
        gateway: Some(gateway),
        livekit,
        rendezvous_task,
        _port_mapping: port_mapping,
    })
}

fn lan_url_for(port: u16) -> Option<String> {
    let ip = local_ip_address::local_ip().ok()?;
    if matches!(ip, IpAddr::V4(v4) if v4.is_loopback() || v4.is_unspecified()) {
        return None;
    }
    Some(format!("ws://{ip}:{port}"))
}

/// The address the router would forward to. IPv4 only, because both mapping
/// protocols are: IGD's `AddPortMapping` and NAT-PMP both name an internal
/// IPv4 client, and a v6 host needs no mapping in the first place — it needs a
/// firewall rule, which is not ours to ask for.
fn local_ipv4() -> Option<Ipv4Addr> {
    match local_ip_address::local_ip().ok()? {
        IpAddr::V4(v4) if !v4.is_loopback() && !v4.is_unspecified() => Some(v4),
        _ => None,
    }
}
