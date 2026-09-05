use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use dioxusfun_server::ServerHandle;
use dioxusfun_server::livekit::LiveKitConfig;
use dioxusfun_server::livekit_bundle::{self, LivekitSubprocess};

use crate::portmap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reachability {
    LoopbackOnly,
    LanOnly { reason: String },
    Direct { method: &'static str, media: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostInfo {
    pub gateway_addr: SocketAddr,
    pub livekit_url: String,
    /// Plain WebSocket, loopback only: what this machine's own client dials.
    pub local_url: String,
    /// `quic://key@addrs` — what a friend types. Absent when nobody but this
    /// machine may connect.
    pub share: Option<String>,
    pub voice_bundled: bool,
    pub shortcode: Option<String>,
    pub publish_error: Option<String>,
    pub listed_public: bool,
    pub reachability: Reachability,
}

pub struct HostHandle {
    pub info: HostInfo,
    gateway: Option<ServerHandle>,
    _quic: Option<dioxusfun_server::quic::QuicHandle>,
    livekit: Option<LivekitSubprocess>,
    rendezvous_task: Option<tokio::task::JoinHandle<()>>,
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
        self.livekit = None;
    }
}

/// The plaintext gateway binds loopback whatever `allow_lan` says: off this
/// machine every connection is QUIC, authenticated by the key in the share
/// string, so nothing on a LAN or a relay can read it. `allow_lan` decides
/// whether this machine's own addresses are offered and its router asked to
/// forward them; a join code needs QUIC too, for the relay to introduce us.
pub async fn start_self_host(
    allow_lan: bool,
    rendezvous_url: Option<String>,
    publish: crate::rendezvous::PublishOptions,
    identity: crate::identity::Identity,
) -> Result<HostHandle, String> {
    let operator_pubkey = identity.pubkey.clone();

    let listener = dioxusfun_server::bind_with_fallback(
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 9000),
        20,
    )
    .await
    .map_err(|e| format!("embedded server: {e}"))?;
    let gateway_addr = listener
        .local_addr()
        .map_err(|e| format!("embedded server: {e}"))?;

    let coordination = match rendezvous_url.as_deref() {
        Some(url) => crate::rendezvous::coordination_offered(url).await,
        None => dioxusfun_server::quic::Coordination::None,
    };

    let open_to_others = allow_lan || rendezvous_url.is_some();
    let quic_endpoint = if !open_to_others {
        eprintln!("[host] nobody but this machine may connect — not opening the QUIC door");
        None
    } else {
        let transport_secret = crate::quic::secret_for(&identity);
        match dioxusfun_server::quic::bind_quic(Some(transport_secret), &coordination).await {
            Ok(ep) => Some(ep),
            Err(e) => {
                eprintln!("[host] quic unavailable: {e}");
                tracing::warn!(error = %e, "quic endpoint not bound");
                None
            }
        }
    };
    let quic_port = quic_endpoint
        .as_ref()
        .and_then(|ep| ep.bound_sockets().first().map(|s| s.port()));

    let (mapped, port_mapping, reachability) = match (allow_lan, local_ipv4(), quic_port) {
        (false, _, _) => (None, None, Reachability::LoopbackOnly),
        (true, None, _) => (
            None,
            None,
            Reachability::LanOnly {
                reason: "this machine has no IPv4 address on a local network".into(),
            },
        ),
        (true, _, None) => (
            None,
            None,
            Reachability::LanOnly {
                reason: "the QUIC endpoint did not bind, so there is no port to forward".into(),
            },
        ),
        (true, Some(local_ip), Some(quic_udp)) => {
            let sfu = livekit_bundle::ports();
            let ports = portmap::Ports {
                media_tcp: sfu.ws,
                media_tcp_ice: sfu.tcp,
                media_udp: sfu.udp,
                quic_udp,
            };
            match portmap::request(local_ip, ports).await {
                Ok((mapped, guard)) => {
                    eprintln!(
                        "[host] {} mapped {} (quic: {}, media: {}, hairpin: {})",
                        mapped.method, mapped.public_ip, mapped.quic, mapped.media, mapped.hairpin
                    );
                    let reach = Reachability::Direct {
                        method: mapped.method,
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
    };

    let advertise_ip = mapped
        .as_ref()
        .filter(|m| m.media && m.hairpin)
        .map(|m| m.public_ip);

    if coordination.is_coordinated()
        && let Some(ep) = quic_endpoint.as_ref()
    {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), ep.online()).await;
    }

    // Public first, LAN second, relay last: a friend tries them in order.
    let transport_addrs: Vec<String> = match (quic_endpoint.as_ref(), quic_port) {
        (Some(ep), Some(port)) => {
            let mut addrs = Vec::new();
            if let Some(m) = mapped.as_ref().filter(|m| m.quic) {
                addrs.push(SocketAddr::new(m.public_ip, port).to_string());
            }
            if allow_lan && let Some(ip) = local_ipv4() {
                addrs.push(SocketAddr::new(IpAddr::V4(ip), port).to_string());
            }
            addrs.extend(ep.addr().addrs.iter().filter_map(|a| match a {
                iroh::TransportAddr::Relay(url) => Some(url.to_string()),
                _ => None,
            }));
            addrs
        }
        _ => Vec::new(),
    };
    let transport_key = quic_endpoint.as_ref().map(|ep| ep.id().to_string());
    let share = transport_key
        .as_ref()
        .filter(|_| !transport_addrs.is_empty())
        .map(|key| crate::protocol::format_quic_share(key, &transport_addrs));
    let transport = transport_key
        .as_ref()
        .filter(|_| !transport_addrs.is_empty())
        .map(|key| crate::rendezvous::TransportAdvert {
            key: key.clone(),
            addrs: transport_addrs.clone(),
        });

    let mut rendezvous_state: Option<(
        crate::rendezvous::ControlStream,
        crate::rendezvous::PublishInfo,
    )> = None;
    let mut publish_error: Option<String> = None;
    let listed_public = publish.publish_public;
    if let Some(url) = rendezvous_url {
        match crate::rendezvous::register(&url, publish, transport, &identity).await {
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

    let explicit_url = rendezvous_state
        .as_ref()
        .and_then(|(_, info)| info.livekit_url.clone());

    let data_dir = crate::identity::config_dir().join("host-data");
    let creds = livekit_bundle::credentials_or_ephemeral(&data_dir);

    let shared_sfu_url = explicit_url.clone();
    let (livekit, voice_bundled) = if explicit_url.is_some() {
        eprintln!("[host] rendezvous supplies the SFU — not starting the bundled one");
        (None, false)
    } else {
        match livekit_bundle::spawn_livekit(advertise_ip, &creds, &data_dir).await {
            Ok(child) => {
                eprintln!(
                    "[host] livekit ready at ws://127.0.0.1:{}",
                    livekit_bundle::ports().ws
                );
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
        port: livekit_bundle::ports().ws,
        api_key: creds.key,
        api_secret: creds.secret,
        minter: rendezvous_state.as_ref().and_then(|(_, info)| {
            match (&info.livekit_url, &info.voice_token_grant) {
                (Some(_), Some(grant)) => Some(std::sync::Arc::new(
                    crate::rendezvous::RendezvousMinter::new(&info.rendezvous_base, grant.clone()),
                )
                    as std::sync::Arc<dyn dioxusfun_server::livekit::VoiceTokenMinter>),
                _ => None,
            }
        }),
        lan_host: local_ip_address::local_ip().ok().map(|ip| ip.to_string()),
        public_host: advertise_ip.map(|ip| ip.to_string()),
    };

    // Every way a friend can reach this gateway, because a login signed for
    // an address not in this set is refused (see server auth.rs).
    let mut identities = dioxusfun_server::local_identities(gateway_addr.port());
    if let Some(key) = transport_key.as_deref() {
        identities.insert(crate::protocol::quic_origin(key));
    }

    let operators = std::collections::HashSet::from([operator_pubkey]);
    let cfg = dioxusfun_server::ServerConfig {
        livekit: livekit_cfg,
        operators,
        data_dir,
        identities,
        media_max_bytes: dioxusfun_server::media::DEFAULT_MAX_BYTES,
    };
    let router = dioxusfun_server::build_router(cfg)
        .await
        .map_err(|e| format!("embedded server: {e}"))?;

    let quic = quic_endpoint.and_then(|ep| {
        match dioxusfun_server::quic::serve_on_with(ep, router.clone(), coordination.clone()) {
            Ok(handle) => {
                eprintln!("[host] quic gateway at key {}", handle.endpoint_id);
                Some(handle)
            }
            Err(e) => {
                eprintln!("[host] quic not serving: {e}");
                tracing::warn!(error = %e, "quic front door not started");
                None
            }
        }
    });

    let gateway = dioxusfun_server::serve_router(listener, router);
    let local_url = format!("ws://127.0.0.1:{}", gateway_addr.port());

    let (shortcode, rendezvous_task) = match rendezvous_state {
        Some((control, info)) => (
            Some(info.shortcode),
            Some(crate::rendezvous::keep_alive(control)),
        ),
        None => (None, None),
    };

    let livekit_display = match (&shared_sfu_url, voice_bundled) {
        (Some(url), _) => url.clone(),
        (None, true) => format!("ws://127.0.0.1:{}", livekit_bundle::ports().ws),
        (None, false) => String::new(),
    };

    Ok(HostHandle {
        info: HostInfo {
            gateway_addr,
            livekit_url: livekit_display,
            local_url,
            share,
            voice_bundled,
            shortcode,
            publish_error,
            listed_public,
            reachability,
        },
        gateway: Some(gateway),
        _quic: quic,
        livekit,
        rendezvous_task,
        _port_mapping: port_mapping,
    })
}

fn local_ipv4() -> Option<Ipv4Addr> {
    match local_ip_address::local_ip().ok()? {
        IpAddr::V4(v4) if !v4.is_loopback() && !v4.is_unspecified() => Some(v4),
        _ => None,
    }
}
