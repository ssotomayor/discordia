use std::net::SocketAddr;
use std::time::Duration;

use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;

const ATTEMPT_DELAY: Duration = Duration::from_millis(250);
const PROBE_TIMEOUT: Duration = Duration::from_secs(1);
const PROBE_BUDGET: Duration = Duration::from_millis(1500);

fn direct_address(raw: &str) -> Option<SocketAddr> {
    let url = url::Url::parse(raw).ok()?;
    if !matches!(url.scheme(), "ws" | "wss") {
        return None;
    }
    let ip = match url.host()? {
        url::Host::Ipv4(ip) => ip.into(),
        url::Host::Ipv6(ip) => ip.into(),
        _ => return None,
    };
    Some(SocketAddr::new(ip, url.port_or_known_default()?))
}

async fn first_reachable<F, Fut>(addresses: &[SocketAddr], mut probe: F) -> Option<usize>
where
    F: FnMut(SocketAddr) -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    let mut attempts = FuturesUnordered::new();
    let mut next = 0;
    let mut launch_at = tokio::time::Instant::now();
    let deadline = launch_at + PROBE_BUDGET;
    loop {
        if next < addresses.len()
            && attempts.len() < 2
            && (attempts.is_empty() || tokio::time::Instant::now() >= launch_at)
        {
            let index = next;
            let future = probe(addresses[index]);
            attempts
                .push(async move { (index, tokio::time::timeout(PROBE_TIMEOUT, future).await) });
            next += 1;
            launch_at = tokio::time::Instant::now() + ATTEMPT_DELAY;
        }
        if attempts.is_empty() {
            return None;
        }
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => return None,
            result = attempts.next() => {
                let (index, result) = result?;
                if matches!(result, Ok(Ok(()))) {
                    return Some(index);
                }
            }
            _ = tokio::time::sleep_until(launch_at), if next < addresses.len() && attempts.len() < 2 => {}
        }
    }
}

fn alternate_families(urls: &mut Vec<String>) {
    let Some(first) = urls.first().and_then(|u| direct_address(u)) else {
        return;
    };
    if !urls.iter().all(|u| direct_address(u).is_some()) {
        return;
    }
    let mut preferred = Vec::new();
    let mut other = Vec::new();
    for url in urls.drain(..) {
        if direct_address(&url).unwrap().is_ipv6() == first.is_ipv6() {
            preferred.push(url);
        } else {
            other.push(url);
        }
    }
    let mut preferred = preferred.into_iter();
    let mut other = other.into_iter();
    loop {
        let a = preferred.next();
        let b = other.next();
        if a.is_none() && b.is_none() {
            break;
        }
        urls.extend(a);
        urls.extend(b);
    }
}

async fn prefer_reachable(urls: &mut Vec<String>) {
    if urls.len() < 2
        || ["HTTP_PROXY", "http_proxy", "HTTPS_PROXY", "https_proxy"]
            .iter()
            .any(|key| std::env::var_os(key).is_some())
    {
        return;
    }
    let Some(addresses) = urls
        .iter()
        .map(|url| direct_address(url))
        .collect::<Option<Vec<_>>>()
    else {
        return;
    };
    // Only unauthenticated TCP probes race: concurrent LiveKit joins can evict
    // one another when they carry the same participant identity.
    let selected = first_reachable(&addresses, |addr| async move {
        tokio::net::TcpStream::connect(addr)
            .await
            .map(|_| ())
            .map_err(|e| {
                tracing::info!(%addr, error = %e, "voice route: signaling probe failed");
                e.to_string()
            })
    })
    .await;
    if let Some(index) = selected {
        let selected = urls.remove(index);
        tracing::info!(url = %selected, "voice route: reachable signaling endpoint preferred; media still unverified");
        urls.insert(0, selected);
    }
}

pub async fn try_endpoints<T, F, Fut>(
    primary: String,
    alternatives: Vec<String>,
    mut connect: F,
) -> Result<(T, String), String>
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
{
    let mut urls = vec![primary];
    for url in alternatives.into_iter().take(7) {
        if !urls.contains(&url) {
            urls.push(url);
        }
    }
    // A loopback alternate is the signaling tunnel: it always answers a TCP
    // probe, so it is kept out of the race and stays the last resort.
    let tunnels: Vec<String> = urls
        .iter()
        .skip(1)
        .filter(|u| direct_address(u).is_some_and(|a| a.ip().is_loopback()))
        .cloned()
        .collect();
    urls.retain(|u| !tunnels.contains(u));
    alternate_families(&mut urls);
    prefer_reachable(&mut urls).await;
    urls.extend(tunnels);
    let mut failures = Vec::new();
    for url in urls {
        eprintln!("[voice] trying endpoint {url}");
        let started = std::time::Instant::now();
        match connect(url.clone()).await {
            Ok(connected) => {
                tracing::info!(%url, elapsed_ms = started.elapsed().as_millis() as u64, "voice route: endpoint connected");
                return Ok((connected, url));
            }
            Err(error) => {
                tracing::warn!(%url, %error, elapsed_ms = started.elapsed().as_millis() as u64, "voice route: endpoint failed");
                failures.push(format!("{url}: {error}"));
            }
        }
    }
    Err(format!(
        "all voice endpoints failed — {}",
        failures.join("; ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_loopback_tunnel_is_tried_last_even_though_it_always_answers() {
        let tunnel = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let tunnel_url = format!("ws://{}/sfu", tunnel.local_addr().unwrap());
        let mut tried = Vec::new();
        let failure = try_endpoints(
            "ws://[2800:810::1]:7880".into(),
            vec!["ws://203.0.113.5:7880".into(), tunnel_url.clone()],
            |url| {
                tried.push(url.clone());
                std::future::ready(Err::<(), _>("refused".into()))
            },
        )
        .await
        .unwrap_err();
        assert_eq!(tried.last(), Some(&tunnel_url));
        assert_eq!(tried.len(), 3);
        assert!(failure.contains("/sfu: refused"));
    }

    #[tokio::test]
    async fn a_slow_family_does_not_delay_the_other_and_losing_probes_are_cancelled() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        struct PendingProbe(Arc<AtomicUsize>);
        impl Drop for PendingProbe {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let cancelled = Arc::new(AtomicUsize::new(0));
        let started = tokio::time::Instant::now();
        let selected = first_reachable(
            &[
                "[::1]:7880".parse().unwrap(),
                "127.0.0.1:7880".parse().unwrap(),
            ],
            |addr| {
                let cancelled = cancelled.clone();
                async move {
                    if addr.is_ipv6() {
                        let _guard = PendingProbe(cancelled);
                        std::future::pending::<Result<(), String>>().await
                    } else {
                        Ok(())
                    }
                }
            },
        )
        .await;
        assert_eq!(selected, Some(1));
        assert!(started.elapsed() < PROBE_TIMEOUT);
        assert_eq!(cancelled.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn an_immediate_network_failure_does_not_wait_for_the_stagger() {
        let started = tokio::time::Instant::now();
        let selected = first_reachable(
            &[
                "[::1]:7880".parse().unwrap(),
                "127.0.0.1:7880".parse().unwrap(),
            ],
            |addr| {
                std::future::ready(if addr.is_ipv6() {
                    Err("network unreachable".into())
                } else {
                    Ok(())
                })
            },
        )
        .await;
        assert_eq!(selected, Some(1));
        assert!(started.elapsed() < ATTEMPT_DELAY);
    }

    #[tokio::test]
    async fn probes_have_a_total_deadline() {
        let started = tokio::time::Instant::now();
        let addresses = vec!["127.0.0.1:7880".parse().unwrap(); 8];
        assert_eq!(
            first_reachable(&addresses, |_| std::future::pending::<Result<(), String>>()).await,
            None
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn tcp_reachability_does_not_hide_a_later_media_failure_or_duplicate_joins() {
        let a = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let b = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let first = format!("ws://{}", a.local_addr().unwrap());
        let second = format!("ws://{}", b.local_addr().unwrap());
        let mut tried = Vec::new();
        let (_, selected) = try_endpoints(first.clone(), vec![second.clone()], |url| {
            tried.push(url.clone());
            std::future::ready(if url == first {
                Err("ICE failed".into())
            } else {
                Ok(())
            })
        })
        .await
        .unwrap();
        assert_eq!(tried, [first, second.clone()]);
        assert_eq!(selected, second);
    }

    #[test]
    fn families_alternate_without_reordering_proxy_or_dns_targets() {
        let mut urls = vec![
            "ws://[::1]:7880".into(),
            "ws://[::2]:7880".into(),
            "ws://127.0.0.1:7880".into(),
        ];
        alternate_families(&mut urls);
        assert_eq!(
            urls,
            ["ws://[::1]:7880", "ws://127.0.0.1:7880", "ws://[::2]:7880"]
        );
        let mut urls = vec!["wss://voice.example".into(), "ws://127.0.0.1:7880".into()];
        let original = urls.clone();
        alternate_families(&mut urls);
        assert_eq!(urls, original);
    }

    #[tokio::test]
    async fn a_failed_ipv4_route_tries_ipv6_once_and_stops_on_success() {
        let mut tried = Vec::new();
        let (_, chosen) = try_endpoints(
            "ws://v4:7880".into(),
            vec![
                "ws://v4:7880".into(),
                "ws://[2800:810::1]:7880".into(),
                "ws://unused:7880".into(),
            ],
            |url| {
                tried.push(url.clone());
                std::future::ready(if url.contains('[') {
                    Ok(())
                } else {
                    Err("ICE failed".into())
                })
            },
        )
        .await
        .unwrap();
        assert_eq!(tried, ["ws://v4:7880", "ws://[2800:810::1]:7880"]);
        assert_eq!(chosen, "ws://[2800:810::1]:7880");
    }

    #[tokio::test]
    async fn exhausted_routes_report_each_failure() {
        let failure = try_endpoints("ws://v4:7880".into(), vec!["ws://v6:7880".into()], |url| {
            std::future::ready(Err::<(), _>(format!("{url} unreachable")))
        })
        .await
        .err()
        .unwrap();
        assert!(failure.contains("v4:7880 unreachable"));
        assert!(failure.contains("v6:7880 unreachable"));
    }
}
