use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use igd_next::PortMappingProtocol;

mod pcp;

const LEASE: Duration = Duration::from_secs(3600);

const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(3);

const HAIRPIN_TIMEOUT: Duration = Duration::from_secs(3);

/// No TCP gateway port: off this machine the gateway speaks QUIC only, so the
/// only thing to open for chat is the UDP port the QUIC endpoint bound.
#[derive(Debug, Clone, Copy)]
pub struct Ports {
    pub media_tcp: u16,
    pub media_tcp_ice: u16,
    pub media_udp: u16,
    pub quic_udp: u16,
}

#[derive(Debug, Clone)]
pub struct Mapped {
    pub method: &'static str,
    pub public_ip: IpAddr,
    pub media: MediaMapping,
    pub quic: bool,
    pub quic_ip: IpAddr,
    pub quic_port: u16,
    pub hairpin: bool,
    pub media_note: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct MediaMapping {
    pub signaling: bool,
    pub udp: bool,
    pub tcp: bool,
}

impl MediaMapping {
    pub fn available(self) -> bool {
        self.udp || self.tcp
    }

    fn note(self) -> Option<&'static str> {
        match (self.udp, self.tcp) {
            (true, true) => Some("UDP and ICE/TCP mapped; callers verify media connectivity"),
            (true, false) => {
                Some("UDP mapped; ICE/TCP unavailable; callers verify media connectivity")
            }
            (false, true) => {
                Some("ICE/TCP mapped; UDP unavailable; callers verify media connectivity")
            }
            _ => None,
        }
    }
}

pub struct MappingGuard {
    shutdown: Vec<tokio::sync::oneshot::Sender<()>>,
}

const METHODS: [&str; 3] = ["UPnP-IGD", "NAT-PMP", "PCP"];

/// Each method's router and public address, asked of all three at once: a
/// router that ignores a method costs a timeout, and in turn they added up.
pub struct Discovery {
    local_ip: Ipv4Addr,
    media_tcp: u16,
    found: Vec<(&'static str, Result<Found, String>)>,
}

pub async fn discover(local_ip: Ipv4Addr, media_tcp: u16) -> Discovery {
    let found =
        futures_util::future::join_all(METHODS.map(|method| async move {
            (method, discover_one(method, local_ip, media_tcp).await)
        }))
        .await;
    Discovery {
        local_ip,
        media_tcp,
        found,
    }
}

pub async fn request(discovery: Discovery, ports: Ports) -> Result<(Mapped, MappingGuard), String> {
    let Discovery {
        local_ip,
        media_tcp,
        found,
    } = discovery;
    request_with(
        found,
        |method| discover_one(method, local_ip, media_tcp),
        |found| finish(found, local_ip, ports),
    )
    .await
}

/// PCP learns the public address by mapping the voice signaling port, so a
/// router found but never finished gives that port back.
struct Found {
    router: Option<Router>,
    public_ip: IpAddr,
    media_tcp: u16,
}

impl Drop for Found {
    fn drop(&mut self) {
        if let Some(Router::Pcp(p)) = self.router.take()
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            let port = self.media_tcp;
            runtime.spawn(async move {
                let _ = p.map(6, port, 0).await;
            });
        }
    }
}

async fn discover_one(
    method: &'static str,
    local_ip: Ipv4Addr,
    media_tcp: u16,
) -> Result<Found, String> {
    let mut router = match method {
        "UPnP-IGD" => igd_router(local_ip).await,
        "NAT-PMP" => natpmp_router().await,
        _ => pcp_router(local_ip).await,
    }?;
    let public_ip = router.public_ip(media_tcp).await?;
    let found = Found {
        router: Some(router),
        public_ip,
        media_tcp,
    };
    if is_private(public_ip) {
        return Err(format!(
            "your router's own address ({public_ip}) is private, so it is behind \
             another NAT — usually carrier-grade NAT at your ISP. Nothing this \
             machine can do opens a path in; you need the relay, or a provider \
             that gives you a public address."
        ));
    }
    Ok(found)
}

/// Mapping stays one method at a time, in order: two methods mapping the same
/// ports at once could race each other, and two hairpin probes cannot share one.
async fn request_with<D, Disc, DFut, Fin, FFut>(
    first: Vec<(&'static str, Result<D, String>)>,
    mut discover: Disc,
    mut finish: Fin,
) -> Result<(Mapped, MappingGuard), String>
where
    Disc: FnMut(&'static str) -> DFut,
    DFut: std::future::Future<Output = Result<D, String>>,
    Fin: FnMut(D) -> FFut,
    FFut: std::future::Future<Output = Result<(Mapped, MappingGuard), String>>,
{
    let mut best: Option<Mapped> = None;
    let mut chat: Option<SocketAddr> = None;
    let mut guards = MappingGuard {
        shutdown: Vec::new(),
    };
    let mut errors = Vec::new();
    let mut round = first;
    for attempt in 0..2 {
        let mut answered = Vec::new();
        for (method, discovered) in round {
            let result = match discovered {
                Ok(found) => {
                    answered.push(method);
                    finish(found).await
                }
                Err(error) => Err(error),
            };
            match result {
                Ok((mut mapped, mut guard)) => {
                    guards.shutdown.append(&mut guard.shutdown);
                    if let Some(old) = best
                        .as_ref()
                        .filter(|old| old.public_ip == mapped.public_ip)
                    {
                        mapped.media.signaling |= old.media.signaling;
                        mapped.media.udp |= old.media.udp;
                        mapped.media.tcp |= old.media.tcp;
                        mapped.hairpin |= old.hairpin;
                        mapped.media_note = mapped.media.note().or(mapped.media_note);
                    }
                    if mapped.quic {
                        chat.get_or_insert(SocketAddr::new(mapped.quic_ip, mapped.quic_port));
                    }
                    let usable =
                        mapped.media.udp && mapped.media.tcp && (mapped.quic || chat.is_some());
                    if best
                        .as_ref()
                        .is_none_or(|old| mapping_rank(&mapped) > mapping_rank(old))
                    {
                        best = Some(mapped);
                    }
                    if usable {
                        return Ok((with_chat_mapping(best.unwrap(), chat), guards));
                    }
                }
                Err(error) => {
                    tracing::info!(method, attempt, %error, "port mapping attempt failed");
                    errors.push(format!("{method}: {error}"));
                }
            }
        }
        // A method no router answered is not asked twice: a second silence
        // only doubled the wait before the host started.
        if attempt == 1 || answered.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        let retries: Vec<_> = answered
            .into_iter()
            .map(|method| {
                let again = discover(method);
                async move { (method, again.await) }
            })
            .collect();
        round = futures_util::future::join_all(retries).await;
    }
    match best {
        Some(mapped) => Ok((with_chat_mapping(mapped, chat), guards)),
        None => Err(format!(
            "automatic port mapping failed — {}. \
            Try IPv6 or a manually forwarded public address; a timeout alone does not prove carrier-grade NAT.",
            errors.join("; ")
        )),
    }
}

fn with_chat_mapping(mut mapped: Mapped, chat: Option<SocketAddr>) -> Mapped {
    if let Some(chat) = chat {
        mapped.quic = true;
        mapped.quic_ip = chat.ip();
        mapped.quic_port = chat.port();
    }
    mapped
}

fn mapping_rank(mapped: &Mapped) -> (bool, bool, bool, bool) {
    (
        mapped.media.udp,
        mapped.media.tcp,
        mapped.media.signaling,
        mapped.quic,
    )
}

pub async fn manual(
    public_ip: IpAddr,
    local_ip: Ipv4Addr,
    media_tcp: u16,
    quic_port: u16,
) -> Mapped {
    let hairpin = probe_hairpin(
        SocketAddr::new(local_ip.into(), media_tcp),
        SocketAddr::new(public_ip, media_tcp),
        HAIRPIN_TIMEOUT,
    )
    .await;
    Mapped {
        method: "manual forwarding",
        public_ip,
        media: MediaMapping {
            signaling: true,
            udp: true,
            tcp: true,
        },
        quic: true,
        quic_ip: public_ip,
        quic_port,
        hairpin,
        media_note: Some("manually configured ports; callers verify media connectivity"),
    }
}

async fn finish(
    mut found: Found,
    local_ip: Ipv4Addr,
    ports: Ports,
) -> Result<(Mapped, MappingGuard), String> {
    let public_ip = found.public_ip;
    let Some(router) = found.router.take() else {
        return Err("router already used".into());
    };

    let mut owned = Vec::new();
    if matches!(&router, Router::Pcp(_)) {
        owned.push((PortMappingProtocol::TCP, ports.media_tcp, ports.media_tcp));
    }
    let (quic_ok, quic_port) = match router
        .add(PortMappingProtocol::UDP, local_ip, ports.quic_udp)
        .await
    {
        Ok(external) => {
            owned.push((PortMappingProtocol::UDP, ports.quic_udp, external));
            (true, external)
        }
        Err(e) => {
            tracing::warn!(error = %e, "QUIC port not mapped; still trying voice ports");
            (false, ports.quic_udp)
        }
    };

    let mut media_note = None;
    let mut media = MediaMapping::default();
    {
        for (proto, port, available) in [
            (
                PortMappingProtocol::TCP,
                ports.media_tcp,
                &mut media.signaling,
            ),
            (
                PortMappingProtocol::TCP,
                ports.media_tcp_ice,
                &mut media.tcp,
            ),
            (PortMappingProtocol::UDP, ports.media_udp, &mut media.udp),
        ] {
            match router.add(proto, local_ip, port).await {
                Ok(external) if external == port => {
                    *available = true;
                    if !owned.contains(&(proto, port, external)) {
                        owned.push((proto, port, external));
                    }
                }
                Ok(external) => {
                    owned.push((proto, port, external));
                    tracing::warn!(
                        %proto, wanted = port, granted = external,
                        "router renumbered a media port — voice cannot use it"
                    );
                    media_note.get_or_insert(
                        "the router renumbered a voice port, so there is no known port to hand friends",
                    );
                }
                Err(e) => {
                    tracing::warn!(%proto, port, error = %e, "media port not mapped");
                    media_note.get_or_insert("the router refused to forward a voice port");
                }
            }
        }
    }

    let hairpin = media.signaling
        && probe_hairpin(
            SocketAddr::new(IpAddr::V4(local_ip), ports.media_tcp),
            SocketAddr::new(public_ip, ports.media_tcp),
            HAIRPIN_TIMEOUT,
        )
        .await;
    media_note = media.note().or(media_note);

    let mapped = Mapped {
        method: router.method(),
        public_ip,
        media,
        quic: quic_ok,
        quic_ip: public_ip,
        quic_port,
        hairpin,
        media_note,
    };
    Ok((mapped, keep_alive(router, local_ip, owned)))
}

fn keep_alive(
    router: Router,
    local_ip: Ipv4Addr,
    all: Vec<(PortMappingProtocol, u16, u16)>,
) -> MappingGuard {
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        tokio::pin!(rx);
        loop {
            tokio::select! {
                _ = tokio::time::sleep(router.renew_after().await) => {
                    for &(proto, internal, external) in &all {
                        match router.add(proto, local_ip, internal).await {
                            Ok(granted) if granted != external => tracing::warn!(
                                %proto, internal, was = external, now = granted,
                                "renewal moved the mapping — the advertised endpoint is stale"
                            ),
                            Ok(_) => {}
                            Err(e) => tracing::warn!(
                                %proto, internal, error = %e, "port mapping renewal failed"
                            ),
                        }
                    }
                }
                _ = &mut rx => break,
            }
        }
        for (proto, internal, external) in all {
            let _ = router.remove(proto, internal, external).await;
        }
    });
    MappingGuard { shutdown: vec![tx] }
}

async fn probe_hairpin(local: SocketAddr, public: SocketAddr, timeout: Duration) -> bool {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let probe = async {
        // LiveKit starts after this probe chooses an SFU, so its port needs a temporary responder.
        let listener = tokio::net::TcpListener::bind(local).await?;
        let challenge = rand::random::<[u8; 16]>();
        let outgoing = async {
            let mut stream = tokio::net::TcpStream::connect(public).await?;
            stream.write_all(&challenge).await?;
            let mut response = [0; 16];
            stream.read_exact(&mut response).await?;
            // Closing from the client keeps the listener port reusable immediately on Windows.
            stream.shutdown().await?;
            Ok::<_, std::io::Error>(response == challenge)
        };
        let incoming = async {
            let (mut stream, _) = listener.accept().await?;
            let mut request = [0; 16];
            stream.read_exact(&mut request).await?;
            if request != challenge {
                return Ok(false);
            }
            stream.write_all(&challenge).await?;
            let mut extra = [0; 1];
            Ok::<_, std::io::Error>(stream.read(&mut extra).await? == 0)
        };
        let (outgoing, incoming) = tokio::try_join!(outgoing, incoming)?;
        Ok::<_, std::io::Error>(outgoing && incoming)
    };
    match tokio::time::timeout(timeout, probe).await {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => {
            tracing::warn!(%local, %public, %error, "media hairpin probe failed");
            false
        }
        Err(_) => {
            tracing::warn!(%local, %public, "media hairpin probe timed out");
            false
        }
    }
}

fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_unspecified()
                || v4.is_multicast()
                || v4.is_broadcast()
                || v4.is_loopback()
                || v4.is_link_local()
                || (v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1]))
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (v6.segments()[0] & 0xe000) != 0x2000
        }
    }
}

enum Router {
    Igd(Box<igd_next::aio::Gateway<igd_next::aio::tokio::Tokio>>),
    NatPmp(natpmp::NatpmpAsync<tokio::net::UdpSocket>),
    Pcp(pcp::Client),
}

async fn pcp_router(local_ip: Ipv4Addr) -> Result<Router, String> {
    let gateway = natpmp::get_default_gateway().map_err(|e| e.to_string())?;
    pcp::Client::new(local_ip, SocketAddr::new(gateway.into(), 5351))
        .await
        .map(Router::Pcp)
}

async fn igd_router(local_ip: Ipv4Addr) -> Result<Router, String> {
    let options = igd_next::SearchOptions {
        bind_addr: SocketAddr::new(IpAddr::V4(local_ip), 0),
        timeout: Some(DISCOVERY_TIMEOUT),
        ..Default::default()
    };
    igd_next::aio::tokio::search_gateway(options)
        .await
        .map(|g| Router::Igd(Box::new(g)))
        .map_err(|e| e.to_string())
}

async fn natpmp_router() -> Result<Router, String> {
    natpmp::new_tokio_natpmp()
        .await
        .map(Router::NatPmp)
        .map_err(|e| e.to_string())
}

impl Router {
    async fn renew_after(&self) -> Duration {
        match self {
            Router::Pcp(p) => p.renew_after().await,
            _ => LEASE / 2,
        }
    }
    fn method(&self) -> &'static str {
        match self {
            Router::Igd(_) => "UPnP-IGD",
            Router::NatPmp(_) => "NAT-PMP",
            Router::Pcp(_) => "PCP",
        }
    }

    async fn public_ip(&mut self, media_tcp: u16) -> Result<IpAddr, String> {
        match self {
            Router::Igd(g) => tokio::time::timeout(DISCOVERY_TIMEOUT, g.get_external_ip())
                .await
                .map_err(|_| "UPnP public-address request timed out".to_string())?
                .map_err(|e| e.to_string()),
            Router::NatPmp(n) => {
                n.send_public_address_request()
                    .await
                    .map_err(|e| e.to_string())?;
                match natpmp_response(n).await? {
                    natpmp::Response::Gateway(g) => Ok(IpAddr::V4(*g.public_address())),
                    other => Err(format!("unexpected NAT-PMP reply: {other:?}")),
                }
            }
            Router::Pcp(p) => {
                let endpoint = p.map(6, media_tcp, LEASE.as_secs() as u32).await?;
                if endpoint.port() != media_tcp || is_private(endpoint.ip()) {
                    let _ = p.map(6, media_tcp, 0).await;
                    return Err("PCP did not grant the public voice signaling port".into());
                }
                Ok(endpoint.ip())
            }
        }
    }

    async fn add(
        &self,
        protocol: PortMappingProtocol,
        local_ip: Ipv4Addr,
        port: u16,
    ) -> Result<u16, String> {
        match self {
            Router::Igd(g) => tokio::time::timeout(
                DISCOVERY_TIMEOUT,
                g.add_port(
                    protocol,
                    port,
                    SocketAddr::new(IpAddr::V4(local_ip), port),
                    LEASE.as_secs() as u32,
                    "Discordia",
                ),
            )
            .await
            .map_err(|_| "UPnP port-mapping request timed out".to_string())?
            .map(|()| port)
            .map_err(|e| e.to_string()),
            Router::NatPmp(n) => {
                n.send_port_mapping_request(
                    natpmp_proto(protocol),
                    port,
                    port,
                    LEASE.as_secs() as u32,
                )
                .await
                .map_err(|e| e.to_string())?;
                match natpmp_response(n).await? {
                    natpmp::Response::TCP(m) | natpmp::Response::UDP(m) => Ok(m.public_port()),
                    other => Err(format!("unexpected NAT-PMP reply: {other:?}")),
                }
            }
            Router::Pcp(p) => p
                .map(pcp_proto(protocol), port, LEASE.as_secs() as u32)
                .await
                .map(|a| a.port()),
        }
    }

    async fn remove(
        &self,
        protocol: PortMappingProtocol,
        internal_port: u16,
        external_port: u16,
    ) -> Result<(), String> {
        match self {
            Router::Igd(g) => {
                tokio::time::timeout(DISCOVERY_TIMEOUT, g.remove_port(protocol, external_port))
                    .await
                    .map_err(|_| "UPnP port-removal request timed out".to_string())?
                    .map_err(|e| e.to_string())
            }
            Router::NatPmp(n) => {
                n.send_port_mapping_request(natpmp_proto(protocol), internal_port, 0, 0)
                    .await
                    .map_err(|e| e.to_string())?;
                natpmp_response(n).await.map(|_| ())
            }
            Router::Pcp(p) => p
                .map(pcp_proto(protocol), internal_port, 0)
                .await
                .map(|_| ()),
        }
    }
}

fn pcp_proto(protocol: PortMappingProtocol) -> u8 {
    match protocol {
        PortMappingProtocol::TCP => 6,
        PortMappingProtocol::UDP => 17,
    }
}

fn natpmp_proto(protocol: PortMappingProtocol) -> natpmp::Protocol {
    match protocol {
        PortMappingProtocol::TCP => natpmp::Protocol::TCP,
        PortMappingProtocol::UDP => natpmp::Protocol::UDP,
    }
}

async fn natpmp_response(
    n: &natpmp::NatpmpAsync<tokio::net::UdpSocket>,
) -> Result<natpmp::Response, String> {
    match tokio::time::timeout(DISCOVERY_TIMEOUT, n.read_response_or_retry()).await {
        Ok(r) => r.map_err(|e| e.to_string()),
        Err(_) => Err("no reply from the router".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapped(method: &'static str, media: bool) -> (Mapped, MappingGuard) {
        (
            Mapped {
                method,
                public_ip: "203.0.113.5".parse().unwrap(),
                media: MediaMapping {
                    signaling: media,
                    udp: media,
                    tcp: media,
                },
                quic: true,
                quic_port: 19001,
                quic_ip: "203.0.113.5".parse().unwrap(),
                hairpin: media,
                media_note: None,
            },
            MappingGuard {
                shutdown: Vec::new(),
            },
        )
    }

    async fn attempts<F>(mut attempt: F) -> Result<(Mapped, MappingGuard), String>
    where
        F: FnMut(&'static str) -> Result<(Mapped, MappingGuard), String>,
    {
        request_with(
            METHODS.map(|method| (method, Ok(method))).into(),
            |method| std::future::ready(Ok(method)),
            |method| std::future::ready(attempt(method)),
        )
        .await
    }

    #[tokio::test]
    async fn a_method_no_router_answered_is_not_asked_again() {
        let mut asked = Vec::new();
        let result = request_with(
            METHODS
                .map(|method| (method, Err::<&str, _>(format!("{method} timed out"))))
                .into(),
            |method| {
                asked.push(method);
                std::future::ready(Ok(method))
            },
            |_| std::future::ready(Err("unreachable".to_string())),
        )
        .await;
        assert!(asked.is_empty());
        let error = result.err().unwrap();
        for method in METHODS {
            assert!(error.contains(&format!("{method} timed out")));
        }
    }

    #[tokio::test]
    async fn only_the_methods_that_answered_are_retried() {
        let mut asked = Vec::new();
        let result = request_with(
            vec![
                ("UPnP-IGD", Err("silent".to_string())),
                ("NAT-PMP", Ok("NAT-PMP")),
                ("PCP", Err("silent".to_string())),
            ],
            |method| {
                asked.push(method);
                std::future::ready(Ok(method))
            },
            |method| std::future::ready(Err(format!("{method} refused"))),
        )
        .await;
        assert_eq!(asked, ["NAT-PMP"]);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn a_discovered_router_with_failed_media_does_not_skip_the_other_methods() {
        let mut tried = Vec::new();
        let (result, _) = attempts(|method| {
            tried.push(method);
            Ok(mapped(method, method == "NAT-PMP"))
        })
        .await
        .unwrap();
        assert_eq!(tried, ["UPnP-IGD", "NAT-PMP"]);
        assert_eq!(result.method, "NAT-PMP");
    }

    #[tokio::test]
    async fn mapping_failures_are_retried_and_reported_by_method() {
        let mut tried = Vec::new();
        let result = attempts(|method| {
            tried.push(method);
            Err(format!("{method} refused"))
        })
        .await;
        assert_eq!(
            tried,
            ["UPnP-IGD", "NAT-PMP", "PCP", "UPnP-IGD", "NAT-PMP", "PCP"]
        );
        let error = result.err().unwrap();
        for method in ["UPnP-IGD", "NAT-PMP", "PCP"] {
            assert!(error.contains(method));
        }
        assert!(error.contains("does not prove carrier-grade NAT"));
    }

    #[tokio::test]
    async fn later_voice_success_keeps_the_earlier_chat_address_and_granted_port() {
        for voice_works in [false, true] {
            let mut count = 0;
            let (result, _) = attempts(|method| {
                count += 1;
                match count {
                    1 => Ok(mapped(method, false)),
                    2 => {
                        let (mut voice, guard) = mapped(method, voice_works);
                        voice.media = MediaMapping {
                            signaling: true,
                            udp: true,
                            tcp: true,
                        };
                        voice.quic = false;
                        voice.public_ip = "198.51.100.9".parse().unwrap();
                        voice.quic_ip = voice.public_ip;
                        voice.quic_port = 9001;
                        Ok((voice, guard))
                    }
                    _ => Err("timeout".into()),
                }
            })
            .await
            .unwrap();
            assert_eq!(result.method, "NAT-PMP");
            assert!(result.media.available());
            assert_eq!(result.hairpin, voice_works);
            assert!(result.quic, "voice success must not discard direct chat");
            assert_eq!(result.quic_ip, "203.0.113.5".parse::<IpAddr>().unwrap());
            assert_eq!(result.quic_port, 19001);
            assert_eq!(result.public_ip, "198.51.100.9".parse::<IpAddr>().unwrap());
        }
    }

    #[tokio::test]
    async fn later_chat_success_is_retained_when_an_earlier_mapping_has_better_media() {
        let mut count = 0;
        let (result, _) = attempts(|method| {
            count += 1;
            match count {
                1 => {
                    let (mut voice, guard) = mapped(method, false);
                    voice.media = MediaMapping {
                        signaling: true,
                        udp: true,
                        tcp: true,
                    };
                    voice.quic = false;
                    Ok((voice, guard))
                }
                2 => {
                    let (mut chat, guard) = mapped(method, false);
                    chat.public_ip = "198.51.100.9".parse().unwrap();
                    chat.quic_ip = chat.public_ip;
                    chat.quic_port = 29001;
                    Ok((chat, guard))
                }
                _ => Err("timeout".into()),
            }
        })
        .await
        .unwrap();
        assert_eq!(result.method, "UPnP-IGD");
        assert!(result.media.available() && !result.hairpin);
        assert!(result.quic);
        assert_eq!(result.quic_ip, "198.51.100.9".parse::<IpAddr>().unwrap());
        assert_eq!(result.quic_port, 29001);
        assert_eq!(result.public_ip, "203.0.113.5".parse::<IpAddr>().unwrap());
    }

    #[tokio::test]
    async fn a_later_timeout_does_not_discard_the_successful_chat_mapping() {
        let mut count = 0;
        let (result, _) = attempts(|method| {
            count += 1;
            if count == 1 {
                Ok(mapped(method, false))
            } else {
                Err("timeout".into())
            }
        })
        .await
        .unwrap();
        assert_eq!(result.method, "UPnP-IGD");
        assert_eq!(result.quic_port, 19001);
        assert!(!result.media.available());
    }

    async fn unused_local_address() -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        listener.local_addr().unwrap()
    }

    #[tokio::test]
    async fn hairpin_probe_responds_before_livekit_and_releases_its_port() {
        let local = unused_local_address().await;
        for _ in 0..3 {
            assert!(probe_hairpin(local, local, Duration::from_secs(1)).await);
            let listener = tokio::net::TcpListener::bind(local).await.unwrap();
            drop(listener);
        }
    }

    #[tokio::test]
    async fn an_existing_service_is_not_mistaken_for_a_successful_probe() {
        let occupied = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = occupied.local_addr().unwrap();
        assert!(!probe_hairpin(address, address, Duration::from_secs(1)).await);
    }

    #[tokio::test]
    async fn a_wrong_public_endpoint_times_out_and_releases_the_probe_listener() {
        let local = unused_local_address().await;
        let other = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            probe_hairpin(
                local,
                other.local_addr().unwrap(),
                Duration::from_millis(50),
            ),
        )
        .await
        .unwrap();
        assert!(!result);
        let _listener = tokio::net::TcpListener::bind(local).await.unwrap();
    }

    #[tokio::test]
    async fn an_unreachable_public_endpoint_releases_the_probe_listener() {
        let local = unused_local_address().await;
        let public = unused_local_address().await;
        assert!(!probe_hairpin(local, public, Duration::from_secs(1)).await);
        let _listener = tokio::net::TcpListener::bind(local).await.unwrap();
    }

    #[test]
    fn private_addresses_are_not_public() {
        for ip in [
            "192.168.1.1",
            "10.0.0.1",
            "172.16.0.1",
            "100.64.0.1",
            "100.127.255.255",
            "127.0.0.1",
            "169.254.1.1",
        ] {
            assert!(is_private(ip.parse().unwrap()), "{ip} should be private");
        }
        for ip in ["203.0.113.5", "8.8.8.8", "100.128.0.1", "99.255.255.255"] {
            assert!(!is_private(ip.parse().unwrap()), "{ip} should be public");
        }
    }
}
