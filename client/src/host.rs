use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use dioxusfun_server::ServerHandle;
use dioxusfun_server::livekit::LiveKitConfig;
use dioxusfun_server::livekit_bundle::{self, Advertise, LivekitSubprocess};

use crate::portmap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reachability {
    LoopbackOnly,
    LanOnly {
        reason: String,
    },
    Direct {
        method: &'static str,
        media: bool,
        note: Option<&'static str>,
    },
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
    pub voice_reason: String,
    pub shortcode: Option<String>,
    pub publish_error: Option<String>,
    pub listed_public: bool,
    pub reachability: Reachability,
}

/// Where a session's calls go. One per session, not per caller: a room lives
/// on one SFU, so everyone in a channel must be handed the same one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SfuPlan {
    Bundled,
    Shared(String),
}

/// This machine pays for its own calls whenever friends can reach its media
/// ports, directly or through a TURN relay the rendezvous offers; the
/// rendezvous's SFU is for the host nobody outside can reach any other way.
pub fn sfu_plan(reachability: &Reachability, shared_offer: Option<&str>, relay: bool) -> SfuPlan {
    match (reachability, shared_offer, relay) {
        (_, None, _) => SfuPlan::Bundled,
        (Reachability::Direct { media: true, .. }, Some(_), _) => SfuPlan::Bundled,
        (_, Some(_), true) => SfuPlan::Bundled,
        (_, Some(url), false) => SfuPlan::Shared(url.to_string()),
    }
}

/// `turn:host:port?transport=udp` → `host:port`: a TURN server answers STUN
/// too, so the relay doubles as the SFU's address discovery.
pub fn stun_target(turn_urls: &[String]) -> Option<String> {
    turn_urls.iter().find_map(|url| {
        let rest = url
            .strip_prefix("turn:")
            .or_else(|| url.strip_prefix("stun:"))?;
        let rest = rest.split('?').next()?;
        (!rest.is_empty()).then(|| rest.to_string())
    })
}

pub struct HostHandle {
    pub info: HostInfo,
    /// What the rendezvous link does after start: lost, restored, renamed.
    pub updates: Option<tokio::sync::mpsc::UnboundedReceiver<crate::rendezvous::HostUpdate>>,
    gateway: Option<ServerHandle>,
    quic: Option<dioxusfun_server::quic::QuicHandle>,
    shutdown: Option<dioxusfun_server::GatewayShutdown>,
    livekit: Option<LivekitSubprocess>,
    rendezvous_task: Option<tokio::task::JoinHandle<()>>,
    runtime: tokio::runtime::Handle,
    _port_mapping: Option<portmap::MappingGuard>,
}

/// How long the QUIC door stays open after the sockets are told to go, so the
/// close frame reaches a friend before the endpoint it travelled over is gone.
const QUIC_CLOSE_GRACE: std::time::Duration = std::time::Duration::from_millis(250);

impl Drop for HostHandle {
    fn drop(&mut self) {
        // Aborting the listener leaves everyone already connected being served:
        // axum spawns a task per connection and `on_upgrade` spawns another,
        // neither of them a child of the accept loop. This is what ends them.
        if let Some(shutdown) = self.shutdown.take() {
            shutdown.close_all();
        }
        if let Some(handle) = self.gateway.take() {
            handle.abort();
        }
        if let Some(task) = self.rendezvous_task.take() {
            task.abort();
        }
        if let Some(quic) = self.quic.take() {
            self.runtime.spawn(async move {
                tokio::time::sleep(QUIC_CLOSE_GRACE).await;
                quic.shutdown().await;
            });
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
    manual_ip: Option<IpAddr>,
    rendezvous_url: Option<String>,
    publish: crate::rendezvous::PublishOptions,
    identity: crate::identity::Identity,
) -> Result<HostHandle, String> {
    if manual_ip.is_some_and(|ip| !ip.is_ipv4() || !public_address(ip)) {
        return Err(
            "manual forwarding needs a public IPv4 address; IPv6 is discovered automatically"
                .into(),
        );
    }
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
        match dioxusfun_server::quic::bind_quic(
            Some(transport_secret),
            &coordination,
            if manual_ip.is_some() {
                dioxusfun_server::quic::DEFAULT_PORT
            } else {
                dioxusfun_server::quic::RANDOM_PORT
            },
        )
        .await
        {
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

    let mut manual = if allow_lan
        && let (Some(ip), Some(local), Some(port)) = (manual_ip, local_ipv4(), quic_port)
    {
        Some(portmap::manual(ip, local, livekit_bundle::ports().ws, port).await)
    } else {
        None
    };
    let (mut mapped, port_mapping, mut reachability) = if manual
        .as_ref()
        .is_some_and(|m| m.media.available() && m.hairpin)
    {
        (
            manual.take(),
            None,
            Reachability::Direct {
                method: "manual forwarding",
                media: true,
                note: None,
            },
        )
    } else {
        match (allow_lan, local_ipv4(), quic_port) {
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
                            "[host] {} mapped {} (quic: {}, media: {:?}, hairpin: {})",
                            mapped.method,
                            mapped.public_ip,
                            mapped.quic,
                            mapped.media,
                            mapped.hairpin
                        );
                        let reach = if mapped.quic || mapped.media.available() {
                            Reachability::Direct {
                                method: mapped.method,
                                media: mapped.media.available(),
                                note: mapped.media_note,
                            }
                        } else {
                            Reachability::LanOnly {
                                reason: mapped
                                    .media_note
                                    .unwrap_or("no usable ports were mapped")
                                    .into(),
                            }
                        };
                        (Some(mapped), Some(guard), reach)
                    }
                    Err(reason) => {
                        eprintln!("[host] no port mapping: {reason}");
                        (None, None, Reachability::LanOnly { reason })
                    }
                }
            }
        }
    };

    apply_manual_mapping(&mut mapped, &mut reachability, manual);

    let public_v6: Vec<IpAddr> = if allow_lan
        && quic_endpoint
            .as_ref()
            .is_some_and(|ep| ep.bound_sockets().iter().any(|socket| socket.is_ipv6()))
    {
        local_ip_address::list_afinet_netifas()
            .unwrap_or_default()
            .into_iter()
            .map(|(_, ip)| ip)
            .filter(|ip| ip.is_ipv6() && public_address(*ip))
            .collect()
    } else {
        Vec::new()
    };
    let mut voice_hosts = public_v6.clone();
    if let Some(ip) = mapped
        .as_ref()
        .filter(|m| m.media.available())
        .map(|m| m.public_ip)
    {
        voice_hosts.push(ip);
    }
    voice_hosts.sort();
    voice_hosts.dedup();
    if !public_v6.is_empty() && !matches!(reachability, Reachability::Direct { media: true, .. }) {
        reachability = Reachability::Direct {
            method: "IPv6",
            media: true,
            note: None,
        };
    }

    let advertise_ip = mapped
        .as_ref()
        .filter(|m| m.media.available())
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
                addrs.push(SocketAddr::new(m.quic_ip, m.quic_port).to_string());
            }
            for ip in &public_v6 {
                if let Some(socket) = ep.bound_sockets().iter().find(|s| s.is_ipv6()) {
                    addrs.push(SocketAddr::new(*ip, socket.port()).to_string());
                }
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
    if let Some(url) = rendezvous_url.as_deref() {
        match crate::rendezvous::register(url, &publish, transport.as_ref(), &identity).await {
            Ok((info, control)) => {
                eprintln!(
                    "[host] rendezvous registered: shortcode={} livekit_url={:?}",
                    info.shortcode, info.livekit_url
                );
                tracing::info!(
                    shortcode = %info.shortcode,
                    shared_sfu = ?info.livekit_url,
                    voice_grant = info.voice_token_grant.is_some(),
                    turn_urls = ?info.turn.as_ref().map(|t| &t.urls),
                    turn_expires_in_s = info.turn.as_ref().map(|t| t.expires_unix.saturating_sub(crate::rendezvous::now_unix())),
                    "voice route: registered with the rendezvous"
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

    let shared_offer = rendezvous_state
        .as_ref()
        .and_then(|(_, info)| info.livekit_url.clone());
    let turn = rendezvous_state
        .as_ref()
        .and_then(|(_, info)| info.turn.clone())
        .filter(|_| open_to_others);
    let plan = sfu_plan(&reachability, shared_offer.as_deref(), turn.is_some());
    let advertise = match (
        advertise_ip,
        turn.as_ref().and_then(|t| stun_target(&t.urls)),
    ) {
        (Some(ip), _) => Advertise::Mapped(ip),
        (None, Some(stun)) => Advertise::Stun(stun),
        (None, None) => Advertise::Local,
    };
    tracing::info!(
        reachability = ?reachability,
        mapping = ?mapped.as_ref().map(|m| format!(
            "{} {} quic={} media={:?} hairpin={}",
            m.method, m.public_ip, m.quic, m.media, m.hairpin
        )),
        public_v6 = ?public_v6,
        open_to_others,
        shared_sfu_offered = shared_offer.is_some(),
        relay_offered = turn.is_some(),
        plan = ?plan,
        advertise = ?advertise,
        "voice route: host plan"
    );
    let mut voice_reason = if !voice_hosts.is_empty() {
        format!(
            "Calls run on this machine. Clients verify media through local and public candidates: {}. {}. IPv6 requires IPv6 at the caller and inbound firewall access.",
            voice_hosts
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", "),
            mapped
                .as_ref()
                .and_then(|m| m.media_note)
                .unwrap_or("Connectivity is verified when joining")
        )
    } else {
        match &reachability {
            Reachability::LanOnly { reason } => reason.clone(),
            Reachability::Direct {
                note: Some(note), ..
            } => (*note).to_string(),
            _ => "No usable public voice address was found; local-network calls may still work."
                .into(),
        }
    };

    let data_dir = crate::identity::config_dir().join("host-data");
    let creds = livekit_bundle::credentials_or_ephemeral(&data_dir);

    let (livekit, explicit_url) = match &plan {
        SfuPlan::Shared(url) => {
            eprintln!(
                "[host] friends cannot reach this machine's voice ports — calls go through the rendezvous's SFU at {url}"
            );
            (None, Some(url.clone()))
        }
        SfuPlan::Bundled => {
            match livekit_bundle::spawn_livekit(advertise.clone(), &creds, &data_dir).await {
                Ok(child) => {
                    eprintln!(
                        "[host] livekit ready at ws://127.0.0.1:{} — this machine carries the calls{}",
                        livekit_bundle::ports().ws,
                        if matches!(advertise, Advertise::Stun(_)) {
                            ", friends behind NAT relayed by the rendezvous"
                        } else {
                            ""
                        }
                    );
                    (Some(child), None)
                }
                Err(e) => match shared_offer.clone() {
                    Some(url) => {
                        voice_reason = format!("The local voice server failed to start: {e}");
                        eprintln!(
                            "[host] livekit unavailable ({e}) — calls go through the rendezvous's SFU at {url}"
                        );
                        tracing::warn!(error = %e, "bundled SFU failed; using the rendezvous's");
                        (None, Some(url))
                    }
                    None => {
                        voice_reason = format!(
                            "The local voice server failed to start: {e}; no rendezvous voice is available."
                        );
                        eprintln!("[host] livekit unavailable: {e}");
                        tracing::warn!(error = %e, "self-host voice unavailable");
                        (None, None)
                    }
                },
            }
        }
    };
    let voice_bundled = livekit.is_some();
    let voice_relayed = voice_bundled && matches!(advertise, Advertise::Stun(_));
    if voice_relayed {
        voice_reason.push_str(
            " Friends who cannot reach this machine's voice ports are relayed by the rendezvous's TURN, which forwards only their encrypted packets.",
        );
    }
    let shared_sfu_url = explicit_url.clone();
    // Only a bundled SFU behind NAT needs the relay; the rendezvous's own SFU
    // is public, and a mapped one is reached directly.
    let relay_ice = voice_relayed
        .then(|| turn.clone())
        .flatten()
        .map(|creds| (creds.ice_servers(), creds));
    let ice_servers = dioxusfun_server::livekit::shared_ice_servers(
        relay_ice
            .as_ref()
            .map(|(servers, _)| servers.clone())
            .unwrap_or_default(),
    );

    let rendezvous_minter = match rendezvous_state.as_ref() {
        Some((_, info)) => info.voice_token_grant.as_ref().map(|grant| {
            std::sync::Arc::new(crate::rendezvous::RendezvousMinter::new(
                &info.rendezvous_base,
                grant.clone(),
            ))
        }),
        _ => None,
    };

    let livekit_cfg = LiveKitConfig {
        explicit_url,
        port: livekit_bundle::ports().ws,
        api_key: creds.key,
        api_secret: creds.secret,
        minter: rendezvous_minter
            .clone()
            .filter(|_| !voice_bundled)
            .map(|m| m as std::sync::Arc<dyn dioxusfun_server::livekit::VoiceTokenMinter>),
        fallback: if voice_bundled {
            shared_offer
                .clone()
                .zip(rendezvous_minter.clone())
                .map(|(url, minter)| {
                    std::sync::Arc::new(dioxusfun_server::livekit::VoiceFallback::new(url, minter))
                })
        } else {
            None
        },
        lan_host: local_ip_address::local_ip().ok().map(|ip| ip.to_string()),
        public_host: advertise_ip.map(|ip| ip.to_string()),
        alternate_hosts: voice_hosts,
        ice_servers: ice_servers.clone(),
    };
    tracing::info!(
        bundled = voice_bundled,
        relayed = voice_relayed,
        shared_sfu = ?livekit_cfg.explicit_url,
        lan_host = ?livekit_cfg.lan_host,
        public_host = ?livekit_cfg.public_host,
        alternate_hosts = ?livekit_cfg.alternate_hosts,
        sfu_port = livekit_cfg.port,
        ice_urls = ?crate::protocol::ice_urls(&livekit_cfg.ice_servers()),
        fallback_armed = livekit_cfg.fallback.is_some(),
        "voice route: addresses friends will be offered"
    );

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
    let (router, shutdown) = dioxusfun_server::build_gateway(cfg)
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

    let (updates_tx, updates_rx) = tokio::sync::mpsc::unbounded_channel();
    let (shortcode, rendezvous_task) = match rendezvous_state {
        Some((control, info)) => {
            let registration = crate::rendezvous::Registration {
                url: rendezvous_url.clone().unwrap_or_default(),
                options: publish,
                transport,
                identity: identity.clone(),
            };
            (
                Some(info.shortcode),
                Some(crate::rendezvous::maintain(
                    control,
                    registration,
                    rendezvous_minter,
                    relay_ice.map(|(_, creds)| (ice_servers, creds)),
                    updates_tx,
                )),
            )
        }
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
            voice_reason,
            shortcode,
            publish_error,
            listed_public,
            reachability,
        },
        updates: Some(updates_rx),
        gateway: Some(gateway),
        quic,
        shutdown: Some(shutdown),
        livekit,
        rendezvous_task,
        runtime: tokio::runtime::Handle::current(),
        _port_mapping: port_mapping,
    })
}

fn apply_manual_mapping(
    mapped: &mut Option<portmap::Mapped>,
    reachability: &mut Reachability,
    manual: Option<portmap::Mapped>,
) {
    if let Some(manual) = manual
        && mapped
            .as_ref()
            .is_none_or(|m| !m.quic && !m.media.available())
    {
        *reachability = Reachability::Direct {
            method: "manual forwarding",
            media: manual.media.available(),
            note: manual.media_note,
        };
        *mapped = Some(manual);
    }
}

pub fn public_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            !crate::protocol::is_private_ip(ip.into())
                && !ip.is_unspecified()
                && !ip.is_multicast()
                && !ip.is_broadcast()
        }
        IpAddr::V6(ip) => (ip.segments()[0] & 0xe000) == 0x2000,
    }
}

fn local_ipv4() -> Option<Ipv4Addr> {
    match local_ip_address::local_ip().ok()? {
        IpAddr::V4(v4) if !v4.is_loopback() && !v4.is_unspecified() => Some(v4),
        _ => None,
    }
}

#[cfg(test)]
mod sfu_tests {
    use super::*;

    fn automatic_mapping(quic: bool, media: bool, hairpin: bool) -> portmap::Mapped {
        portmap::Mapped {
            method: "UPnP-IGD",
            public_ip: "203.0.113.5".parse().unwrap(),
            media: portmap::MediaMapping {
                signaling: media,
                udp: media,
                tcp: media,
            },
            quic,
            quic_port: 19001,
            quic_ip: "203.0.113.5".parse().unwrap(),
            hairpin,
            media_note: Some("no hairpin NAT"),
        }
    }

    fn unverified_manual_mapping() -> portmap::Mapped {
        portmap::Mapped {
            method: "manual forwarding",
            public_ip: "198.51.100.9".parse().unwrap(),
            media: portmap::MediaMapping {
                signaling: true,
                udp: true,
                tcp: true,
            },
            quic: true,
            quic_port: 9001,
            quic_ip: "198.51.100.9".parse().unwrap(),
            hairpin: false,
            media_note: Some("manual signaling probe failed"),
        }
    }

    #[test]
    fn a_stale_manual_address_cannot_replace_a_router_granted_chat_endpoint() {
        for media in [false, true] {
            let automatic = automatic_mapping(true, media, false);
            let mut mapped = Some(automatic.clone());
            let mut reach = Reachability::Direct {
                method: automatic.method,
                media: false,
                note: automatic.media_note,
            };
            apply_manual_mapping(&mut mapped, &mut reach, Some(unverified_manual_mapping()));
            let selected = mapped.unwrap();
            assert_eq!(selected.public_ip, automatic.public_ip);
            assert_eq!(selected.quic_port, 19001);
            assert!(matches!(
                reach,
                Reachability::Direct {
                    method: "UPnP-IGD",
                    media: false,
                    ..
                }
            ));
        }
    }

    #[test]
    fn manual_address_remains_available_when_automatic_mapping_is_unusable() {
        for automatic in [None, Some(automatic_mapping(false, false, false))] {
            let mut mapped = automatic;
            let mut reach = Reachability::LanOnly {
                reason: "automatic failed".into(),
            };
            let manual = unverified_manual_mapping();
            apply_manual_mapping(&mut mapped, &mut reach, Some(manual.clone()));
            let selected = mapped.unwrap();
            assert_eq!(selected.public_ip, manual.public_ip);
            assert_eq!(selected.quic_port, 9001);
            assert!(matches!(
                reach,
                Reachability::Direct {
                    method: "manual forwarding",
                    media: true,
                    ..
                }
            ));
        }
    }

    #[test]
    fn ipv6_candidates_exclude_local_only_and_multicast_addresses() {
        for ip in [
            "::",
            "::1",
            "fe80::1",
            "fd00::1",
            "ff02::1",
            "::ffff:192.168.0.1",
        ] {
            assert!(!public_address(ip.parse().unwrap()), "{ip}");
        }
        assert!(public_address("2800:810::123".parse().unwrap()));
    }

    #[test]
    fn the_host_carries_calls_whenever_friends_can_reach_it() {
        let direct = Reachability::Direct {
            method: "UPnP",
            media: true,
            note: None,
        };
        assert_eq!(
            sfu_plan(&direct, Some("ws://shared"), false),
            SfuPlan::Bundled
        );
        assert_eq!(
            sfu_plan(&Reachability::LoopbackOnly, None, false),
            SfuPlan::Bundled
        );
        let lan = Reachability::LanOnly {
            reason: "no mapping".into(),
        };
        assert_eq!(sfu_plan(&lan, None, false), SfuPlan::Bundled);
    }

    #[test]
    fn a_relay_keeps_the_calls_on_this_machine_without_a_port_map() {
        let lan = Reachability::LanOnly {
            reason: "no mapping".into(),
        };
        assert_eq!(sfu_plan(&lan, Some("ws://shared"), true), SfuPlan::Bundled);
        let chat_only = Reachability::Direct {
            method: "UPnP",
            media: false,
            note: None,
        };
        assert_eq!(
            sfu_plan(&chat_only, Some("ws://shared"), true),
            SfuPlan::Bundled
        );
        assert_eq!(
            stun_target(&["turn:relay.example:7702?transport=udp".into()]),
            Some("relay.example:7702".into())
        );
        assert_eq!(stun_target(&["wss://not-a-relay".into()]), None);
    }

    #[test]
    fn the_shared_sfu_is_only_for_a_host_nobody_outside_can_reach() {
        let shared = SfuPlan::Shared("ws://shared".into());
        let lan = Reachability::LanOnly {
            reason: "no mapping".into(),
        };
        assert_eq!(sfu_plan(&lan, Some("ws://shared"), false), shared);
        let chat_only = Reachability::Direct {
            method: "UPnP",
            media: false,
            note: None,
        };
        assert_eq!(sfu_plan(&chat_only, Some("ws://shared"), false), shared);
        assert_eq!(
            sfu_plan(&Reachability::LoopbackOnly, Some("ws://shared"), false),
            shared
        );
    }
}
