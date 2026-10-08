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
    let mut failures = Vec::new();
    for url in urls {
        eprintln!("[voice] trying endpoint {url}");
        match connect(url.clone()).await {
            Ok(connected) => return Ok((connected, url)),
            Err(error) => failures.push(format!("{url}: {error}")),
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
