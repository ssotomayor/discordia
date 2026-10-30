use std::collections::HashSet;
use std::net::IpAddr;
use std::sync::Arc;

use discordia_server::livekit::LiveKitConfig;
use discordia_server::livekit_bundle::{self, Advertise};
use discordia_server::quic::Coordination;
use discordia_server::rendezvous::{HostKey, TransportAdvert};

use crate::options::{DEFAULT_ADDR, ServerOptions};

pub async fn serve(opts: ServerOptions) -> Result<(), String> {
    let addr = opts.addr.unwrap_or(DEFAULT_ADDR.parse().unwrap());
    let publish_to = crate::publish::targets(opts.publish_to.as_deref().unwrap_or_default())?;
    let data_dir = opts.data_dir();
    let _data_lease = livekit_bundle::HostLease::claim(&data_dir)
        .map_err(|e| format!("host ownership refused: {e}"))?;
    tracing::info!(data_dir = %data_dir.display(), "durable data directory");

    let public = match opts.public_address.as_deref() {
        Some(name) => Some((name.to_string(), resolve(name).await?)),
        None => None,
    };

    let explicit_url = opts.livekit_url.clone().filter(|u| !u.trim().is_empty());
    let mut livekit_cfg = LiveKitConfig::new(
        &data_dir,
        explicit_url.clone(),
        opts.livekit_api_key
            .clone()
            .zip(opts.livekit_api_secret.clone()),
        opts.ice_servers.clone().unwrap_or_default().0,
    );

    let want_autospawn = opts.livekit_autospawn.unwrap_or(true);
    let _livekit_handle = if want_autospawn && explicit_url.is_none() {
        livekit_cfg.port = livekit_bundle::ports().ws;
        // Over QUIC the Host header is a placeholder, so these are what a caller is told to dial.
        livekit_cfg.lan_host = local_ip_address::local_ip().ok().map(|ip| ip.to_string());
        livekit_cfg.public_host = public.as_ref().map(|(name, _)| bare_host(name));
        let creds = livekit_bundle::Credentials {
            key: livekit_cfg.api_key.clone(),
            secret: livekit_cfg.api_secret.clone(),
        };
        let advertise = Advertise::mapped(public.as_ref().map(|(_, ip)| *ip));
        match livekit_bundle::spawn_livekit(advertise, &creds, &data_dir).await {
            Ok(child) => {
                tracing::info!(port = livekit_cfg.port, "bundled livekit-server started");
                Some(child)
            }
            Err(e) => {
                tracing::warn!(error = %e, "livekit subprocess not started — voice will be unavailable unless you set LIVEKIT_URL");
                None
            }
        }
    } else {
        if !want_autospawn {
            tracing::info!("livekit_autospawn off, not spawning bundled livekit");
        } else {
            tracing::info!("livekit_url set, assuming external livekit instance");
        }
        None
    };

    tracing::info!(
        explicit_url = ?livekit_cfg.explicit_url,
        public_host = ?livekit_cfg.public_host,
        port = livekit_cfg.port,
        "livekit configured"
    );

    let operators: HashSet<String> = opts
        .operators
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    if !operators.is_empty() {
        tracing::info!(
            count = operators.len(),
            "operators configured for system guilds"
        );
    }

    let identities = discordia_server::declared_identities(
        &opts.public_hosts.clone().unwrap_or_default().join(","),
        addr.port(),
    );
    if !identities.is_empty() {
        tracing::info!(?identities, "public names clients may dial");
    }

    let mut cfg = discordia_server::ServerConfig {
        livekit: livekit_cfg,
        operators,
        data_dir: data_dir.clone(),
        identities,
        media_max_bytes: opts
            .media_max_bytes
            .unwrap_or(discordia_server::media::DEFAULT_MAX_BYTES),
        admission: discordia_server::admission::Admission {
            trusted_proxies: opts.trusted_proxies.clone().unwrap_or_default(),
            ..Default::default()
        },
    };
    if !cfg.admission.trusted_proxies.is_empty() {
        tracing::info!(proxies = ?cfg.admission.trusted_proxies, "trusting X-Forwarded-For from");
    }
    let admission = cfg.admission.clone();

    // The plaintext socket is for loopback and a TLS proxy; anything reaching
    // this box directly from elsewhere comes in over QUIC with the key below.
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("cannot bind the gateway on {addr}: {e}"))?;
    let bound = listener.local_addr().unwrap_or(addr);
    cfg.identities
        .extend(discordia_server::local_identities(bound.port()));

    let coordination = match opts.relay_url.as_deref().map(str::trim) {
        Some(url) if !url.is_empty() => Coordination::Relay(url.to_string()),
        _ => Coordination::None,
    };
    let quic_secret = discordia_server::quic::persistent_secret(&data_dir).unwrap_or_else(|e| {
        tracing::error!(error = %e, "cannot persist the QUIC key; friends will have to re-add this server after a restart");
        iroh::SecretKey::generate()
    });
    let name_key = Arc::new(HostKey::derived_from(&quic_secret));
    let quic_port = opts
        .quic_port
        .unwrap_or(discordia_server::quic::DEFAULT_PORT);
    let quic_endpoint = match discordia_server::quic::bind_quic(
        Some(quic_secret),
        &coordination,
        quic_port,
    )
    .await
    {
        Ok(ep) => Some(ep),
        Err(e) => {
            tracing::warn!(error = %e, port = quic_port, "QUIC endpoint not bound — only loopback and a TLS proxy can reach this gateway");
            None
        }
    };
    if let Some(ep) = quic_endpoint.as_ref() {
        cfg.identities
            .insert(discordia_server::protocol::quic_origin(
                &ep.id().to_string(),
            ));
    }

    let router = discordia_server::build_router(cfg)
        .await
        .map_err(|e| format!("server failed to start: {e}"))?;

    let _quic = quic_endpoint.and_then(|ep| {
        let key = ep.id().to_string();
        match discordia_server::quic::serve_on_with(ep, router.clone(), coordination.clone()) {
            Ok(handle) => {
                let port = handle.sockets.first().map(|s| s.port()).unwrap_or(0);
                tracing::info!(
                    share = %share_string(&key, opts.public_address.as_deref(), port, &coordination),
                    "friends connect with this address"
                );
                if !publish_to.is_empty() {
                    // A rendezvous admits only literal addresses, checked against the peer it saw.
                    let public_ip = public.as_ref().map(|(_, ip)| ip.to_string());
                    let transport = TransportAdvert {
                        key: key.clone(),
                        addrs: share_addrs(public_ip.as_deref(), port, &coordination),
                    };
                    tracing::info!(rendezvous = ?publish_to, addrs = ?transport.addrs, "publishing");
                    crate::publish::spawn(
                        publish_to.clone(),
                        crate::publish::options(&opts),
                        transport,
                        name_key.clone(),
                    );
                }
                Some(handle)
            }
            Err(e) => {
                tracing::warn!(error = %e, "QUIC gateway not serving; nothing to publish");
                None
            }
        }
    });

    let gateway = discordia_server::serve_router_with(listener, router, &admission);
    tracing::info!(%bound, "plaintext gateway listening (loopback and TLS proxies only)");

    discordia_rendezvous::run::shutdown_signal().await;
    tracing::info!("shutting down");
    gateway.abort();
    Ok(())
}

/// The public address first when there is one; without it, every interface
/// address, which on a box behind NAT the person has to swap for the public one.
pub fn share_string(
    key: &str,
    public_address: Option<&str>,
    port: u16,
    coordination: &Coordination,
) -> String {
    discordia_server::protocol::format_quic_share(
        key,
        &share_addrs(public_address, port, coordination),
    )
}

pub fn share_addrs(
    public_address: Option<&str>,
    port: u16,
    coordination: &Coordination,
) -> Vec<String> {
    let mut addrs: Vec<String> = match public_address {
        Some(name) => vec![with_port(name, port)],
        None => {
            let mut local: Vec<String> = discordia_server::local_identities(port)
                .into_iter()
                .filter(|a| {
                    !a.starts_with("localhost") && !a.starts_with("127.") && !a.starts_with("[::1]")
                })
                .collect();
            local.sort();
            local
        }
    };
    if let Coordination::Relay(url) = coordination {
        addrs.push(url.clone());
    }
    addrs
}

pub fn bare_host(name: &str) -> String {
    match name.parse::<IpAddr>() {
        Ok(IpAddr::V6(v6)) => format!("[{v6}]"),
        _ => name.to_string(),
    }
}

pub fn with_port(name: &str, port: u16) -> String {
    format!("{}:{port}", bare_host(name))
}

/// The SFU advertises an IP, so a DNS name is looked up once at start.
async fn resolve(name: &str) -> Result<IpAddr, String> {
    if let Ok(ip) = name.parse() {
        return Ok(ip);
    }
    let found: Vec<IpAddr> = tokio::net::lookup_host((name, 0))
        .await
        .map_err(|e| format!("public_address {name}: {e}"))?
        .map(|a| a.ip())
        .collect();
    found
        .iter()
        .find(|ip| ip.is_ipv4())
        .or(found.first())
        .copied()
        .ok_or_else(|| format!("public_address {name} has no address"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_public_address_leads_and_the_relay_follows() {
        let share = share_string(
            "k",
            Some("2001:db8::1"),
            9001,
            &Coordination::Relay("http://r.example:7701".into()),
        );
        assert_eq!(share, "quic://k@[2001:db8::1]:9001;http://r.example:7701");
        assert_eq!(
            share_string("k", Some("box.example"), 9001, &Coordination::None),
            "quic://k@box.example:9001"
        );
    }
}
