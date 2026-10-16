use std::sync::Arc;

use discordia_server::rendezvous::{
    self, HostKey, HostUpdate, PublishOptions, Registration, TransportAdvert,
};

use crate::options::ServerOptions;

/// Refused at start rather than retried forever: no rendezvous answers on another scheme.
pub fn targets(urls: &[String]) -> Result<Vec<String>, String> {
    let urls: Vec<String> = urls
        .iter()
        .map(|u| u.trim().trim_end_matches('/').to_string())
        .filter(|u| !u.is_empty())
        .collect();
    match urls
        .iter()
        .find(|u| !u.starts_with("ws://") && !u.starts_with("wss://"))
    {
        Some(bad) => Err(format!(
            "publish_to {bad}: a rendezvous URL starts with ws:// or wss://"
        )),
        None => Ok(urls),
    }
}

pub fn options(opts: &ServerOptions) -> PublishOptions {
    let line = |s: &Option<String>| s.clone().filter(|s| !s.trim().is_empty());
    PublishOptions {
        publish_name: line(&opts.publish_name),
        description: line(&opts.publish_description),
        publish_public: opts.publish_public.unwrap_or(false),
        location: opts.publish_location.map(|l| l.0),
    }
}

pub fn spawn(
    urls: Vec<String>,
    options: PublishOptions,
    transport: TransportAdvert,
    key: Arc<HostKey>,
) {
    for url in urls {
        tokio::spawn(publish(Registration {
            url,
            options: options.clone(),
            transport: Some(transport.clone()),
            signer: key.clone(),
        }));
    }
}

/// A rendezvous down at boot is retried: `maintain` only takes over once one registration landed.
async fn publish(registration: Registration) {
    let url = registration.url.clone();
    let mut attempt = 0;
    let control = loop {
        let registered = tokio::time::timeout(
            rendezvous::REGISTER_TIMEOUT,
            rendezvous::register(
                &registration.url,
                &registration.options,
                registration.transport.as_ref(),
                registration.signer.as_ref(),
            ),
        )
        .await
        .unwrap_or_else(|_| Err("no answer from the rendezvous".to_string()));
        match registered {
            Ok((info, control)) => {
                tracing::info!(rendezvous = %url, code = %info.shortcode, "published: friends join with this code");
                break control;
            }
            Err(error) => {
                tracing::warn!(rendezvous = %url, %error, attempt, "publishing failed; trying again");
                tokio::time::sleep(rendezvous::backoff(attempt)).await;
                attempt += 1;
            }
        }
    };
    let (updates, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let _maintained = rendezvous::maintain(control, registration, None, None, updates);
    while let Some(update) = rx.recv().await {
        match update {
            HostUpdate::Restored { shortcode } => {
                tracing::info!(rendezvous = %url, code = %shortcode, "published again")
            }
            HostUpdate::Lost { error } => {
                tracing::warn!(rendezvous = %url, %error, "rendezvous unreachable; retrying")
            }
            HostUpdate::Superseded => tracing::warn!(
                rendezvous = %url,
                "another server with this quic-secret took the name; this one stops publishing there"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_websocket_urls_are_rendezvous() {
        let typed = [" ws://r.example:7700/ ".to_string(), String::new()];
        assert_eq!(targets(&typed).unwrap(), ["ws://r.example:7700"]);
        assert!(targets(&["http://r.example:7700".into()]).is_err());
        assert!(targets(&[]).unwrap().is_empty());
    }

    #[test]
    fn a_blank_name_asks_for_a_random_code() {
        let opts = ServerOptions {
            publish_name: Some("  ".into()),
            ..ServerOptions::default()
        };
        assert_eq!(options(&opts).publish_name, None);
        assert!(!options(&opts).publish_public);
    }
}
