use std::sync::Arc;

use discordia_server::protocol::LaunchSource;
use discordia_server::push::PushConfig;
use discordia_server::rendezvous::{
    self, HostKey, HostUpdate, PublishOptions, Registration, TransportAdvert,
};
use discordia_server::server_settings::LaunchSettings;

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

/// Whatever the flags, environment or config file set is fixed for this run;
/// Server Settings fills in the rest.
pub fn launch(opts: &ServerOptions, publishes: bool) -> Result<LaunchSettings, String> {
    Ok(LaunchSettings {
        name: opts.publish_name.clone().map(Some),
        description: opts.publish_description.clone().map(Some),
        listed_public: opts.publish_public,
        location: opts.publish_location.map(|l| Some(l.0)),
        media_max_bytes: opts.media_max_bytes,
        source: LaunchSource::Config,
        publishes,
        push: PushConfig::from_setting(opts.push_relay.as_deref())?,
    })
}

pub fn spawn(
    urls: Vec<String>,
    changes: tokio::sync::watch::Receiver<PublishOptions>,
    transport: TransportAdvert,
    key: Arc<HostKey>,
) {
    for url in urls {
        tokio::spawn(publish(Registration {
            url,
            options: changes.borrow().clone(),
            transport: Some(transport.clone()),
            signer: key.clone(),
            changes: Some(changes.clone()),
        }));
    }
}

/// A rendezvous down at boot is retried: `maintain` only takes over once one registration landed.
async fn publish(mut registration: Registration) {
    let url = registration.url.clone();
    let mut attempt = 0;
    let control = loop {
        registration.refresh();
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
    fn only_what_was_set_is_pinned() {
        let opts = ServerOptions {
            publish_name: Some("  ".into()),
            media_max_bytes: Some(1 << 30),
            ..ServerOptions::default()
        };
        let pins = launch(&opts, true).unwrap().pins();
        assert!(pins.name && pins.media_max_bytes);
        assert!(!pins.description && !pins.listed_public && !pins.location);
    }

    #[test]
    fn push_goes_to_the_project_relay_unless_turned_off() {
        let with = |push_relay: Option<&str>| {
            launch(
                &ServerOptions {
                    push_relay: push_relay.map(str::to_string),
                    ..ServerOptions::default()
                },
                false,
            )
            .map(|l| l.push.url().map(str::to_string))
        };
        assert_eq!(
            with(None).unwrap().as_deref(),
            Some(discordia_server::protocol::push::DEFAULT_PUSH_RELAY)
        );
        assert_eq!(with(Some("off")).unwrap(), None);
        assert_eq!(with(Some("")).unwrap(), None);
        assert_eq!(
            with(Some("https://push.example/")).unwrap().as_deref(),
            Some("https://push.example")
        );
        assert!(with(Some("http://push.example")).is_err());
    }
}
